//! Credential revocation remains available and atomically invalidates management authority.
use super::{PersistenceError, Store};
use firecrab_api_types::DeployToken;
use firecrab_stack::{
    ManagedResource, Manager, NativeAction, NativeOperation, NativeOperationState,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

fn identity(service: Uuid, token: Uuid) -> ManagedResource {
    ManagedResource {
        kind: "service_deploy_token".into(),
        id: format!("{service}:{token}"),
    }
}

pub(super) fn validate(
    conn: &Connection,
    resource: &ManagedResource,
    adopting: bool,
) -> Result<(), PersistenceError> {
    let (service, token) = resource
        .id
        .split_once(':')
        .ok_or(PersistenceError::InvalidIacOwner)?;
    let row: Option<(String, i64)> = conn.query_row(
        "SELECT metadata,length(secret_hash) FROM service_deploy_tokens WHERE id=?1 AND service_id=?2",
        params![token,service], |row| Ok((row.get(0)?,row.get(1)?)),
    ).optional()?;
    if !adopting {
        return Ok(());
    }
    let (raw, length) = row.ok_or(PersistenceError::InvalidStackResource)?;
    let corrupt = || PersistenceError::CorruptRecord {
        id: "service_deploy_token".into(),
        reason: "invalid credential identity".into(),
    };
    let metadata: DeployToken = serde_json::from_str(&raw).map_err(|_| corrupt())?;
    if metadata.id.to_string() != token
        || metadata.service_id.to_string() != service
        || length != 32
    {
        return Err(corrupt());
    }
    let independent: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM micro_services WHERE id=?1) AND NOT EXISTS(SELECT 1 FROM cluster_service_workers WHERE id=?1)", [service], |row|row.get(0))?;
    if !independent {
        return Err(PersistenceError::InvalidStackResource);
    }
    Ok(())
}

impl Store {
    /// Operator-only revocation. It does not cancel work already admitted with this credential.
    pub(crate) fn revoke_deploy_token_with_receipt(
        &self,
        service: Uuid,
        token: Uuid,
        id: Uuid,
    ) -> Result<NativeOperation, PersistenceError> {
        if service.is_nil() || token.is_nil() || id.is_nil() {
            return Err(PersistenceError::InvalidIacOwner);
        }
        let resource = identity(service, token);
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(operation) = super::iac_native::read(&tx, id)? {
            if operation.action != NativeAction::RevokeDeployToken
                || operation.ownership.resource != resource
                || operation.state != NativeOperationState::Succeeded
            {
                return Err(PersistenceError::IacOperationIdentity);
            }
            tx.commit()?;
            return Ok(operation);
        }
        let ownership = super::iac_owners::read(&tx, &resource)?;
        let generation = ownership
            .generation
            .checked_add(1)
            .and_then(|value| i64::try_from(value).ok())
            .ok_or(PersistenceError::StaleIacOwner)?;
        tx.execute(
            "DELETE FROM service_deploy_tokens WHERE id=?1 AND service_id=?2",
            params![token.to_string(), service.to_string()],
        )?;
        if let Some(owner) = ownership
            .owner
            .as_ref()
            .filter(|owner| owner.manager == Manager::Stack)
        {
            tx.execute(
                "UPDATE stacks SET revision=revision+1 WHERE id=?1",
                [owner.id.to_string()],
            )?;
        }
        tx.execute("INSERT INTO iac_resource_owners(kind,resource_id,generation) VALUES('service_deploy_token',?1,?2)
            ON CONFLICT(kind,resource_id) DO UPDATE SET generation=excluded.generation,manager=NULL,owner_id=NULL,logical_key=NULL",params![resource.id,generation])?;
        let operation = NativeOperation {
            operation_id: id,
            ownership,
            action: NativeAction::RevokeDeployToken,
            state: NativeOperationState::Succeeded,
            error_code: None,
            execution_generation: 0,
            configuration_generation: None,
        };
        super::iac_native::save(&tx, &operation)?;
        tx.commit()?;
        Ok(operation)
    }
}

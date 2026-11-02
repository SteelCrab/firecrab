//! Policy replacement follows management authority; explicit operator revocation wins.

use super::{PersistenceError, Store};
use firecrab_api_types::SandboxAccessPolicy;
use firecrab_stack::{
    ManagedResource, Manager, NativeAction, NativeOperation, NativeOperationState,
    ResourceOwnership,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

fn resource(principal: Uuid) -> ManagedResource {
    ManagedResource {
        kind: "sandbox_access".into(),
        id: principal.to_string(),
    }
}

fn corrupt() -> PersistenceError {
    PersistenceError::CorruptRecord {
        id: "sandbox_access".into(),
        reason: "invalid access policy".into(),
    }
}

pub(super) fn validate(
    conn: &Connection,
    resource: &ManagedResource,
    adopting: bool,
) -> Result<(), PersistenceError> {
    if adopting {
        let raw: Option<String> = conn
            .query_row(
                "SELECT spec FROM ai_sandbox_access WHERE principal=?1",
                [&resource.id],
                |row| row.get(0),
            )
            .optional()?;
        serde_json::from_str::<SandboxAccessPolicy>(
            &raw.ok_or(PersistenceError::InvalidStackResource)?,
        )
        .map_err(|_| corrupt())?;
    }
    Ok(())
}

impl Store {
    /// A missing policy denies access. Revoked rows remain explicit deny policies.
    pub fn sandbox_access(&self, principal: Uuid) -> Result<SandboxAccessPolicy, PersistenceError> {
        self.lock()
            .query_row(
                "SELECT spec FROM ai_sandbox_access WHERE principal=?1",
                [principal.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|raw| serde_json::from_str(&raw).map_err(|_| corrupt()))
            .transpose()
            .map(Option::unwrap_or_default)
    }

    pub(crate) fn sandbox_access_authority(
        &self,
        principal: Uuid,
    ) -> Result<ResourceOwnership, PersistenceError> {
        self.iac_resource_owner(&resource(principal))
    }

    /// The API captures authority before dispatch; revocation invalidates late saves.
    pub(crate) fn save_sandbox_access_checked(
        &self,
        principal: Uuid,
        policy: &SandboxAccessPolicy,
        expected: &ResourceOwnership,
    ) -> Result<(), PersistenceError> {
        let resource = resource(principal);
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if super::iac_owners::read(&tx, &resource)? != *expected {
            return Err(PersistenceError::StaleIacOwner);
        }
        if !super::iac_owners::public_write_allowed(&tx, &resource)? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        tx.execute("INSERT INTO ai_sandbox_access(principal,spec) VALUES(?1,?2) ON CONFLICT(principal) DO UPDATE SET spec=excluded.spec",
            params![principal.to_string(),serde_json::to_string(policy).map_err(|_| corrupt())?])?;
        tx.commit()?;
        Ok(())
    }

    #[cfg(test)]
    pub fn save_sandbox_access(
        &self,
        principal: Uuid,
        policy: &SandboxAccessPolicy,
    ) -> Result<(), PersistenceError> {
        self.save_sandbox_access_checked(
            principal,
            policy,
            &self.sandbox_access_authority(principal)?,
        )
    }

    /// Operator-only emergency action. Denial, authority invalidation and acknowledgement commit together.
    pub(crate) fn revoke_sandbox_access(
        &self,
        principal: Uuid,
        id: Uuid,
    ) -> Result<NativeOperation, PersistenceError> {
        if principal.is_nil() || id.is_nil() {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let resource = resource(principal);
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = super::iac_native::read(&tx, id)? {
            if existing.ownership.resource != resource
                || existing.action != NativeAction::RevokeSandboxAccess
                || existing.state != NativeOperationState::Succeeded
            {
                return Err(PersistenceError::IacOperationIdentity);
            }
            tx.commit()?;
            return Ok(existing);
        }
        let ownership = super::iac_owners::read(&tx, &resource)?;
        let generation = ownership
            .generation
            .checked_add(1)
            .and_then(|value| i64::try_from(value).ok())
            .ok_or(PersistenceError::StaleIacOwner)?;
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
        tx.execute("INSERT INTO ai_sandbox_access(principal,spec) VALUES(?1,?2) ON CONFLICT(principal) DO UPDATE SET spec=excluded.spec",
            params![principal.to_string(),serde_json::to_string(&SandboxAccessPolicy::default()).map_err(|_| corrupt())?])?;
        tx.execute("INSERT INTO iac_resource_owners(kind,resource_id,generation) VALUES('sandbox_access',?1,?2)
            ON CONFLICT(kind,resource_id) DO UPDATE SET generation=excluded.generation,manager=NULL,owner_id=NULL,logical_key=NULL",
            params![principal.to_string(),generation])?;
        let operation = NativeOperation {
            operation_id: id,
            ownership,
            action: NativeAction::RevokeSandboxAccess,
            state: NativeOperationState::Succeeded,
            error_code: None,
            configuration_generation: None,
        };
        super::iac_native::save(&tx, &operation)?;
        tx.commit()?;
        Ok(operation)
    }
}

//! Native installations have their own physical binding, outside the Linux guest's Pipe realm.

use super::{PersistenceError as Error, Store, iac_host, iac_owners};
use firecrab_helper_protocol::native_iac::{OwnerChange, Receipt, Rejection, Target};
use firecrab_stack::{ChangeResourceOwner, Manager, ResourceOwnership};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

#[cfg(test)]
#[path = "iac_native_host_tests.rs"]
mod tests;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS iac_native_host_binding (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1), identity TEXT NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS iac_native_host_bindings (
 kind TEXT PRIMARY KEY CHECK(kind IN ('management_settings','host_configuration')), identity TEXT NOT NULL
) STRICT;";

/// Preserve the original realm of every legacy admitted intent, atomically.
pub(super) fn initialize(conn: &rusqlite::Connection) -> Result<(), Error> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(SCHEMA)?;
    let original: Option<String> = tx
        .query_row(
            "SELECT identity FROM iac_native_host_binding WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(original) = original {
        let target: Target =
            serde_json::from_str(&original).map_err(|_| Error::IacOperationIdentity)?;
        target.validate().map_err(|_| Error::IacOperationIdentity)?;
        let kinds=tx.prepare("SELECT DISTINCT kind FROM iac_host_owner_intents WHERE kind IN ('management_settings','host_configuration') AND json_extract(intent,'$.target')=?1")?
            .query_map([&original],|row|row.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        if kinds.is_empty() {
            return Err(Error::IacOperationIdentity);
        }
        for kind in kinds {
            let bound: Option<String> = tx
                .query_row(
                    "SELECT identity FROM iac_native_host_bindings WHERE kind=?1",
                    [&kind],
                    |row| row.get(0),
                )
                .optional()?;
            if bound.as_ref().is_some_and(|bound| bound != &original) {
                return Err(Error::IacEndpointConflict);
            }
            tx.execute(
                "INSERT INTO iac_native_host_bindings VALUES(?1,?2) ON CONFLICT DO NOTHING",
                params![kind, original],
            )?;
        }
    }
    tx.execute("DROP TABLE iac_native_host_binding", [])?;
    tx.commit()?;
    Ok(())
}

/// Construction requires this database's original durable intent and exact native result.
pub(super) struct NativeProof {
    intent: OwnerChange,
    receipt: Receipt,
}

impl NativeProof {
    pub(super) fn check(&self, request: &ChangeResourceOwner) -> Result<(), Error> {
        let generation = request
            .expected_generation
            .checked_add(u64::from(request.expected_owner != request.owner))
            .ok_or(Error::StaleIacOwner)?;
        if self.intent.change != *request
            || self.receipt.operation_id != request.operation_id
            || self.receipt.target != self.intent.target
            || self.receipt.request_sha256
                != self
                    .intent
                    .request_sha256()
                    .map_err(|_| Error::IacOperationIdentity)?
            || self.receipt.ownership
                != (ResourceOwnership {
                    endpoint_id: request.expected_endpoint_id,
                    resource: request.resource.clone(),
                    generation,
                    owner: request.owner.clone(),
                })
        {
            return Err(Error::IacOperationIdentity);
        }
        Ok(())
    }
}

impl Store {
    /// Retained intents are recovered before reading mutable native settings or TLS connectivity.
    pub(crate) fn native_host_owner_intent(
        &self,
        operation: Uuid,
    ) -> Result<Option<OwnerChange>, Error> {
        self.lock().query_row(
            "SELECT intent FROM iac_host_owner_intents WHERE operation_id=?1 AND kind IN ('management_settings','host_configuration')",
            [operation.to_string()], |row| row.get::<_, String>(0),
        ).optional()?.map(|raw| serde_json::from_str(&raw).map_err(|_| Error::IacOperationIdentity)).transpose()
    }

    /// The native Mac/Windows identity never replaces the API guest's bound Linux host identity.
    pub(crate) fn check_iac_native_host(&self, kind: &str, target: &Target) -> Result<(), Error> {
        if !firecrab_helper_protocol::native_iac::native_kind(kind) {
            return Err(Error::UnsupportedIacResource);
        }
        target.validate().map_err(|_| Error::InvalidIacOwner)?;
        let raw: Option<String> = self
            .lock()
            .query_row(
                "SELECT identity FROM iac_native_host_bindings WHERE kind=?1",
                [kind],
                |row| row.get(0),
            )
            .optional()?;
        let original = raw
            .map(|raw| {
                serde_json::from_str::<Target>(&raw).map_err(|_| Error::IacOperationIdentity)
            })
            .transpose()?;
        if original.as_ref().is_some_and(|original| original != target) {
            return Err(Error::IacEndpointConflict);
        }
        Ok(())
    }

    /// Native and Pipe transitions share operation IDs and logical-address reservations.
    pub(crate) fn prepare_native_host_owner(
        &self,
        mut intent: OwnerChange,
    ) -> Result<OwnerChange, Error> {
        intent.validate().map_err(|_| Error::InvalidIacOwner)?;
        let request = &intent.change;
        let hash = intent
            .request_sha256()
            .map_err(|_| Error::IacOperationIdentity)?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if iac_owners::endpoint(&tx)? != request.expected_endpoint_id {
            return Err(Error::IacEndpointConflict);
        }
        let previous: Option<(String, String)> = tx
            .query_row(
                "SELECT request_hash,intent FROM iac_host_owner_intents WHERE operation_id=?1",
                [request.operation_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((previous_hash, raw)) = previous {
            if previous_hash != hash {
                return Err(Error::IacOperationIdentity);
            }
            return serde_json::from_str(&raw).map_err(|_| Error::IacOperationIdentity);
        }
        if tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM iac_owner_changes WHERE operation_id=?1)",
            [request.operation_id.to_string()],
            |row| row.get::<_, bool>(0),
        )? {
            return Err(Error::IacOperationIdentity);
        }
        iac_host::prepare_owner(&tx, request)?;
        let identity =
            serde_json::to_string(&intent.target).map_err(|_| Error::IacOperationIdentity)?;
        let original: Option<String> = tx
            .query_row(
                "SELECT identity FROM iac_native_host_bindings WHERE kind=?1",
                [&request.resource.kind],
                |row| row.get(0),
            )
            .optional()?;
        if original
            .as_ref()
            .is_some_and(|original| original != &identity)
        {
            return Err(Error::IacEndpointConflict);
        }
        tx.execute(
            "INSERT INTO iac_native_host_bindings VALUES(?1,?2) ON CONFLICT DO NOTHING",
            params![request.resource.kind, identity],
        )?;
        tx.execute("INSERT INTO iac_host_owner_intents(operation_id,request_hash,intent,kind,resource_id,manager,owner_id,logical_key) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![request.operation_id.to_string(),hash,serde_json::to_string(&intent).map_err(|_| Error::IacOperationIdentity)?,request.resource.kind,request.resource.id,
                request.owner.as_ref().map(|owner| match owner.manager { Manager::Stack => "stack", Manager::Terraform => "terraform" }),request.owner.as_ref().map(|owner|owner.id.to_string()),request.owner.as_ref().map(|owner|owner.key.as_str())])?;
        tx.commit()?;
        Ok(intent)
    }

    /// Only a verified native receipt may enter the shared owner and immutable-result transaction.
    pub(crate) fn commit_native_host_owner(
        &self,
        request: &ChangeResourceOwner,
        receipt: Receipt,
    ) -> Result<ResourceOwnership, Error> {
        let intent = self
            .native_host_owner_intent(request.operation_id)?
            .ok_or(Error::IacOperationIdentity)?;
        let proof = NativeProof { intent, receipt };
        proof.check(request)?;
        self.change_iac_native_host_owner(request, &proof)
    }

    /// An unavailable transport cannot release a reservation; a retained no-effect result can.
    pub(crate) fn reject_native_host_owner(
        &self,
        request: &ChangeResourceOwner,
        rejection: Rejection,
    ) -> Result<(), Error> {
        let intent = self
            .native_host_owner_intent(request.operation_id)?
            .ok_or(Error::IacOperationIdentity)?;
        if intent.change != *request
            || rejection.operation_id != request.operation_id
            || rejection.target != intent.target
            || rejection.request_sha256
                != intent
                    .request_sha256()
                    .map_err(|_| Error::IacOperationIdentity)?
        {
            return Err(Error::IacOperationIdentity);
        }
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM iac_owner_changes WHERE operation_id=?1)",
            [request.operation_id.to_string()],
            |row| row.get::<_, bool>(0),
        )? {
            return Err(Error::IacOperationIdentity);
        }
        if tx.execute("UPDATE iac_host_owner_intents SET confirmed=1,rejected=1 WHERE operation_id=?1 AND kind IN ('management_settings','host_configuration')", [request.operation_id.to_string()])? != 1 {
            return Err(Error::IacOperationIdentity);
        }
        tx.commit()?;
        Ok(())
    }
}

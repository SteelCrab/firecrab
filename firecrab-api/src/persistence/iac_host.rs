//! API reservations bridge the privileged host commit and its durable local ownership mirror.

use firecrab_helper_protocol::host_iac::{HostIdentity, OwnerChange, OwnerReceipt, OwnerRejection};
use firecrab_stack::{ChangeResourceOwner, Manager, ResourceOwnership};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use super::{PersistenceError as Error, Store, iac_owners};

#[cfg(test)]
#[path = "iac_host_tests.rs"]
mod tests;

/// No credential or arbitrary execution input is stored in these metadata-only intents.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS iac_host_binding (singleton INTEGER PRIMARY KEY CHECK(singleton=1), identity TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS iac_host_owner_intents (
 operation_id TEXT PRIMARY KEY, request_hash TEXT NOT NULL, intent TEXT NOT NULL,
 kind TEXT NOT NULL, resource_id TEXT NOT NULL, manager TEXT, owner_id TEXT, logical_key TEXT,
 confirmed INTEGER NOT NULL DEFAULT 0 CHECK(confirmed IN (0,1)),
 rejected INTEGER NOT NULL DEFAULT 0 CHECK(rejected IN (0,1))
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS iac_host_owner_reservation ON iac_host_owner_intents(kind,resource_id) WHERE confirmed=0;
CREATE TRIGGER IF NOT EXISTS iac_host_stack_delete BEFORE DELETE ON stacks
WHEN EXISTS(SELECT 1 FROM iac_host_owner_intents WHERE confirmed=0 AND
 ((manager='stack' AND owner_id=OLD.id) OR (json_extract(intent,'$.change.expectedOwner.manager')='stack' AND json_extract(intent,'$.change.expectedOwner.id')=OLD.id)))
BEGIN SELECT RAISE(ABORT,'iac_host_owner_pending'); END;
";

/// A verified target receipt, constructed only from this database's original admitted intent.
pub(super) struct HostProof {
    /// Frozen CAS and physical installation observed before the privileged mutation.
    intent: OwnerChange,
    /// Target acknowledgement remains reserved until the local mirror commits.
    receipt: OwnerReceipt,
}
impl HostProof {
    /// Recheck all public identities while the ordinary ownership transaction is active.
    pub(super) fn check(&self, request: &ChangeResourceOwner) -> Result<(), Error> {
        let expected_generation = request
            .expected_generation
            .checked_add(u64::from(request.expected_owner != request.owner))
            .ok_or(Error::StaleIacOwner)?;
        if self.intent.change != *request
            || self.receipt.operation_id != request.operation_id
            || self.receipt.request_sha256
                != self
                    .intent
                    .request_sha256()
                    .map_err(|_| Error::IacOperationIdentity)?
            || self.receipt.host != self.intent.host
            || self.receipt.ownership
                != (ResourceOwnership {
                    endpoint_id: request.expected_endpoint_id,
                    resource: request.resource.clone(),
                    generation: expected_generation,
                    owner: request.owner.clone(),
                })
        {
            return Err(Error::IacOperationIdentity);
        }
        Ok(())
    }
}

/// Protect pending resource and desired logical address across all transfer adapters.
pub(super) fn ensure_available(
    conn: &Connection,
    request: &ChangeResourceOwner,
) -> Result<(), Error> {
    let original: Option<String> = conn
        .query_row(
            "SELECT request_hash FROM iac_host_owner_intents WHERE operation_id=?1",
            [request.operation_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(original) = original {
        use sha2::{Digest, Sha256};
        if original
            != format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(request).map_err(|_| Error::IacOperationIdentity)?
                )
            )
        {
            return Err(Error::IacOperationIdentity);
        }
    }
    let manager = request.owner.as_ref().map(|owner| match owner.manager {
        Manager::Stack => "stack",
        Manager::Terraform => "terraform",
    });
    let occupied: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM iac_host_owner_intents WHERE confirmed=0 AND operation_id<>?1 AND
        ((kind=?2 AND resource_id=?3) OR (manager=?4 AND owner_id=?5 AND logical_key=?6 AND (manager='terraform' OR kind=?2))))",
        params![request.operation_id.to_string(),request.resource.kind,request.resource.id,manager,request.owner.as_ref().map(|owner|owner.id.to_string()),request.owner.as_ref().map(|owner|owner.key.as_str())], |row|row.get(0))?;
    if occupied {
        return Err(Error::IacResourceBusy);
    }
    Ok(())
}

/// Ordinary native writers cannot cross an API reservation before host admission or confirmation.
pub(super) fn ensure_resource_idle(
    conn: &Connection,
    resource: &firecrab_stack::ManagedResource,
) -> Result<(), Error> {
    let busy:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM iac_host_owner_intents WHERE confirmed=0 AND kind=?1 AND resource_id=?2)", params![resource.kind,resource.id],|row|row.get(0))?;
    if busy {
        return Err(Error::IacResourceBusy);
    }
    Ok(())
}

impl Store {
    /// Shared host units cannot be reused while any connector owner or transfer occupies the slot.
    pub(crate) fn iac_host_kind_public_write_allowed(&self, kind: &str) -> Result<bool, Error> {
        if !firecrab_helper_protocol::host_iac::is_pipe_kind(kind) {
            return Err(Error::UnsupportedIacResource);
        }
        Ok(!self.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM iac_resource_owners WHERE kind=?1 AND manager IS NOT NULL)
             OR EXISTS(SELECT 1 FROM iac_host_owner_intents WHERE kind=?1 AND confirmed=0)",
            [kind],
            |row| row.get::<_, bool>(0),
        )?)
    }

    /// Local terminal state remains readable after a later helper outage.
    pub(crate) fn host_owner_status(&self, operation: Uuid) -> Result<(bool, bool), Error> {
        Ok(self.lock().query_row(
            "SELECT confirmed,rejected FROM iac_host_owner_intents WHERE operation_id=?1",
            [operation.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?)
    }

    /// Only a retained, exact no-effect target result can release an uncommitted API reservation.
    pub(crate) fn reject_host_owner(
        &self,
        request: &ChangeResourceOwner,
        rejection: OwnerRejection,
    ) -> Result<(), Error> {
        let intent = self
            .host_owner_intent(request.operation_id)?
            .ok_or(Error::IacOperationIdentity)?;
        if intent.change != *request
            || rejection.operation_id != request.operation_id
            || rejection.host != intent.host
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
        tx.execute(
            "UPDATE iac_host_owner_intents SET confirmed=1,rejected=1 WHERE operation_id=?1",
            [request.operation_id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Recover the original observation instead of following a changed host or manifest on retry.
    pub(crate) fn host_owner_intent(&self, operation: Uuid) -> Result<Option<OwnerChange>, Error> {
        self.lock()
            .query_row(
                "SELECT intent FROM iac_host_owner_intents WHERE operation_id=?1",
                [operation.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|raw| serde_json::from_str(&raw).map_err(|_| Error::IacOperationIdentity))
            .transpose()
    }

    /// Bind a configured API to the original physical host, even before the first mirror is committed.
    pub(crate) fn check_iac_host(&self, host: &HostIdentity) -> Result<(), Error> {
        let raw: Option<String> = self
            .lock()
            .query_row(
                "SELECT identity FROM iac_host_binding WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if raw
            .map(|raw| {
                serde_json::from_str::<HostIdentity>(&raw).map_err(|_| Error::IacOperationIdentity)
            })
            .transpose()?
            .as_ref()
            .is_some_and(|original| original != host)
        {
            return Err(Error::IacEndpointConflict);
        }
        Ok(())
    }

    /// Reserve original owner metadata and the desired logical address before privileged admission.
    pub(crate) fn prepare_host_owner(&self, mut intent: OwnerChange) -> Result<OwnerChange, Error> {
        intent.validate().map_err(|_| Error::InvalidIacOwner)?;
        let hash = intent
            .request_sha256()
            .map_err(|_| Error::IacOperationIdentity)?;
        let request = &intent.change;
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
        ensure_available(&tx, request)?;
        let before = iac_owners::read(&tx, &request.resource)?;
        if before.generation != request.expected_generation
            || before.owner != request.expected_owner
        {
            return Err(Error::StaleIacOwner);
        }
        super::iac_native::ensure_idle(&tx, &request.resource)?;
        if request.owner.is_some() {
            super::iac_resources::ensure_independent(&tx, &request.resource)?;
        }
        for owner in [&request.owner, &request.expected_owner]
            .into_iter()
            .flatten()
        {
            if owner.manager == Manager::Stack {
                super::stack_applies::ensure_idle(&tx, owner.id)?;
                super::stack_sources::ensure_idle(&tx, owner.id)?;
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM stacks WHERE id=?1)",
                    [owner.id.to_string()],
                    |row| row.get(0),
                )?;
                if !exists {
                    return Err(Error::MissingStack);
                }
            }
        }
        if let Some(owner) = &request.owner {
            let manager = match owner.manager {
                Manager::Stack => "stack",
                Manager::Terraform => "terraform",
            };
            let occupied:bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM iac_resource_owners WHERE manager=?1 AND owner_id=?2 AND logical_key=?3
                AND (manager='terraform' OR kind=?4) AND (kind<>?4 OR resource_id<>?5))",
                params![manager,owner.id.to_string(),owner.key,request.resource.kind,request.resource.id],|row|row.get(0))?;
            if occupied {
                return Err(Error::IacOwnershipConflict);
            }
            if owner.manager == Manager::Stack
                && super::stacks::binding_count(&tx, owner.id, Some(&request.resource))?
                    >= firecrab_stack::MAX_RESOURCES
            {
                return Err(Error::StackBindingLimit);
            }
        }
        let identity =
            serde_json::to_string(&intent.host).map_err(|_| Error::IacOperationIdentity)?;
        let original: Option<String> = tx
            .query_row(
                "SELECT identity FROM iac_host_binding WHERE singleton=1",
                [],
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
            "INSERT INTO iac_host_binding VALUES(1,?1) ON CONFLICT DO NOTHING",
            [&identity],
        )?;
        tx.execute("INSERT INTO iac_host_owner_intents(operation_id,request_hash,intent,kind,resource_id,manager,owner_id,logical_key) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![request.operation_id.to_string(), hash,serde_json::to_string(&intent).map_err(|_|Error::IacOperationIdentity)?,request.resource.kind,request.resource.id,
                request.owner.as_ref().map(|owner|match owner.manager {Manager::Stack=>"stack",Manager::Terraform=>"terraform"}),request.owner.as_ref().map(|owner|owner.id.to_string()),request.owner.as_ref().map(|owner|owner.key.as_str())])?;
        tx.commit()?;
        Ok(intent)
    }

    /// Only the exact verified host receipt can enter the ordinary ownership transaction.
    pub(crate) fn commit_host_owner(
        &self,
        request: &ChangeResourceOwner,
        receipt: OwnerReceipt,
    ) -> Result<ResourceOwnership, Error> {
        let intent = self
            .host_owner_intent(request.operation_id)?
            .ok_or(Error::IacOperationIdentity)?;
        let proof = HostProof { intent, receipt };
        proof.check(request)?;
        self.change_iac_host_owner(request, &proof)
    }

    /// Release the API reservation only after both durable mirrors have the original receipt.
    pub(crate) fn confirm_host_owner(&self, operation: Uuid) -> Result<(), Error> {
        let conn = self.lock();
        if !conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM iac_owner_changes WHERE operation_id=?1)",
            [operation.to_string()],
            |row| row.get::<_, bool>(0),
        )? {
            return Err(Error::IacOperationIdentity);
        }
        if conn.execute(
            "UPDATE iac_host_owner_intents SET confirmed=1 WHERE operation_id=?1",
            [operation.to_string()],
        )? != 1
        {
            return Err(Error::IacOperationIdentity);
        }
        Ok(())
    }
}

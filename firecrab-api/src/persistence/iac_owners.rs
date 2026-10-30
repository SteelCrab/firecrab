//! Atomic ownership transfers and immutable acknowledgements across IaC workflows.

use firecrab_stack::{
    Binding, ChangeResourceOwner, ManagedResource, Manager, ResourceOwner, ResourceOwnership,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{PersistenceError, Store};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS iac_endpoint (singleton INTEGER PRIMARY KEY CHECK(singleton=1), endpoint_id TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS iac_resource_owners (
    kind TEXT NOT NULL, resource_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK(generation >= 0),
    manager TEXT CHECK(manager IN ('terraform','stack')),
    owner_id TEXT, logical_key TEXT,
    CHECK((manager IS NULL AND owner_id IS NULL AND logical_key IS NULL)
       OR (manager IS NOT NULL AND owner_id IS NOT NULL AND logical_key IS NOT NULL)),
    PRIMARY KEY(kind, resource_id)
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS iac_terraform_address ON iac_resource_owners(owner_id,logical_key) WHERE manager='terraform';
CREATE TABLE IF NOT EXISTS iac_owner_changes (
    operation_id TEXT PRIMARY KEY, request_hash TEXT NOT NULL, result TEXT NOT NULL
) STRICT;
INSERT INTO iac_resource_owners(kind,resource_id,generation)
SELECT kind,resource_id,1 FROM stack_bindings WHERE true
ON CONFLICT(kind,resource_id) DO NOTHING;";

pub(super) fn initialize(conn: &Connection) -> Result<(), PersistenceError> {
    conn.execute_batch(SCHEMA)?;
    conn.execute("INSERT INTO iac_endpoint(singleton,endpoint_id) VALUES (1,?1) ON CONFLICT(singleton) DO NOTHING",
        [Uuid::new_v4().to_string()])?;
    Ok(())
}

fn endpoint(conn: &Connection) -> Result<Uuid, PersistenceError> {
    let raw: String = conn.query_row(
        "SELECT endpoint_id FROM iac_endpoint WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    Uuid::parse_str(&raw)
        .ok()
        .filter(|id| !id.is_nil())
        .ok_or(PersistenceError::IacEndpointConflict)
}

/// Called in the same transaction as every legacy binding change.
/// Tombstones preserve generations across release/reclaim and API restarts.
pub(super) fn native_binding_changed(
    conn: &Connection,
    kind: &str,
    resource: Uuid,
) -> Result<(), PersistenceError> {
    let changed = conn.execute(
        "INSERT INTO iac_resource_owners(kind,resource_id,generation) VALUES (?1,?2,1)
         ON CONFLICT(kind,resource_id) DO UPDATE SET generation=generation+1 WHERE manager IS NULL",
        params![kind, resource.to_string()],
    )?;
    if changed != 1 {
        return Err(PersistenceError::IacOwnershipConflict);
    }
    Ok(())
}

pub(super) fn terraform_owned(
    conn: &Connection,
    kind: &str,
    resource: Uuid,
) -> Result<bool, PersistenceError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM iac_resource_owners WHERE kind=?1 AND resource_id=?2 AND manager='terraform')",
        params![kind, resource.to_string()], |row| row.get(0),
    )?)
}

impl Store {
    /// Stable endpoint identity for clients that manage multiple API hosts.
    pub(crate) fn iac_endpoint_id(&self) -> Result<Uuid, PersistenceError> {
        endpoint(&self.lock())
    }

    /// Ownership metadata only; a free slot is not a resource-existence assertion.
    pub(crate) fn iac_resource_owner(
        &self,
        resource: &ManagedResource,
    ) -> Result<ResourceOwnership, PersistenceError> {
        let mut resource = resource.clone();
        resource
            .normalize()
            .map_err(|_| PersistenceError::InvalidIacOwner)?;
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let result = read(&tx, &resource)?;
        tx.commit()?;
        Ok(result)
    }

    /// Active ownership records only; released tombstones stay available by exact identity.
    pub(crate) fn list_iac_resource_owners(
        &self,
    ) -> Result<Vec<ResourceOwnership>, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let mut statement = tx.prepare(
            "SELECT kind,resource_id FROM iac_resource_owners WHERE manager IS NOT NULL
             UNION SELECT kind,resource_id FROM stack_bindings ORDER BY kind,resource_id",
        )?;
        let resources = statement
            .query_map([], |row| {
                Ok(ManagedResource {
                    kind: row.get(0)?,
                    id: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let result = resources
            .iter()
            .map(|resource| read(&tx, resource))
            .collect::<Result<_, _>>()?;
        tx.commit()?;
        Ok(result)
    }

    /// Read the original committed result without replaying any transition.
    pub(crate) fn iac_ownership_operation(
        &self,
        id: Uuid,
    ) -> Result<Option<firecrab_stack::OwnershipOperation>, PersistenceError> {
        let raw: Option<String> = self
            .lock()
            .query_row(
                "SELECT result FROM iac_owner_changes WHERE operation_id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        raw.map(|raw| {
            Ok(firecrab_stack::OwnershipOperation {
                operation_id: id,
                ownership: serde_json::from_str(&raw)
                    .map_err(|_| PersistenceError::IacOperationIdentity)?,
            })
        })
        .transpose()
    }

    /// Atomically transfer a verified resource without changing its configuration or data.
    pub(crate) fn change_iac_resource_owner(
        &self,
        request: &ChangeResourceOwner,
    ) -> Result<ResourceOwnership, PersistenceError> {
        let mut request = request.clone();
        request
            .validate()
            .map_err(|_| PersistenceError::InvalidIacOwner)?;
        let hash = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&request).map_err(|_| PersistenceError::IacOperationIdentity)?
            )
        );
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if endpoint(&tx)? != request.expected_endpoint_id {
            return Err(PersistenceError::IacEndpointConflict);
        }
        let previous: Option<(String, String)> = tx
            .query_row(
                "SELECT request_hash,result FROM iac_owner_changes WHERE operation_id=?1",
                [request.operation_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((previous_hash, result)) = previous {
            if previous_hash != hash {
                return Err(PersistenceError::IacOperationIdentity);
            }
            return serde_json::from_str(&result)
                .map_err(|_| PersistenceError::IacOperationIdentity);
        }
        // Kind support grows only when its existence, parent and activity checks exist.
        let kind = request
            .resource
            .legacy_kind()
            .ok_or(PersistenceError::UnsupportedIacResource)?;
        let resource_id = Uuid::parse_str(&request.resource.id)
            .map_err(|_| PersistenceError::InvalidStackResource)?;
        let before = read(&tx, &request.resource)?;
        if before.generation != request.expected_generation
            || before.owner != request.expected_owner
        {
            return Err(PersistenceError::StaleIacOwner);
        }
        if super::stacks::resource_apply_busy(&tx, kind, resource_id)? {
            return Err(PersistenceError::StackApplyBusy);
        }
        ensure_resource_idle(&tx, kind, resource_id)?;
        for owner in [&before.owner, &request.owner].into_iter().flatten() {
            if owner.manager == Manager::Stack {
                super::stack_applies::ensure_idle(&tx, owner.id)?;
                super::stack_sources::ensure_idle(&tx, owner.id)?;
            }
        }
        if request.owner.is_some() {
            super::stacks::validate_resource(
                &tx,
                &Binding {
                    kind,
                    key: "adopt".into(),
                    resource_id,
                },
            )?;
        }
        if let Some(owner) = request
            .owner
            .as_ref()
            .filter(|owner| owner.manager == Manager::Terraform)
        {
            let occupied: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM iac_resource_owners WHERE manager='terraform' AND owner_id=?1 AND logical_key=?2 AND (kind<>?3 OR resource_id<>?4))",
                params![owner.id.to_string(),owner.key,request.resource.kind,request.resource.id], |row|row.get(0),
            )?;
            if occupied {
                return Err(PersistenceError::IacOwnershipConflict);
            }
        }
        let mut result = before.clone();
        if before.owner != request.owner {
            if let Some(owner) = before
                .owner
                .as_ref()
                .filter(|owner| owner.manager == Manager::Stack)
            {
                if tx.execute("DELETE FROM stack_bindings WHERE stack_id=?1 AND kind=?2 AND logical_key=?3 AND resource_id=?4",
                    params![owner.id.to_string(), kind.as_str(),owner.key,resource_id.to_string()])? != 1 {
                    return Err(PersistenceError::IacOwnershipConflict);
                }
                tx.execute(
                    "UPDATE stacks SET revision=revision+1 WHERE id=?1",
                    [owner.id.to_string()],
                )?;
            }
            if let Some(owner) = request
                .owner
                .as_ref()
                .filter(|owner| owner.manager == Manager::Stack)
            {
                let name: String = tx
                    .query_row(
                        "SELECT name FROM stacks WHERE id=?1",
                        [owner.id.to_string()],
                        |row| row.get(0),
                    )
                    .optional()?
                    .ok_or(PersistenceError::MissingStack)?;
                let stack =
                    super::stacks::read_stack(&tx, &name)?.ok_or(PersistenceError::MissingStack)?;
                if stack.bindings.len() >= firecrab_stack::MAX_RESOURCES {
                    return Err(PersistenceError::StackBindingLimit);
                }
                if stack
                    .bindings
                    .iter()
                    .any(|binding| binding.kind == kind && binding.key == owner.key)
                {
                    return Err(PersistenceError::IacOwnershipConflict);
                }
                tx.execute("INSERT INTO stack_bindings(stack_id,kind,logical_key,resource_id) VALUES (?1,?2,?3,?4)",
                    params![owner.id.to_string(),kind.as_str(),owner.key,resource_id.to_string()])?;
                tx.execute(
                    "UPDATE stacks SET revision=revision+1 WHERE id=?1",
                    [owner.id.to_string()],
                )?;
            }
            result.generation = before
                .generation
                .checked_add(1)
                .filter(|generation| *generation <= i64::MAX as u64)
                .ok_or(PersistenceError::StaleIacOwner)?;
            result.owner = request.owner.clone();
            let external = request
                .owner
                .as_ref()
                .filter(|owner| owner.manager == Manager::Terraform);
            tx.execute("INSERT INTO iac_resource_owners(kind,resource_id,generation,manager,owner_id,logical_key)
                VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(kind,resource_id) DO UPDATE SET
                generation=excluded.generation,manager=excluded.manager,owner_id=excluded.owner_id,logical_key=excluded.logical_key",
                params![kind.as_str(),resource_id.to_string(),i64::try_from(result.generation).map_err(|_|PersistenceError::StaleIacOwner)?,
                    external.map(|_|"terraform"),external.map(|owner|owner.id.to_string()),external.map(|owner|owner.key.as_str())])?;
        }
        tx.execute(
            "INSERT INTO iac_owner_changes(operation_id,request_hash,result) VALUES (?1,?2,?3)",
            params![
                request.operation_id.to_string(),
                hash,
                serde_json::to_string(&result)
                    .map_err(|_| PersistenceError::IacOperationIdentity)?
            ],
        )?;
        tx.commit()?;
        Ok(result)
    }
}

/// Do not move write authority while another controller owns an admitted operation.
fn ensure_resource_idle(
    conn: &Connection,
    kind: firecrab_stack::ResourceKind,
    id: Uuid,
) -> Result<(), PersistenceError> {
    match kind {
        firecrab_stack::ResourceKind::Network => Ok(()),
        firecrab_stack::ResourceKind::Vm => {
            let busy:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM vms WHERE id=?1 AND state IN ('starting','pausing','resuming','stopping'))",[id.to_string()],|row|row.get(0))?;
            if busy {
                return Err(PersistenceError::IacResourceBusy);
            }
            Ok(())
        }
        firecrab_stack::ResourceKind::Service => {
            let worker: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM cluster_service_workers WHERE id=?1)",
                [id.to_string()],
                |row| row.get(0),
            )?;
            if worker {
                return Err(PersistenceError::InvalidStackResource);
            }
            let raw: Option<String> = conn
                .query_row(
                    "SELECT deployment FROM micro_service_deployments WHERE service_id=?1",
                    [id.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(raw) = raw {
                let deployment: firecrab_api_types::MicroServiceDeployment =
                    serde_json::from_str(&raw)
                        .map_err(|_| PersistenceError::IacOwnershipConflict)?;
                if deployment.phase.is_busy()
                    || deployment
                        .update
                        .as_ref()
                        .is_some_and(|update| !update.completed)
                    || deployment
                        .recovery
                        .as_ref()
                        .is_some_and(|recovery| recovery.repair.is_some())
                {
                    return Err(PersistenceError::IacResourceBusy);
                }
            }
            Ok(())
        }
    }
}

#[derive(Default)]
struct StoredOwner {
    generation: i64,
    manager: Option<String>,
    id: Option<String>,
    key: Option<String>,
}

fn read(
    conn: &Connection,
    resource: &ManagedResource,
) -> Result<ResourceOwnership, PersistenceError> {
    let row: Option<StoredOwner> = conn.query_row(
        "SELECT generation,manager,owner_id,logical_key FROM iac_resource_owners WHERE kind=?1 AND resource_id=?2",
        params![resource.kind,resource.id],|row|Ok(StoredOwner {generation:row.get(0)?,manager:row.get(1)?,id:row.get(2)?,key:row.get(3)?}),
    ).optional()?;
    let StoredOwner {
        generation,
        manager,
        id: owner_id,
        key,
    } = row.unwrap_or_default();
    let generation =
        u64::try_from(generation).map_err(|_| PersistenceError::IacOwnershipConflict)?;
    let external = match (manager, owner_id, key) {
        (None, None, None) => None,
        (Some(manager), Some(id), Some(key)) if manager == "terraform" => Some(ResourceOwner {
            manager: Manager::Terraform,
            id: Uuid::parse_str(&id).map_err(|_| PersistenceError::IacOwnershipConflict)?,
            key,
        }),
        _ => return Err(PersistenceError::IacOwnershipConflict),
    };
    let native: Option<(String, String)> = conn
        .query_row(
            "SELECT stack_id,logical_key FROM stack_bindings WHERE kind=?1 AND resource_id=?2",
            params![resource.kind, resource.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let owner = match (external, native) {
        (Some(_), Some(_)) => return Err(PersistenceError::IacOwnershipConflict),
        (owner, None) => owner,
        (None, Some((id, key))) => Some(ResourceOwner {
            manager: Manager::Stack,
            id: Uuid::parse_str(&id).map_err(|_| PersistenceError::IacOwnershipConflict)?,
            key,
        }),
    };
    if let Some(owner) = &owner {
        owner
            .validate()
            .map_err(|_| PersistenceError::IacOwnershipConflict)?;
    }
    Ok(ResourceOwnership {
        endpoint_id: endpoint(conn)?,
        resource: resource.clone(),
        generation,
        owner,
    })
}

#[cfg(test)]
#[path = "../../tests/unit/iac_owners.rs"]
mod tests;

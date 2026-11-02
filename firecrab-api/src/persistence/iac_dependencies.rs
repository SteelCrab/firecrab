//! Shared parent leases. Independent children may share a read lease; configuration writers wait.
use super::{PersistenceError, iac_owners};
use firecrab_stack::{ManagedResource, NativeOperation, ResourceOwnership};
use rusqlite::{Connection, params};
use std::collections::BTreeSet;
use uuid::Uuid;

pub(super) fn validate(conn: &Connection, operation: Uuid) -> Result<(), PersistenceError> {
    let mut stmt =
        conn.prepare("SELECT ownership FROM iac_native_dependencies WHERE operation_id=?1")?;
    let rows = stmt.query_map([operation.to_string()], |row| row.get::<_, String>(0))?;
    for raw in rows {
        let expected: ResourceOwnership =
            serde_json::from_str(&raw?).map_err(|_| PersistenceError::IacOperationIdentity)?;
        if iac_owners::read(conn, &expected.resource)? != expected {
            return Err(PersistenceError::StaleIacOwner);
        }
    }
    Ok(())
}

pub(super) fn reserve(
    conn: &Connection,
    operation: &NativeOperation,
    mut parent: ManagedResource,
) -> Result<(), PersistenceError> {
    parent
        .normalize()
        .map_err(|_| PersistenceError::InvalidIacOwner)?;
    let busy: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM iac_native_operations
        WHERE kind=?1 AND resource_id=?2 AND state IN ('running','needs_recovery'))",
        params![parent.kind, parent.id],
        |row| row.get(0),
    )?;
    if busy {
        return Err(PersistenceError::IacResourceBusy);
    }
    let ownership = iac_owners::read(conn, &parent)?;
    let snapshot =
        serde_json::to_string(&ownership).map_err(|_| PersistenceError::IacOperationIdentity)?;
    conn.execute(
        "INSERT INTO iac_native_dependencies VALUES(?1,?2,?3,?4)",
        params![
            operation.operation_id.to_string(),
            parent.kind,
            parent.id,
            snapshot
        ],
    )?;
    Ok(())
}

/// Capture durable origin records, including releases that have left the active service head.
pub(super) fn reserve_vm_parents(
    conn: &Connection,
    operation: &NativeOperation,
    vm: Uuid,
) -> Result<(), PersistenceError> {
    let mut parents = BTreeSet::new();
    let mut pool = conn.prepare("SELECT pool_id FROM vm_pool_members WHERE vm_id=?1")?;
    for id in pool.query_map([vm.to_string()], |row| row.get::<_, String>(0))? {
        parents.insert(("pool".to_owned(), id?));
    }
    let mut env = conn.prepare(
        "SELECT r.environment_id,e.project_id FROM ai_sandbox_environment_resources r
        JOIN ai_sandbox_environments e ON e.id=r.environment_id WHERE r.kind='vm' AND r.id=?1",
    )?;
    for row in env.query_map([vm.to_string()], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (environment, project) = row?;
        parents.insert(("sandbox_environment".to_owned(), environment));
        parents.insert(("sandbox_project".to_owned(), project));
    }
    let mut broker=conn.prepare("SELECT json_extract(record,'$.vm_id') FROM queue_engine WHERE json_extract(record,'$.vm_id')=?1")?;
    for id in broker.query_map([vm.to_string()], |row| row.get::<_, String>(0))? {
        parents.insert(("queue_engine".to_owned(), id?));
    }
    for table in [
        "micro_service_deployments",
        "micro_service_attempts",
        "micro_service_releases",
    ] {
        let mut stmt = conn.prepare(&format!(
            "SELECT service_id,deployment FROM {table} WHERE instr(deployment,?1)>0"
        ))?;
        for row in stmt.query_map([vm.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (service, raw) = row?;
            let deployment: firecrab_api_types::MicroServiceDeployment =
                serde_json::from_str(&raw).map_err(|_| PersistenceError::IacOperationIdentity)?;
            if deployment.owned_vm_ids().any(|id| id == vm) {
                parents.insert(("service".into(), service));
            }
        }
    }
    for (kind, id) in parents {
        reserve(conn, operation, ManagedResource { kind, id })?;
    }
    Ok(())
}

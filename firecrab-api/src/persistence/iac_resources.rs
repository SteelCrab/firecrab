//! Resource existence and controller activity checks for ownership transitions.

use firecrab_stack::ManagedResource;
use rusqlite::{Connection, OptionalExtension, params};

use super::PersistenceError;

/// Advertise only kinds with an existence check and a public mutation gate.
pub(crate) const TRANSFER_KINDS: &[&str] = &[
    "network",
    "vm",
    "service",
    "storage_root",
    "volume",
    "security_group",
    "shell",
    "schedule",
    "pool",
    "queue",
    "database_connection",
    "storage_connection",
    "secret",
    "secret_connection",
    "auth",
];

/// Reservations and tombstones keep generated identities with their parent.
/// Referencing a borrowed network or group does not create a ledger entry.
pub(super) fn ensure_independent(
    conn: &Connection,
    resource: &ManagedResource,
) -> Result<(), PersistenceError> {
    let generated: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM ai_sandbox_environment_resources WHERE kind=?1 AND id=?2)",
        params![resource.kind, resource.id],
        |row| row.get(0),
    )?;
    if generated {
        return Err(PersistenceError::IacParentManaged);
    }
    Ok(())
}

pub(super) fn validate(
    conn: &Connection,
    resource: &ManagedResource,
    adopting: bool,
) -> Result<(), PersistenceError> {
    if adopting {
        ensure_independent(conn, resource)?;
    }
    let exists = match resource.kind.as_str() {
        "storage_root" => "SELECT EXISTS(SELECT 1 FROM micro_storages WHERE id=?1)",
        "volume" => "SELECT EXISTS(SELECT 1 FROM micro_block_storages WHERE id=?1)",
        "security_group" => "SELECT EXISTS(SELECT 1 FROM security_groups WHERE id=?1)",
        "shell" => "SELECT EXISTS(SELECT 1 FROM shells WHERE id=?1)",
        "schedule" => "SELECT EXISTS(SELECT 1 FROM schedules WHERE id=?1)",
        "pool" => "SELECT EXISTS(SELECT 1 FROM vm_pools WHERE id=?1)",
        "queue" => {
            "SELECT EXISTS(SELECT 1 FROM application_queues WHERE id=?1 AND json_extract(record,'$.id')=?1)"
        }
        "database_connection" => "SELECT EXISTS(SELECT 1 FROM pipe_databases WHERE id=?1)",
        "storage_connection" => {
            "SELECT EXISTS(SELECT 1 FROM pipe_storage WHERE id=?1 AND json_extract(record,'$.id')=?1)"
        }
        "auth" => {
            "SELECT EXISTS(SELECT 1 FROM pipe_auth_credentials WHERE provider=?1 AND json_extract(credential,'$.provider')=?1)"
        }
        _ => return Err(PersistenceError::UnsupportedIacResource),
    };
    if adopting && !conn.query_row(exists, [&resource.id], |row| row.get::<_, bool>(0))? {
        return Err(PersistenceError::InvalidStackResource);
    }
    let busy = match resource.kind.as_str() {
        "storage_root" => Some(
            "SELECT EXISTS(SELECT 1 FROM micro_block_storages WHERE storage_root=?1 AND phase<>'ready')
             OR EXISTS(SELECT 1 FROM vms WHERE storage_root=?1 AND state IN ('starting','pausing','resuming','stopping'))",
        ),
        "volume" => Some(
            "SELECT EXISTS(SELECT 1 FROM micro_block_storages WHERE id=?1 AND phase<>'ready')
             OR EXISTS(SELECT 1 FROM micro_block_storages b JOIN vms v ON v.id=b.attached_vm_id
                       WHERE b.id=?1 AND (v.purpose<>'instance' OR v.state IN ('starting','pausing','resuming','stopping')))",
        ),
        "security_group" => Some(
            "SELECT EXISTS(SELECT 1 FROM vms WHERE security_group_id=?1 AND state IN ('starting','pausing','resuming','stopping'))",
        ),
        "schedule" => Some(
            "SELECT EXISTS(SELECT 1 FROM schedule_runs WHERE schedule_id=?1 AND status='running')",
        ),
        "pool" => Some(
            "SELECT EXISTS(SELECT 1 FROM vm_pools WHERE id=?1 AND deleting<>0)
             OR EXISTS(SELECT 1 FROM vm_pool_members WHERE pool_id=?1 AND state NOT IN ('ready','leased'))",
        ),
        "queue" => Some(
            "SELECT EXISTS(SELECT 1 FROM application_queues WHERE id=?1 AND COALESCE(json_extract(record,'$.phase'),'') NOT IN ('ready','failed'))
             OR EXISTS(SELECT 1 FROM queue_engine WHERE COALESCE(json_extract(record,'$.phase'),'') NOT IN ('running','stopped','failed'))",
        ),
        "database_connection" => return database_idle(conn, resource),
        // Connection edits hold the same service-mutation lock as ownership admission.
        "storage_connection" => None,
        // Native credential commits and ownership changes share the SQLite transaction.
        "auth" => None,
        // Adding a revision never changes scripts already pinned by a VM.
        "shell" => None,
        _ => return Err(PersistenceError::UnsupportedIacResource),
    };
    if let Some(query) = busy
        && conn.query_row(query, [&resource.id], |row| row.get::<_, bool>(0))?
    {
        return Err(PersistenceError::IacResourceBusy);
    }
    Ok(())
}

/// Maintenance may be attached to another saved alias for the same database.
fn database_idle(conn: &Connection, resource: &ManagedResource) -> Result<(), PersistenceError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT spec FROM pipe_databases WHERE id=?1",
            [&resource.id],
            |row| row.get(0),
        )
        .optional()?;
    let target = raw
        .map(|raw| {
            let input = serde_json::from_str::<crate::pipe_databases::DatabaseInput>(&raw)
                .map_err(|_| PersistenceError::IacOwnershipConflict)?;
            let record = crate::pipe_databases::DatabaseRecord {
                id: uuid::Uuid::parse_str(&resource.id)
                    .map_err(|_| PersistenceError::InvalidIacOwner)?,
                input,
            };
            crate::pipe_databases::maintenance::target_hash(&record)
                .map_err(|_| PersistenceError::IacOwnershipConflict)
        })
        .transpose()?;
    let mut statement=conn.prepare(
        "SELECT spec FROM database_jobs WHERE database_id=?1 OR json_extract(spec,'$.targetHash')=?2",
    )?;
    let jobs = statement.query_map(params![resource.id, target], |row| row.get::<_, String>(0))?;
    for raw in jobs {
        let job: crate::pipe_databases::maintenance::Job =
            serde_json::from_str(&raw?).map_err(|_| PersistenceError::IacOwnershipConflict)?;
        if !matches!(
            job.status.as_str(),
            "succeeded" | "failed" | "recovered" | "expired"
        ) {
            return Err(PersistenceError::IacResourceBusy);
        }
    }
    Ok(())
}

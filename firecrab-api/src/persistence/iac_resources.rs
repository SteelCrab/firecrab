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
    "registry",
    "ai_connection",
    "ai_model",
    "sandbox_environment",
    "sandbox_project",
    "stack_source",
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
        if resource.kind == "registry" {
            ensure_registry_direct(conn, &resource.id)?;
        }
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
        "registry" if resource.id == "docker.io" => {
            "SELECT EXISTS(SELECT 1 FROM docker_hub_credential WHERE id=1 AND ?1='docker.io')"
        }
        "registry" => "SELECT EXISTS(SELECT 1 FROM registry_credentials WHERE registry=?1)",
        "ai_connection" => {
            "SELECT EXISTS(SELECT 1 FROM pipe_ai_connections WHERE id=?1 AND json_extract(spec,'$.connection.id')=?1)"
        }
        "ai_model" => "SELECT EXISTS(SELECT 1 FROM pipe_ai_defaults WHERE project_id=?1)",
        "sandbox_environment" => {
            "SELECT EXISTS(SELECT 1 FROM ai_sandbox_environments WHERE id=?1 AND state<>'deleted' AND json_extract(spec,'$.id')=?1)"
        }
        "sandbox_project" => {
            "SELECT EXISTS(SELECT 1 FROM ai_sandbox_projects WHERE id=?1 AND json_extract(spec,'$.id')=?1)"
        }
        "stack_source" => {
            "SELECT EXISTS(SELECT 1 FROM stack_git_sources WHERE stack_id=?1 AND json_extract(status,'$.stackId')=?1)"
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
        "stack_source"=>{
            let id=uuid::Uuid::parse_str(&resource.id).map_err(|_|PersistenceError::InvalidIacOwner)?;
            super::stack_sources::ensure_idle(conn,id)?;
            return super::stack_applies::ensure_idle(conn,id);
        }
        "sandbox_environment" => Some(
            "SELECT EXISTS(SELECT 1 FROM ai_sandbox_environments WHERE id=?1 AND state IN ('provisioning','cleanup_pending'))
             OR EXISTS(SELECT 1 FROM ai_sandbox_environment_resources r JOIN vms v ON v.id=r.id
                       WHERE r.environment_id=?1 AND r.kind='vm' AND v.state IN ('starting','pausing','resuming','stopping'))
             OR EXISTS(SELECT 1 FROM ai_sandbox_tasks t JOIN ai_sandbox_task_index i ON i.id=t.id
                       WHERE json_extract(t.spec,'$.task.environment.id')=?1 AND i.busy=1)"
        ),
        "sandbox_project"=>Some("SELECT EXISTS(SELECT 1 FROM ai_sandbox_task_index WHERE project_id=?1 AND busy=1)
             OR EXISTS(SELECT 1 FROM ai_sandbox_environments WHERE project_id=?1 AND state IN ('provisioning','cleanup_pending'))"),
        // Connection edits hold the same service-mutation lock as ownership admission.
        "storage_connection" => None,
        // Native credential commits and ownership changes share the SQLite transaction.
        "auth" | "registry" | "ai_connection" | "ai_model" => None,
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

/// Shared Auth owns its derived connection; the dormant direct row is not effective configuration.
pub(super) fn ensure_registry_direct(
    conn: &Connection,
    registry: &str,
) -> Result<(), PersistenceError> {
    let mut statement = conn.prepare("SELECT credential FROM pipe_auth_credentials")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        let auth: crate::pipe_auth::SavedAuth =
            serde_json::from_str(&row?).map_err(|_| PersistenceError::IacOwnershipConflict)?;
        if auth.connection().registry.as_deref() == Some(registry) {
            return Err(PersistenceError::IacParentManaged);
        }
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

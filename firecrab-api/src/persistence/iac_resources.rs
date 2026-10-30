//! Resource existence and controller activity checks for ownership transitions.

use firecrab_stack::ManagedResource;
use rusqlite::Connection;

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
];

pub(super) fn validate(
    conn: &Connection,
    resource: &ManagedResource,
    adopting: bool,
) -> Result<(), PersistenceError> {
    let exists = match resource.kind.as_str() {
        "storage_root" => "SELECT EXISTS(SELECT 1 FROM micro_storages WHERE id=?1)",
        "volume" => "SELECT EXISTS(SELECT 1 FROM micro_block_storages WHERE id=?1)",
        "security_group" => "SELECT EXISTS(SELECT 1 FROM security_groups WHERE id=?1)",
        "shell" => "SELECT EXISTS(SELECT 1 FROM shells WHERE id=?1)",
        "schedule" => "SELECT EXISTS(SELECT 1 FROM schedules WHERE id=?1)",
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

//! Durable offline project deletion records and fail-closed startup fencing.
use super::PersistenceError;
use rusqlite::Connection;

/// Shared with the offline deletion command. Completed records are permanent tombstones.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS project_workspace_deletions(
 id TEXT PRIMARY KEY, project_id TEXT NOT NULL UNIQUE,
 phase TEXT NOT NULL CHECK(phase IN ('staging','prepared','erasing','finishing','complete')),
 document TEXT NOT NULL
) STRICT;
";

/// An interrupted deletion must finish before any workload is allowed to start.
pub(super) fn require_complete(conn: &Connection) -> Result<(), PersistenceError> {
    let pending: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM project_workspace_deletions WHERE phase!='complete')",
        [],
        |row| row.get(0),
    )?;
    if pending {
        return Err(PersistenceError::BlockStorageConflict {
            message: "Offline project deletion is incomplete; resume its saved operation while the controller is stopped",
        });
    }
    Ok(())
}

//! Offline imports block startup until their durable file installation and registration complete.
use super::{PersistenceError, Store};
use rusqlite::Connection;
use uuid::Uuid;

/// Shared with the offline importer; historical task IDs remain read-only after cross-host recovery.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS project_workspace_imports(id TEXT PRIMARY KEY,project_id TEXT NOT NULL,phase TEXT NOT NULL CHECK(phase IN ('staging','prepared','complete')),document TEXT NOT NULL) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS project_workspace_import_active ON project_workspace_imports(project_id) WHERE phase!='complete';
CREATE TABLE IF NOT EXISTS project_workspace_import_tasks(task_id TEXT PRIMARY KEY,operation_id TEXT NOT NULL REFERENCES project_workspace_imports(id)) STRICT;
";
/// Incomplete offline imports must be resumed by their operator before serving any workload.
pub(super) fn require_complete(connection: &Connection) -> Result<(), PersistenceError> {
    let pending: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM project_workspace_imports WHERE phase!='complete')",
        [],
        |row| row.get(0),
    )?;
    if pending {
        return Err(PersistenceError::BlockStorageConflict {
            message: "Offline project import is incomplete; stop the controller and resume its import operation",
        });
    }
    Ok(())
}
impl Store {
    /// Imported attempts are immutable history; new execution requires a new task identity.
    pub(crate) fn imported_sandbox_task(&self, task: Uuid) -> Result<bool, PersistenceError> {
        Ok(self.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM project_workspace_import_tasks WHERE task_id=?1)",
            [task.to_string()],
            |row| row.get(0),
        )?)
    }
}

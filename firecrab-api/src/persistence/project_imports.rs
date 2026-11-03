//! Offline imports block startup until their durable file installation and registration complete.
use super::{PersistenceError, Store};
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;
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
            message: "Offline project import is incomplete; inspect import-status and resume or abandon staging while the controller is stopped",
        });
    }
    Ok(())
}
impl Store {
    /// Return the destination task ordering for the original imported checkpoint.
    pub(crate) fn imported_checkpoint_ordinals(
        &self,
        project: Uuid,
        checkpoint: Uuid,
    ) -> Result<Option<HashMap<String, i64>>, PersistenceError> {
        let conn = self.lock();
        let raw: Option<String> = conn.query_row(
            "SELECT json_extract(document,'$.taskOrdinals') FROM project_workspace_imports WHERE project_id=?1 AND phase='complete' AND COALESCE(json_extract(document,'$.outcome'),'imported')='imported' AND json_extract(document,'$.checkpoint')=?2",
            [project.to_string(), checkpoint.to_string()],
            |row| row.get(0),
        ).optional()?.flatten();
        let Some(raw) = raw else { return Ok(None) };
        let ordinals: HashMap<String, i64> =
            serde_json::from_str(&raw).map_err(|_| PersistenceError::BlockStorageConflict {
                message: "Imported task ordering is corrupt",
            })?;
        for (id, ordinal) in &ordinals {
            let actual: Option<i64> = conn
                .query_row(
                    "SELECT ordinal FROM sandbox_task_catalog WHERE project_id=?1 AND id=?2",
                    rusqlite::params![project.to_string(), id],
                    |row| row.get(0),
                )
                .optional()?;
            if *ordinal <= 0 || actual != Some(*ordinal) {
                return Err(PersistenceError::BlockStorageConflict {
                    message: "Imported task ordering differs from the destination catalog",
                });
            }
        }
        Ok(Some(ordinals))
    }
    /// Reject an older imported checkpoint that would discard newer project task records.
    pub(crate) fn require_checkpoint_tasks(
        &self,
        project: Uuid,
        ids: &std::collections::HashSet<String>,
    ) -> Result<(), PersistenceError> {
        let conn = self.lock();
        let mut query =
            conn.prepare("SELECT id FROM sandbox_task_catalog WHERE project_id=?1 AND ready=1")?;
        let rows = query.query_map([project.to_string()], |row| row.get::<_, String>(0))?;
        for id in rows {
            if !ids.contains(&id?) {
                return Err(PersistenceError::BlockStorageConflict {
                    message: "Imported checkpoint predates destination task history; use a newer checkpoint",
                });
            }
        }
        Ok(())
    }
    /// Imported attempts are immutable history; new execution requires a new task identity.
    pub(crate) fn imported_sandbox_task(&self, task: Uuid) -> Result<bool, PersistenceError> {
        Ok(self.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM project_workspace_import_tasks WHERE task_id=?1)",
            [task.to_string()],
            |row| row.get(0),
        )?)
    }
}

//! Durable identities for an explicitly requested Canvas workspace provision.
use super::{
    PersistenceError, Store,
    ai_sandbox::{decode, encode},
    sandbox_project_db::Durable,
};
use crate::project_workspace::provision::Record;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS project_workspace_provisions (
 id TEXT PRIMARY KEY, project_id TEXT NOT NULL UNIQUE, vm_id TEXT NOT NULL UNIQUE,
 volume_id TEXT NOT NULL UNIQUE, document TEXT NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS project_workspace_provision_tombstones (id TEXT PRIMARY KEY) STRICT;";

impl Store {
    pub(crate) fn canvas_provision(&self, id: Uuid) -> Result<Option<Record>, PersistenceError> {
        let row: Option<(String, String, String, String)> = self
            .lock()
            .query_row(
                "SELECT project_id,vm_id,volume_id,document FROM project_workspace_provisions WHERE id=?1",
                [id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((project, vm, volume, raw)) = row else {
            return Ok(None);
        };
        let record: Record = decode(raw)?;
        if record.id != id
            || record.project_id.to_string() != project
            || record.vm_id.to_string() != vm
            || record.volume_id.to_string() != volume
        {
            return Err(PersistenceError::BlockStorageConflict {
                message: "Canvas provision identity is corrupt",
            });
        }
        Ok(Some(record))
    }

    pub(crate) fn reserve_canvas_provision(&self, record: &Record) -> Result<(), PersistenceError> {
        let c = self.lock();
        let _durable = Durable::new(&c)?;
        let changed = c.execute(
            "INSERT INTO project_workspace_provisions(id,project_id,vm_id,volume_id,document) SELECT ?1,?2,?3,?4,?5 WHERE NOT EXISTS(SELECT 1 FROM project_workspace_provision_tombstones WHERE id=?1)",
            params![record.id.to_string(), record.project_id.to_string(), record.vm_id.to_string(), record.volume_id.to_string(), encode(record)?],
        )?;
        if changed != 1 {
            return Err(PersistenceError::BlockStorageConflict {
                message: "Canvas provision identity is retired",
            });
        }
        Ok(())
    }

    pub(crate) fn save_canvas_provision(&self, record: &Record) -> Result<(), PersistenceError> {
        let c = self.lock();
        let _durable = Durable::new(&c)?;
        let changed = c.execute(
            "UPDATE project_workspace_provisions SET document=?5 WHERE id=?1 AND project_id=?2 AND vm_id=?3 AND volume_id=?4",
            params![record.id.to_string(), record.project_id.to_string(), record.vm_id.to_string(), record.volume_id.to_string(), encode(record)?],
        )?;
        if changed != 1 {
            return Err(PersistenceError::BlockStorageConflict {
                message: "Canvas provision identity changed",
            });
        }
        Ok(())
    }
}

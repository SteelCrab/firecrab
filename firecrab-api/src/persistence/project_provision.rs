//! Durable identities for an explicitly requested Canvas workspace provision.
use super::{
    PersistenceError, Store,
    ai_sandbox::{decode, encode},
    sandbox_project_db::Durable,
};
use crate::project_workspace::provision::{CreationRecord, Record};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS project_workspace_provisions (
 id TEXT PRIMARY KEY, project_id TEXT NOT NULL UNIQUE, vm_id TEXT NOT NULL UNIQUE,
 volume_id TEXT NOT NULL UNIQUE, document TEXT NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS project_workspace_provision_tombstones (id TEXT PRIMARY KEY) STRICT;
CREATE TABLE IF NOT EXISTS project_workspace_vm_creations (id TEXT PRIMARY KEY, document TEXT NOT NULL) STRICT;
CREATE TRIGGER IF NOT EXISTS retire_canvas_vm_on_delete AFTER DELETE ON vms
 WHEN EXISTS(SELECT 1 FROM project_workspace_vm_creations WHERE id=OLD.id)
 BEGIN INSERT OR IGNORE INTO project_workspace_provision_tombstones(id) VALUES(OLD.id); END;
CREATE TRIGGER IF NOT EXISTS fence_retired_canvas_vm BEFORE INSERT ON vms
 WHEN EXISTS(SELECT 1 FROM project_workspace_provision_tombstones WHERE id=NEW.id)
 BEGIN SELECT RAISE(ABORT,'Canvas VM identity is retired'); END;";

impl Store {
    pub(crate) fn canvas_creation_retired(&self, id: Uuid) -> Result<bool, PersistenceError> {
        Ok(self.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM project_workspace_provision_tombstones WHERE id=?1)",
            [id.to_string()],
            |row| row.get(0),
        )?)
    }
    pub(crate) fn canvas_vm_row_exists(&self, id: Uuid) -> Result<bool, PersistenceError> {
        Ok(self.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM vms WHERE id=?1)",
            [id.to_string()],
            |row| row.get(0),
        )?)
    }

    pub(crate) fn canvas_vm_creation(
        &self,
        id: Uuid,
    ) -> Result<Option<CreationRecord>, PersistenceError> {
        let raw: Option<String> = self
            .lock()
            .query_row(
                "SELECT document FROM project_workspace_vm_creations WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(raw) = raw else { return Ok(None) };
        let record: CreationRecord = decode(raw)?;
        if record.id != id {
            return Err(PersistenceError::BlockStorageConflict {
                message: "Canvas VM creation identity is corrupt",
            });
        }
        Ok(Some(record))
    }

    pub(crate) fn reserve_canvas_vm_creation(
        &self,
        record: &CreationRecord,
    ) -> Result<(), PersistenceError> {
        let c = self.lock();
        let _durable = Durable::new(&c)?;
        let changed = c.execute(
            "INSERT INTO project_workspace_vm_creations(id,document) SELECT ?1,?2 WHERE NOT EXISTS(SELECT 1 FROM project_workspace_provision_tombstones WHERE id=?1) AND NOT EXISTS(SELECT 1 FROM vms WHERE id=?1) AND NOT EXISTS(SELECT 1 FROM project_workspace_provisions WHERE id=?1)",
            params![record.id.to_string(), encode(record)?],
        )?;
        if changed != 1 {
            return Err(PersistenceError::BlockStorageConflict {
                message: "Canvas VM creation identity is unavailable",
            });
        }
        Ok(())
    }

    pub(crate) fn save_canvas_vm_creation(
        &self,
        record: &CreationRecord,
    ) -> Result<(), PersistenceError> {
        let c = self.lock();
        let _durable = Durable::new(&c)?;
        if c.execute(
            "UPDATE project_workspace_vm_creations SET document=?2 WHERE id=?1",
            params![record.id.to_string(), encode(record)?],
        )? != 1
        {
            return Err(PersistenceError::BlockStorageConflict {
                message: "Canvas VM creation identity changed",
            });
        }
        Ok(())
    }

    /// Retirement prevents delayed requests from creating a VM after cancellation.
    pub(crate) fn retire_canvas_vm_creation(&self, id: Uuid) -> Result<bool, PersistenceError> {
        let c = self.lock();
        let _durable = Durable::new(&c)?;
        let tx = c.unchecked_transaction()?;
        let used: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM vms WHERE id=?1) OR EXISTS(SELECT 1 FROM project_workspace_provisions WHERE id=?1)",
            [id.to_string()], |row| row.get(0),
        )?;
        if used {
            return Ok(false);
        }
        tx.execute(
            "INSERT OR IGNORE INTO project_workspace_provision_tombstones(id) VALUES(?1)",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM project_workspace_vm_creations WHERE id=?1",
            [id.to_string()],
        )?;
        tx.commit()?;
        Ok(true)
    }
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

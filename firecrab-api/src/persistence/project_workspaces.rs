//! Common control records own workspace resource bindings; project documents live in private DBs.
use super::{PersistenceError, Store};
use crate::project_workspace::Binding;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

/// Retained resource registrations are independent of transient VM and browser lifetimes.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS project_workspaces (
 project_id TEXT PRIMARY KEY, database_id TEXT NOT NULL UNIQUE, volume_id TEXT NOT NULL UNIQUE,
 vm_id TEXT NOT NULL UNIQUE, generation INTEGER NOT NULL CHECK(generation>0),
 provisioning INTEGER NOT NULL CHECK(provisioning IN (0,1)), verified_runtime_id TEXT
) STRICT;
";
/// Return a common conflict without exposing another project identity.
fn conflict() -> PersistenceError {
    PersistenceError::BlockStorageConflict {
        message: "workspace binding changed or its resources belong to another project",
    }
}
/// Decode a reservation and reject invalid persisted identities.
fn read(conn: &rusqlite::Connection, project: Uuid) -> Result<Option<Binding>, PersistenceError> {
    let mut statement = conn.prepare(
        "SELECT database_id,volume_id,vm_id,generation,provisioning,verified_runtime_id
         FROM project_workspaces WHERE project_id=?1",
    )?;
    let row = statement
        .query_row([project.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, bool>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })
        .optional()?;
    row.map(
        |(database, volume, vm, generation, provisioning, runtime)| {
            Ok(Binding {
                project_id: project,
                database_id: Uuid::parse_str(&database).map_err(|_| conflict())?,
                volume_id: Uuid::parse_str(&volume).map_err(|_| conflict())?,
                vm_id: Uuid::parse_str(&vm).map_err(|_| conflict())?,
                generation: u64::try_from(generation).map_err(|_| conflict())?,
                provisioning,
                verified_runtime_id: runtime
                    .map(|s| Uuid::parse_str(&s).map_err(|_| conflict()))
                    .transpose()?,
            })
        },
    )
    .transpose()
}
impl Store {
    /// Read a project reservation without creating private state.
    pub(crate) fn project_workspace(
        &self,
        project: Uuid,
    ) -> Result<Option<Binding>, PersistenceError> {
        read(&self.lock(), project)
    }
    /// Find the retained project owner before changing a volume attachment.
    pub(crate) fn workspace_volume_owner(
        &self,
        volume: Uuid,
    ) -> Result<Option<Binding>, PersistenceError> {
        let project: Option<String> = self
            .lock()
            .query_row(
                "SELECT project_id FROM project_workspaces WHERE volume_id=?1",
                [volume.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        project
            .map(|p| {
                self.project_workspace(Uuid::parse_str(&p).map_err(|_| conflict())?)?
                    .ok_or_else(conflict)
            })
            .transpose()
    }
    /// Reserve completed storage atomically and fence stale runtime replacements.
    pub(crate) fn reserve_project_workspace(
        &self,
        project: Uuid,
        volume: Uuid,
        vm: Uuid,
        expected: Option<u64>,
    ) -> Result<Binding, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let ready: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM micro_block_storages WHERE id=?1 AND phase='ready')",
            [volume.to_string()],
            |r| r.get(0),
        )?;
        if !ready {
            return Err(conflict());
        }
        let other: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM project_workspaces
             WHERE project_id!=?1 AND (volume_id=?2 OR vm_id=?3))",
            params![project.to_string(), volume.to_string(), vm.to_string()],
            |r| r.get(0),
        )?;
        if other {
            return Err(conflict());
        }
        if let Some(previous) = read(&tx, project)? {
            if previous.volume_id != volume {
                return Err(conflict());
            }
            if previous.vm_id == vm {
                return Ok(previous);
            }
            if expected != Some(previous.generation) || previous.generation >= i64::MAX as u64 {
                return Err(conflict());
            }
            tx.execute(
                "UPDATE project_workspaces SET vm_id=?2,generation=generation+1,verified_runtime_id=NULL
                 WHERE project_id=?1",
                params![project.to_string(), vm.to_string()],
            )?;
        } else {
            if expected.is_some_and(|n| n != 0) {
                return Err(conflict());
            }
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM ai_sandbox_projects WHERE id=?1)",
                [project.to_string()],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(conflict());
            }
            tx.execute(
                "INSERT INTO project_workspaces VALUES(?1,?2,?3,?4,1,1,NULL)",
                params![
                    project.to_string(),
                    Uuid::new_v4().to_string(),
                    volume.to_string(),
                    vm.to_string()
                ],
            )?;
        }
        let result = read(&tx, project)?.ok_or_else(conflict)?;
        tx.commit()?;
        Ok(result)
    }
    /// Commit initial provisioning only for the still-current resource binding.
    pub(crate) fn finish_project_workspace(
        &self,
        binding: &Binding,
    ) -> Result<(), PersistenceError> {
        let changed = self.lock().execute(
            "UPDATE project_workspaces SET provisioning=0
             WHERE project_id=?1 AND generation=?2 AND volume_id=?3 AND vm_id=?4",
            params![
                binding.project_id.to_string(),
                i64::try_from(binding.generation).map_err(|_| conflict())?,
                binding.volume_id.to_string(),
                binding.vm_id.to_string()
            ],
        )?;
        if changed != 1 {
            return Err(conflict());
        }
        Ok(())
    }
    /// Associate an accepted mount proof with this binding generation.
    pub(crate) fn verify_project_workspace(
        &self,
        binding: &Binding,
        runtime: Uuid,
    ) -> Result<(), PersistenceError> {
        let changed = self.lock().execute(
            "UPDATE project_workspaces SET verified_runtime_id=?3
             WHERE project_id=?1 AND generation=?2 AND provisioning=0",
            params![
                binding.project_id.to_string(),
                i64::try_from(binding.generation).map_err(|_| conflict())?,
                runtime.to_string()
            ],
        )?;
        if changed != 1 {
            return Err(conflict());
        }
        Ok(())
    }
    /// Release the old attachment only while its complete reservation still matches.
    pub(crate) fn detach_workspace_volume(
        &self,
        binding: &Binding,
        old_vm: Uuid,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        if read(&tx, binding.project_id)?.as_ref() != Some(binding) {
            return Err(conflict());
        }
        tx.execute(
            "UPDATE micro_block_storages SET attached_vm_id=NULL,mount_path=NULL
             WHERE id=?1 AND attached_vm_id=?2",
            params![binding.volume_id.to_string(), old_vm.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
}

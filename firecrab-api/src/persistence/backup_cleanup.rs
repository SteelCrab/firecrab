//! Retention state, immutable deletion plans and retained tombstones in the control DB.
use super::{
    PersistenceError, Store,
    ai_sandbox::{decode, encode},
    sandbox_project_db::Durable,
};
use crate::project_workspace::backups::{Record as Backup, cleanup::Record};
use firecrab_api_types::{ProjectBackupCleanupOperation, ProjectBackupRetention};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

/// Additive control tables retain policies and deletion evidence across project-DB restoration.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS project_backup_retention(project_id TEXT PRIMARY KEY,document TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS project_backup_cleanups(id TEXT PRIMARY KEY,project_id TEXT NOT NULL,active INTEGER NOT NULL CHECK(active IN (0,1)),document TEXT NOT NULL) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS project_cleanup_active ON project_backup_cleanups(project_id) WHERE active=1;
";
/// Sanitize identity/precondition failures without exposing private paths or SQLite details.
fn conflict() -> PersistenceError {
    PersistenceError::BlockStorageConflict {
        message: "Backup cleanup identity, generation or protection changed",
    }
}
impl Store {
    /// Read the policy without creating state; automatic deletion defaults to disabled.
    pub(crate) fn backup_retention(
        &self,
        project: Uuid,
    ) -> Result<ProjectBackupRetention, PersistenceError> {
        let raw: Option<String> = self
            .lock()
            .query_row(
                "SELECT document FROM project_backup_retention WHERE project_id=?1",
                [project.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        raw.map(decode).transpose().map(Option::unwrap_or_default)
    }
    /// Atomically acknowledge the current binding generation with the operator policy edit.
    pub(crate) fn save_backup_retention(
        &self,
        project: Uuid,
        generation: u64,
        policy: &ProjectBackupRetention,
    ) -> Result<(), PersistenceError> {
        let c = self.lock();
        let _durable = Durable::new(&c)?;
        let tx = c.unchecked_transaction()?;
        let matches: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM project_workspaces WHERE project_id=?1 AND generation=?2 AND provisioning=0)",params![project.to_string(),i64::try_from(generation).map_err(|_|conflict())?],|r|r.get(0))?;
        if !matches {
            return Err(conflict());
        }
        tx.execute("INSERT INTO project_backup_retention VALUES(?1,?2) ON CONFLICT(project_id) DO UPDATE SET document=excluded.document",params![project.to_string(),encode(policy)?])?;
        tx.commit()?;
        Ok(())
    }
    /// Compare the saved policy so an old worker cannot overwrite a newer operator edit.
    pub(crate) fn finish_retention_attempt(
        &self,
        project: Uuid,
        expected: &ProjectBackupRetention,
        next: &ProjectBackupRetention,
    ) -> Result<(), PersistenceError> {
        let c = self.lock();
        let _durable = Durable::new(&c)?;
        c.execute(
            "UPDATE project_backup_retention SET document=?3 WHERE project_id=?1 AND document=?2",
            params![project.to_string(), encode(expected)?, encode(next)?],
        )?;
        Ok(())
    }
    /// Enumerate only projects with explicitly saved retention configuration.
    pub(crate) fn retention_projects(&self) -> Result<Vec<Uuid>, PersistenceError> {
        let c = self.lock();
        let mut q =
            c.prepare("SELECT project_id FROM project_backup_retention ORDER BY project_id")?;
        q.query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Uuid::parse_str(&r?).map_err(|_| conflict()))
            .collect()
    }
    /// Read a private exact-ID plan and verify the retained receipt identity.
    pub(crate) fn cleanup_record(&self, id: Uuid) -> Result<Option<Record>, PersistenceError> {
        let raw: Option<String> = self
            .lock()
            .query_row(
                "SELECT document FROM project_backup_cleanups WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        raw.map(|raw| {
            let r: Record = decode(raw)?;
            if r.public.id != id {
                return Err(conflict());
            }
            Ok(r)
        })
        .transpose()
    }
    /// Load project audit records, or only restartable active operations when no project is selected.
    pub(crate) fn cleanup_records(
        &self,
        project: Option<Uuid>,
    ) -> Result<Vec<Record>, PersistenceError> {
        let c = self.lock();
        let mut q=c.prepare("SELECT document FROM project_backup_cleanups WHERE project_id=?1 OR (?1 IS NULL AND active=1 AND json_extract(document,'$.public.error') IS NULL) ORDER BY rowid DESC LIMIT 4096")?;
        q.query_map([project.map(|p| p.to_string())], |r| r.get::<_, String>(0))?
            .map(|r| decode(r?))
            .collect()
    }
    /// Project history exposes only public receipts, never the potentially large private file plan.
    pub(crate) fn cleanup_history(
        &self,
        project: Uuid,
    ) -> Result<Vec<ProjectBackupCleanupOperation>, PersistenceError> {
        let c = self.lock();
        let mut q=c.prepare("SELECT json_extract(document,'$.public'),active FROM project_backup_cleanups WHERE project_id=?1 ORDER BY active DESC,rowid DESC LIMIT 100")?;
        q.query_map([project.to_string()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))
        })?
        .map(|r| {
            let (raw, active) = r?;
            let mut op: ProjectBackupCleanupOperation = decode(raw)?;
            op.pending_reconciliation = active;
            Ok(op)
        })
        .collect()
    }
    /// Fence VM lifecycle changes until filesystem and registry cleanup both complete.
    pub(crate) fn workspace_cleanup_active(&self, project: Uuid) -> Result<bool, PersistenceError> {
        self.lock().query_row("SELECT EXISTS(SELECT 1 FROM project_backup_cleanups WHERE project_id=?1 AND active=1)",[project.to_string()],|r|r.get(0)).map_err(Into::into)
    }
    /// Advance progress or error state without replacing the admitted binding, action or selection.
    pub(crate) fn save_cleanup_record(&self, record: &Record) -> Result<(), PersistenceError> {
        let old = self
            .cleanup_record(record.public.id)?
            .ok_or_else(conflict)?;
        if old.binding != record.binding
            || old.public.request != record.public.request
            || old.public.project_id != record.public.project_id
            || old.public.automatic != record.public.automatic
        {
            return Err(conflict());
        }
        let c = self.lock();
        let _durable = Durable::new(&c)?;
        c.execute(
            "UPDATE project_backup_cleanups SET active=?2,document=?3 WHERE id=?1",
            params![record.public.id.to_string(), record.active, encode(record)?],
        )?;
        Ok(())
    }
    /// Intent and all target tombstones commit before the first filesystem deletion.
    pub(crate) fn admit_backup_cleanup(&self, record: &Record) -> Result<(), PersistenceError> {
        let c = self.lock();
        let _durable = Durable::new(&c)?;
        let tx = c.unchecked_transaction()?;
        let blocked:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM project_backup_cleanups WHERE project_id=?1 AND active=1) OR EXISTS(SELECT 1 FROM project_backup_operations WHERE project_id=?1 AND active=1) OR EXISTS(SELECT 1 FROM project_backup_operations WHERE id=?2)",params![record.public.project_id.to_string(),record.public.id.to_string()],|r|r.get(0))?;
        if blocked || !record.active || record.public.id.is_nil() {
            return Err(conflict());
        }
        let matches:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM project_workspaces WHERE project_id=?1 AND generation=?2 AND database_id=?3 AND volume_id=?4 AND vm_id=?5 AND provisioning=0)",params![record.binding.project_id.to_string(),i64::try_from(record.binding.generation).map_err(|_|conflict())?,record.binding.database_id.to_string(),record.binding.volume_id.to_string(),record.binding.vm_id.to_string()],|r|r.get(0))?;
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM project_backup_cleanups WHERE project_id=?1",
            [record.binding.project_id.to_string()],
            |r| r.get(0),
        )?;
        if !matches || count >= 4096 || encode(record)?.len() > 1024 * 1024 {
            return Err(conflict());
        }
        tx.execute(
            "INSERT INTO project_backup_cleanups VALUES(?1,?2,1,?3)",
            params![
                record.public.id.to_string(),
                record.public.project_id.to_string(),
                encode(record)?
            ],
        )?;
        for id in &record.public.request.target_ids {
            let raw: String = tx.query_row(
                "SELECT document FROM project_backup_operations WHERE id=?1 AND project_id=?2",
                params![id.to_string(), record.public.project_id.to_string()],
                |r| r.get(0),
            )?;
            let mut target: Backup = decode(raw)?;
            if target.active
                || target.public.protected
                || target.public.cleanup_id.is_some()
                || !matches!(target.public.phase.as_str(), "complete" | "aborted")
            {
                return Err(conflict());
            }
            target.public.cleanup_id = Some(record.public.id);
            tx.execute(
                "UPDATE project_backup_operations SET document=?2 WHERE id=?1",
                params![id.to_string(), encode(&target)?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Mark target bytes cleaned while retaining manifests, receipts and the active maintenance gate.
    pub(crate) fn complete_backup_cleanup(
        &self,
        record: &mut Record,
    ) -> Result<(), PersistenceError> {
        let c = self.lock();
        let _durable = Durable::new(&c)?;
        let tx = c.unchecked_transaction()?;
        let now = crate::ai_sandbox::now();
        for id in &record.public.request.target_ids {
            let raw: String = tx.query_row(
                "SELECT document FROM project_backup_operations WHERE id=?1 AND project_id=?2",
                params![id.to_string(), record.public.project_id.to_string()],
                |r| r.get(0),
            )?;
            let mut target: Backup = decode(raw)?;
            if target.public.cleanup_id != Some(record.public.id) || target.active {
                return Err(conflict());
            }
            target.public.cleaned_at.get_or_insert(now);
            tx.execute(
                "UPDATE project_backup_operations SET document=?2 WHERE id=?1",
                params![id.to_string(), encode(&target)?],
            )?;
        }
        let mut next = record.clone();
        next.public.phase = "complete".into();
        next.public.error = None;
        next.public.updated_at = now;
        if tx.execute(
            "UPDATE project_backup_cleanups SET document=?2 WHERE id=?1 AND active=1",
            params![record.public.id.to_string(), encode(&next)?],
        )? != 1
        {
            return Err(conflict());
        }
        tx.commit()?;
        *record = next;
        Ok(())
    }
}

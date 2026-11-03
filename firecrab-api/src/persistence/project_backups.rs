//! Durable backup intents live in the control database, outside restored project state.
use super::{
    PersistenceError, Store,
    ai_sandbox::{decode, encode},
    sandbox_project_db::Durable,
};
use crate::project_workspace::{Binding, backups::Record};
use firecrab_api_types::ProjectBackupPolicy;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

pub(super) const SCHEMA:&str="
CREATE TABLE IF NOT EXISTS project_backup_policies(project_id TEXT PRIMARY KEY,document TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS project_backup_operations(id TEXT PRIMARY KEY,project_id TEXT NOT NULL,active INTEGER NOT NULL CHECK(active IN (0,1)),document TEXT NOT NULL) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS project_backup_active ON project_backup_operations(project_id) WHERE active=1;
";
fn conflict() -> PersistenceError {
    PersistenceError::BlockStorageConflict {
        message: "Project backup identity or binding changed",
    }
}
impl Store {
    pub(crate) fn backup_policy(
        &self,
        project: Uuid,
    ) -> Result<ProjectBackupPolicy, PersistenceError> {
        let raw: Option<String> = self
            .lock()
            .query_row(
                "SELECT document FROM project_backup_policies WHERE project_id=?1",
                [project.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(match raw {
            Some(raw) => decode(raw)?,
            None => ProjectBackupPolicy {
                enabled: false,
                interval_seconds: 3600,
                next_run_at: 0,
                last_checkpoint: None,
                error: None,
            },
        })
    }
    pub(crate) fn save_backup_policy(
        &self,
        project: Uuid,
        policy: &ProjectBackupPolicy,
    ) -> Result<(), PersistenceError> {
        let c = self.lock();
        let _durable = Durable::new(&c)?;
        c.execute("INSERT INTO project_backup_policies VALUES(?1,?2) ON CONFLICT(project_id) DO UPDATE SET document=excluded.document",params![project.to_string(),encode(policy)?])?;
        Ok(())
    }
    /// A schedule edit during a large copy takes precedence over the older attempt.
    pub(crate) fn finish_backup_policy_attempt(
        &self,
        project: Uuid,
        expected: &ProjectBackupPolicy,
        next: &ProjectBackupPolicy,
    ) -> Result<(), PersistenceError> {
        let c = self.lock();
        let _durable = Durable::new(&c)?;
        c.execute(
            "UPDATE project_backup_policies SET document=?3 WHERE project_id=?1 AND document=?2",
            params![project.to_string(), encode(expected)?, encode(next)?],
        )?;
        Ok(())
    }
    pub(crate) fn backup_record(&self, id: Uuid) -> Result<Option<Record>, PersistenceError> {
        let raw: Option<String> = self
            .lock()
            .query_row(
                "SELECT document FROM project_backup_operations WHERE id=?1",
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
    pub(crate) fn backup_records(
        &self,
        project: Option<Uuid>,
    ) -> Result<Vec<Record>, PersistenceError> {
        let c = self.lock();
        let mut q=c.prepare("SELECT document FROM project_backup_operations WHERE (project_id=?1 OR (?1 IS NULL AND (active=1 OR json_extract(document,'$.public.phase')='pending') AND json_extract(document,'$.public.error') IS NULL)) ORDER BY rowid DESC LIMIT 4096")?;
        q.query_map([project.map(|p| p.to_string())], |r| r.get::<_, String>(0))?
            .map(|r| decode(r?))
            .collect()
    }
    pub(crate) fn save_backup_record(&self, record: &Record) -> Result<(), PersistenceError> {
        if record.public.project_id != record.binding.project_id
            || record.public.request.expected_generation != record.binding.generation
            || record.public.id.is_nil()
        {
            return Err(conflict());
        }
        if let Some(old) = self.backup_record(record.public.id)?
            && (old.binding != record.binding
                || old.public.project_id != record.public.project_id
                || old.public.request != record.public.request
                || old.caller.principal != record.caller.principal
                || old.public.cleanup_id != record.public.cleanup_id
                || old.public.cleaned_at != record.public.cleaned_at)
        {
            return Err(conflict());
        }
        if self.cleanup_record(record.public.id)?.is_some() {
            return Err(conflict());
        }
        let json = encode(record)?;
        if json.len() > 16384 {
            return Err(conflict());
        }
        let c = self.lock();
        let _durable = Durable::new(&c)?;
        let count: i64 = c.query_row(
            "SELECT count(*) FROM project_backup_operations WHERE project_id=?1",
            [record.public.project_id.to_string()],
            |r| r.get(0),
        )?;
        let exists: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM project_backup_operations WHERE id=?1 AND project_id=?2)",
            params![
                record.public.id.to_string(),
                record.public.project_id.to_string()
            ],
            |r| r.get(0),
        )?;
        if count >= 4096 && !exists {
            return Err(conflict());
        }
        let changed=c.execute("INSERT INTO project_backup_operations VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET active=excluded.active,document=excluded.document WHERE project_id=excluded.project_id",params![record.public.id.to_string(),record.public.project_id.to_string(),record.active,json])?;
        if changed != 1 {
            return Err(conflict());
        }
        Ok(())
    }
    pub(crate) fn backup_projects(&self) -> Result<Vec<Uuid>, PersistenceError> {
        let c = self.lock();
        let mut q = c.prepare("SELECT project_id FROM project_backup_policies")?;
        q.query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Uuid::parse_str(&r?).map_err(|_| conflict()))
            .collect()
    }
    pub(crate) fn workspace_generation(
        &self,
        project: Uuid,
    ) -> Result<Option<(u64, bool)>, PersistenceError> {
        self.lock().query_row("SELECT generation,EXISTS(SELECT 1 FROM project_backup_operations o WHERE o.project_id=w.project_id AND json_extract(o.document,'$.public.restoredGeneration') IS NOT NULL) FROM project_workspaces w WHERE project_id=?1",[project.to_string()],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,bool>(1)?))).optional()?.map(|(g,r)|Ok((u64::try_from(g).map_err(|_|conflict())?,r))).transpose()
    }
    pub(crate) fn workspace_backup_active(&self, project: Uuid) -> Result<bool, PersistenceError> {
        self.lock().query_row("SELECT EXISTS(SELECT 1 FROM project_backup_operations WHERE project_id=?1 AND active=1)",[project.to_string()],|r|r.get(0)).map_err(Into::into)
    }
    // Controller task storage stays authoritative even when its unrelated project DB is lost.
    pub(crate) fn workspace_backup_busy(
        &self,
        project: Uuid,
        restore: bool,
    ) -> Result<bool, PersistenceError> {
        if restore && self.sandbox_storage_status(project)?["storage"] == "controller" {
            return self.lock().query_row("SELECT EXISTS(SELECT 1 FROM ai_sandbox_task_index WHERE project_id=?1 AND busy=1)",[project.to_string()],|r|r.get(0)).map_err(Into::into);
        }
        self.sandbox_project_busy(project)
    }
    pub(crate) fn workspace_vm_backup_pending(&self, vm: Uuid) -> Result<bool, PersistenceError> {
        let projects: Vec<Uuid> = {
            let c = self.lock();
            let mut q = c.prepare("SELECT project_id FROM project_workspaces WHERE vm_id=?1")?;
            q.query_map([vm.to_string()], |r| r.get::<_, String>(0))?
                .map(|r| Uuid::parse_str(&r?).map_err(|_| conflict()))
                .collect::<Result<_, _>>()?
        };
        for project in projects {
            if self.workspace_backup_active(project)?
                || self.workspace_cleanup_active(project)?
                || crate::project_workspace::backups::fenced(&self.workspaces, project)
                    .map_err(|_| conflict())?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(crate) fn backup_control(
        &self,
        destination: &std::path::Path,
    ) -> Result<(), PersistenceError> {
        self.lock().backup(rusqlite::MAIN_DB, destination, None)?;
        Ok(())
    }
    pub(crate) fn finish_project_restore(
        &self,
        b: &Binding,
        record: &mut Record,
    ) -> Result<(), PersistenceError> {
        let c = self.lock();
        let _durable = Durable::new(&c)?;
        let tx = c.unchecked_transaction()?;
        let matches:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM project_workspaces WHERE project_id=?1 AND database_id=?2 AND volume_id=?3 AND vm_id=?4 AND generation=?5 AND provisioning=0)",params![b.project_id.to_string(),b.database_id.to_string(),b.volume_id.to_string(),b.vm_id.to_string(),i64::try_from(b.generation).map_err(|_|conflict())?],|r|r.get(0))?;
        if !matches || b.generation >= i64::MAX as u64 {
            return Err(conflict());
        }
        let retained:Option<String>=tx.query_row("SELECT document FROM project_backup_operations WHERE id=?1 AND project_id=?2 AND active=1",params![record.public.id.to_string(),b.project_id.to_string()],|r|r.get(0)).optional()?;
        let retained: Record = decode(retained.ok_or_else(conflict)?)?;
        if retained.binding != *b
            || retained.public.request != record.public.request
            || retained.public.phase != "installing"
        {
            return Err(conflict());
        }
        let mut finished = record.clone();
        finished.public.updated_at = crate::ai_sandbox::now();
        finished.public.phase = "complete".into();
        finished.public.restored_generation = Some(b.generation + 1);
        finished.active = true;
        finished.public.error = None;
        tx.execute("UPDATE project_workspaces SET generation=generation+1,verified_runtime_id=NULL WHERE project_id=?1",[b.project_id.to_string()])?;
        tx.execute(
            "UPDATE project_backup_operations SET active=1,document=?2 WHERE id=?1",
            params![finished.public.id.to_string(), encode(&finished)?],
        )?;
        tx.commit()?;
        *record = finished;
        Ok(())
    }
}

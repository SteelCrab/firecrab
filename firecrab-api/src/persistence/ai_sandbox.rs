//! Durable Sandbox records share the private controller database.
use super::{PersistenceError, Store};
use crate::ai_sandbox::{ModelProof, SavedTask};
use firecrab_api_types::{
    SandboxAccessPolicy, SandboxProject, SandboxTaskState, SandboxTaskSummary,
};
use rusqlite::{OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

/// Private project, attempt and model-proof tables.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS ai_sandbox_projects (id TEXT PRIMARY KEY, vm_id TEXT UNIQUE, spec TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS ai_sandbox_tasks (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, spec TEXT NOT NULL) STRICT;
CREATE INDEX IF NOT EXISTS ai_sandbox_task_project ON ai_sandbox_tasks(project_id);
CREATE TABLE IF NOT EXISTS ai_sandbox_task_index (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, ordinal INTEGER NOT NULL, updated_at INTEGER NOT NULL, busy INTEGER NOT NULL, summary TEXT NOT NULL) STRICT;
CREATE INDEX IF NOT EXISTS ai_sandbox_summary_order ON ai_sandbox_task_index(project_id,ordinal);
CREATE INDEX IF NOT EXISTS ai_sandbox_summary_updates ON ai_sandbox_task_index(project_id,updated_at);
CREATE INDEX IF NOT EXISTS ai_sandbox_summary_busy ON ai_sandbox_task_index(busy,project_id);
CREATE INDEX IF NOT EXISTS ai_sandbox_summary_retry ON ai_sandbox_task_index(json_extract(summary,'$.retryRoot'),ordinal);
CREATE TRIGGER IF NOT EXISTS ai_sandbox_index_update AFTER UPDATE OF spec ON ai_sandbox_tasks BEGIN DELETE FROM ai_sandbox_task_index WHERE id=NEW.id; END;
CREATE TRIGGER IF NOT EXISTS ai_sandbox_index_delete AFTER DELETE ON ai_sandbox_tasks BEGIN DELETE FROM ai_sandbox_task_index WHERE id=OLD.id; END;
CREATE TABLE IF NOT EXISTS ai_sandbox_models (connection_id TEXT NOT NULL, model TEXT NOT NULL, spec TEXT NOT NULL, PRIMARY KEY(connection_id,model)) STRICT;
CREATE TABLE IF NOT EXISTS ai_sandbox_access (principal TEXT PRIMARY KEY, spec TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS ai_sandbox_attempts (task_id TEXT PRIMARY KEY, project_id TEXT NOT NULL, attempt_id TEXT NOT NULL UNIQUE, vm_id TEXT, runtime_id TEXT, caller TEXT NOT NULL, source_path TEXT, CHECK ((vm_id IS NULL) = (runtime_id IS NULL))) STRICT;
CREATE TABLE IF NOT EXISTS ai_sandbox_admissions (task_id TEXT PRIMARY KEY, identity TEXT NOT NULL) STRICT;
";

/// Serialize a typed private record without logging its contents.
fn encode<T: Serialize>(value: &T) -> Result<String, PersistenceError> {
    serde_json::to_string(value).map_err(|_| corrupt())
}
/// Reject corrupt records instead of inventing partial state.
fn decode<T: DeserializeOwned>(value: String) -> Result<T, PersistenceError> {
    serde_json::from_str(&value).map_err(|_| corrupt())
}
/// Sanitize stored-record errors.
fn corrupt() -> PersistenceError {
    PersistenceError::CorruptRecord {
        id: "ai_sandbox".into(),
        reason: "invalid Sandbox record".into(),
    }
}

/// Save a compact derived projection in the same transaction as its canonical attempt.
fn index_task(
    conn: &rusqlite::Connection,
    ordinal: i64,
    saved: &SavedTask,
) -> Result<(), PersistenceError> {
    let summary = SandboxTaskSummary::from(&saved.task);
    let updated_at = i64::try_from(saved.task.updated_at).map_err(|_| corrupt())?;
    let busy = saved.task.state.active() || saved.guest_pending || saved.guest_staged;
    conn.execute("INSERT INTO ai_sandbox_task_index(id,project_id,ordinal,updated_at,busy,summary) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(id) DO UPDATE SET updated_at=excluded.updated_at,busy=excluded.busy,summary=excluded.summary",params![saved.task.id.to_string(),saved.task.project_id.to_string(),ordinal,updated_at,busy,encode(&summary)?])?;
    Ok(())
}

impl Store {
    /// A revoked policy remains a delegation boundary; never restore legacy operator access.
    pub fn sandbox_delegate(&self, principal: Uuid) -> Result<bool, PersistenceError> {
        Ok(self.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_sandbox_access WHERE principal=?1)",
            [principal.to_string()],
            |row| row.get(0),
        )?)
    }
    /// Missing host policies deny every delegated operation.
    pub fn sandbox_access(&self, principal: Uuid) -> Result<SandboxAccessPolicy, PersistenceError> {
        self.lock()
            .query_row(
                "SELECT spec FROM ai_sandbox_access WHERE principal=?1",
                [principal.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .map(decode)
            .transpose()
            .map(Option::unwrap_or_default)
    }
    /// Atomically replace one host's complete policy, including explicit revocation.
    pub fn save_sandbox_access(
        &self,
        principal: Uuid,
        policy: &SandboxAccessPolicy,
    ) -> Result<(), PersistenceError> {
        self.lock().execute("INSERT INTO ai_sandbox_access(principal,spec) VALUES(?1,?2) ON CONFLICT(principal) DO UPDATE SET spec=excluded.spec", params![principal.to_string(),encode(policy)?])?;
        Ok(())
    }

    /// Backfill legacy local-operator records before serving requests; never grant a remote host.
    pub fn rebuild_sandbox_references(&self) -> Result<(), PersistenceError> {
        loop {
            let row: Option<(String,String,String)> = self.lock().query_row("SELECT t.id,t.project_id,t.spec FROM ai_sandbox_tasks t LEFT JOIN ai_sandbox_attempts a ON a.task_id=t.id WHERE a.task_id IS NULL LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let Some((id, project, record)) = row else {
                break;
            };
            let mut saved: SavedTask = decode(record)?;
            if saved.task.id.to_string() != id || saved.task.project_id.to_string() != project {
                return Err(corrupt());
            }
            if saved.task.runtime.is_none() {
                saved.task.runtime = saved.guest;
            }
            self.save_sandbox_task(&saved)?;
        }
        Ok(())
    }
    /// List project settings without accessing provider credentials.
    pub fn sandbox_projects(&self) -> Result<Vec<SandboxProject>, PersistenceError> {
        self.lock()
            .prepare("SELECT spec FROM ai_sandbox_projects ORDER BY rowid")?
            .query_map([], |r| r.get(0))?
            .map(|r| decode(r?))
            .collect()
    }
    /// Read one project.
    pub fn sandbox_project(&self, id: Uuid) -> Result<Option<SandboxProject>, PersistenceError> {
        self.lock()
            .query_row(
                "SELECT spec FROM ai_sandbox_projects WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .map(decode)
            .transpose()
    }
    /// Save a project and its exclusive borrowed-VM reference.
    pub fn save_sandbox_project(&self, project: &SandboxProject) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let previous: Option<String> = tx
            .query_row(
                "SELECT spec FROM ai_sandbox_projects WHERE id=?1",
                [project.id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(previous) = previous {
            let previous: SandboxProject = decode(previous)?;
            if previous.id != project.id || previous.owner != project.owner {
                return Err(corrupt());
            }
        }
        tx.execute("INSERT INTO ai_sandbox_projects(id,vm_id,spec) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET vm_id=excluded.vm_id,spec=excluded.spec",
            params![project.id.to_string(), project.spec.vm_id.map(|v|v.to_string()), encode(project)?])?;
        tx.commit()?;
        Ok(())
    }
    /// Delete metadata only; workspace removal is performed after ownership checks.
    pub fn delete_sandbox_project(&self, id: Uuid) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM ai_sandbox_admissions WHERE task_id IN (SELECT id FROM ai_sandbox_tasks WHERE project_id=?1)",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM ai_sandbox_attempts WHERE project_id=?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM ai_sandbox_task_index WHERE project_id=?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM ai_sandbox_tasks WHERE project_id=?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM ai_sandbox_projects WHERE id=?1",
            [id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Backfill earlier records one at a time; never load all retained diffs into memory.
    pub fn rebuild_sandbox_index(&self) -> Result<(), PersistenceError> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM ai_sandbox_task_index WHERE id NOT IN (SELECT id FROM ai_sandbox_tasks)",
            [],
        )?;
        let mut statement=conn.prepare("SELECT t.rowid,t.spec FROM ai_sandbox_tasks t LEFT JOIN ai_sandbox_task_index i ON i.id=t.id WHERE i.id IS NULL ORDER BY t.rowid")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let ordinal = row.get(0)?;
            let saved: SavedTask = decode(row.get(1)?)?;
            index_task(&conn, ordinal, &saved)?;
        }
        Ok(())
    }
    /// Resolve an older-page cursor within its own project without reading the task body.
    pub fn sandbox_cursor(&self, project: Uuid, id: Uuid) -> Result<Option<i64>, PersistenceError> {
        Ok(self
            .lock()
            .query_row(
                "SELECT ordinal FROM ai_sandbox_task_index WHERE project_id=?1 AND id=?2",
                params![project.to_string(), id.to_string()],
                |r| r.get(0),
            )
            .optional()?)
    }
    /// Read only bounded summaries for polling; retained tool output and diffs stay in their rows.
    #[cfg(test)]
    pub fn sandbox_summaries(
        &self,
        project: Uuid,
        since: Option<u64>,
        before: Option<i64>,
    ) -> Result<Vec<SandboxTaskSummary>, PersistenceError> {
        self.sandbox_visible_summaries(project, since, before, None)
    }
    /// Filter resource grants before pagination, without loading private task bodies.
    pub fn sandbox_visible_summaries(
        &self,
        project: Uuid,
        since: Option<u64>,
        before: Option<i64>,
        sources: Option<Vec<String>>,
    ) -> Result<Vec<SandboxTaskSummary>, PersistenceError> {
        let sources = sources.map(|paths| encode(&paths)).transpose()?;
        if since.is_some_and(|v| v > i64::MAX as u64) {
            return Ok(vec![]);
        }
        let conn = self.lock();
        let mut statement=conn.prepare("SELECT i.summary FROM ai_sandbox_task_index i JOIN ai_sandbox_attempts a ON a.task_id=i.id WHERE i.project_id=?1 AND (?2 IS NULL OR i.ordinal<?2) AND i.updated_at>=?3 AND (?4 IS NULL OR a.source_path IS NULL OR a.source_path IN (SELECT value FROM json_each(?4))) ORDER BY i.ordinal DESC LIMIT 50")?;
        let mut rows = statement
            .query_map(
                params![
                    project.to_string(),
                    before,
                    since.unwrap_or(0) as i64,
                    sources
                ],
                |r| r.get(0),
            )?
            .map(|r| decode(r?))
            .collect::<Result<Vec<_>, _>>()?;
        rows.reverse();
        Ok(rows)
    }
    /// Read recent completed context without decoding any file changes or tool logs.
    #[cfg(test)]
    pub fn sandbox_history(
        &self,
        project: Uuid,
    ) -> Result<Vec<SandboxTaskSummary>, PersistenceError> {
        self.sandbox_history_for_source(project, None)
    }
    /// Source changes never pull another source's history into a new provider context.
    pub fn sandbox_history_for_source(
        &self,
        project: Uuid,
        source: Option<&str>,
    ) -> Result<Vec<SandboxTaskSummary>, PersistenceError> {
        let conn = self.lock();
        let mut statement=conn.prepare("SELECT i.summary FROM ai_sandbox_task_index i JOIN ai_sandbox_attempts a ON a.task_id=i.id WHERE i.project_id=?1 AND a.source_path IS ?2 AND json_extract(i.summary,'$.state') IN ('result','merged') ORDER BY i.ordinal DESC LIMIT 12")?;
        let mut rows = statement
            .query_map(params![project.to_string(), source], |r| r.get(0))?
            .map(|r| decode(r?))
            .collect::<Result<Vec<_>, _>>()?;
        rows.reverse();
        Ok(rows)
    }
    /// Count retained attempts without reading their bodies.
    pub fn sandbox_task_count(&self, project: Uuid) -> Result<usize, PersistenceError> {
        let count: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM ai_sandbox_task_index WHERE project_id=?1",
            [project.to_string()],
            |r| r.get(0),
        )?;
        Ok(count as usize)
    }
    /// Active and unconfirmed guest work prevents source/configuration mutations.
    pub fn sandbox_project_busy(&self, project: Uuid) -> Result<bool, PersistenceError> {
        Ok(self.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_sandbox_task_index WHERE project_id=?1 AND busy=1)",
            [project.to_string()],
            |r| r.get(0),
        )?)
    }
    /// Read only active or unresolved attempts, including dependency waiters.
    pub fn sandbox_busy_tasks(&self, project: Uuid) -> Result<Vec<SavedTask>, PersistenceError> {
        let ids: Vec<String> = self.lock().prepare("SELECT id FROM ai_sandbox_task_index WHERE project_id=?1 AND busy=1 ORDER BY ordinal")?
            .query_map([project.to_string()], |row| row.get(0))?.collect::<Result<_,_>>()?;
        ids.into_iter()
            .map(|id| {
                self.sandbox_task(Uuid::parse_str(&id).map_err(|_| corrupt())?)?
                    .ok_or_else(corrupt)
            })
            .collect()
    }
    /// A retained Workspace VM admits one active or uncertain attempt across every project.
    pub fn sandbox_runtime_busy(&self, vm: Uuid, except: Uuid) -> Result<bool, PersistenceError> {
        Ok(self.lock().query_row("SELECT EXISTS(SELECT 1 FROM ai_sandbox_task_index i JOIN ai_sandbox_attempts a ON a.task_id=i.id JOIN ai_sandbox_tasks t ON t.id=i.id WHERE a.vm_id=?1 AND i.id!=?2 AND i.busy=1 AND (json_extract(i.summary,'$.state')!='waiting' OR json_extract(t.spec,'$.guest_pending')=1 OR json_extract(t.spec,'$.guest_staged')=1))",
            params![vm.to_string(),except.to_string()], |row| row.get(0))?)
    }
    /// Retry chains retain every task/attempt; the latest ordinal is the authoritative outcome.
    pub fn sandbox_retry_tasks(&self, root: Uuid) -> Result<Vec<SavedTask>, PersistenceError> {
        let ids: Vec<String> = self.lock().prepare("SELECT id FROM ai_sandbox_task_index WHERE id=?1 OR json_extract(summary,'$.retryRoot')=?1 ORDER BY ordinal")?
            .query_map([root.to_string()], |row| row.get(0))?.collect::<Result<_,_>>()?;
        ids.into_iter()
            .map(|id| {
                self.sandbox_task(Uuid::parse_str(&id).map_err(|_| corrupt())?)?
                    .ok_or_else(corrupt)
            })
            .collect()
    }
    /// Bound queued and unresolved guest attempts across controller restarts.
    pub fn sandbox_pending_count(&self) -> Result<usize, PersistenceError> {
        let count: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM ai_sandbox_task_index WHERE busy=1",
            [],
            |r| r.get(0),
        )?;
        Ok(count as usize)
    }
    /// Read a task together with its private workspace and guest ownership.
    pub fn sandbox_task(&self, id: Uuid) -> Result<Option<SavedTask>, PersistenceError> {
        let saved: Option<SavedTask> = self
            .lock()
            .query_row(
                "SELECT spec FROM ai_sandbox_tasks WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .map(decode)
            .transpose()?;
        if let Some(saved) = &saved {
            if saved.task.id != id {
                return Err(corrupt());
            }
            verify_references(&self.lock(), saved)?;
        }
        Ok(saved)
    }
    /// Save a checkpoint before starting provider or guest side effects.
    pub fn save_sandbox_task(&self, task: &SavedTask) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let runtime = task.task.runtime;
        validate_runtime(task)?;
        validate_outputs(task)?;
        tx.execute("INSERT INTO ai_sandbox_attempts(task_id,project_id,attempt_id,vm_id,runtime_id,caller,source_path) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(task_id) DO NOTHING",params![task.task.id.to_string(),task.task.project_id.to_string(),task.task.attempt_id.to_string(),runtime.map(|r|r.vm_id.to_string()),runtime.map(|r|r.runtime_id.to_string()),encode(&task.caller)?,task.project.source_path])?;
        tx.execute("INSERT INTO ai_sandbox_admissions(task_id,identity) VALUES(?1,?2) ON CONFLICT(task_id) DO NOTHING", params![task.task.id.to_string(), admission_identity(task)?])?;
        verify_references(&tx, task)?;
        tx.execute("INSERT INTO ai_sandbox_tasks(id,project_id,spec) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec",params![task.task.id.to_string(),task.task.project_id.to_string(),encode(task)?])?;
        let ordinal = tx.query_row(
            "SELECT rowid FROM ai_sandbox_tasks WHERE id=?1",
            [task.task.id.to_string()],
            |r| r.get(0),
        )?;
        index_task(&tx, ordinal, task)?;
        tx.commit()?;
        Ok(())
    }
    /// Restart settles active records one by one; terminal file payloads are not loaded.
    pub fn interrupt_sandbox_tasks(&self) -> Result<(), PersistenceError> {
        let ids:Vec<String>=self.lock().prepare("SELECT id FROM ai_sandbox_task_index WHERE busy=1 AND json_extract(summary,'$.state') IN ('waiting','accepted','in_progress')")?.query_map([],|r|r.get(0))?.collect::<Result<_,_>>()?;
        for id in ids {
            let key = Uuid::parse_str(&id).map_err(|_| corrupt())?;
            let mut saved = self.sandbox_task(key)?.ok_or_else(corrupt)?;
            saved.task.state = SandboxTaskState::Interrupted;
            saved.task.error_code = Some("sandbox_restarted".into());
            saved.task.error =
                Some("Controller restarted. Reconcile the attempt before retrying.".into());
            self.save_sandbox_task(&saved)?;
        }
        Ok(())
    }
    /// Store proof bound to the authenticated account and model adapter.
    pub fn save_sandbox_model(&self, proof: &ModelProof) -> Result<(), PersistenceError> {
        self.lock().execute("INSERT INTO ai_sandbox_models(connection_id,model,spec) VALUES(?1,?2,?3) ON CONFLICT(connection_id,model) DO UPDATE SET spec=excluded.spec",
            params![proof.selection.connection_id.to_string(),proof.selection.model,encode(proof)?])?;
        Ok(())
    }
    /// List proofs; callers discard proofs from replaced or disconnected credentials.
    pub fn sandbox_models(&self, connection: Uuid) -> Result<Vec<ModelProof>, PersistenceError> {
        self.lock()
            .prepare("SELECT spec FROM ai_sandbox_models WHERE connection_id=?1")?
            .query_map([connection.to_string()], |r| r.get(0))?
            .map(|r| decode(r?))
            .collect()
    }
}

/// Canonical identities cannot change during checkpointing, apply, retry or recovery.
fn verify_references(
    conn: &rusqlite::Connection,
    task: &SavedTask,
) -> Result<(), PersistenceError> {
    validate_runtime(task)?;
    let runtime = task.task.runtime;
    let valid:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM ai_sandbox_attempts WHERE task_id=?1 AND project_id=?2 AND attempt_id=?3 AND vm_id IS ?4 AND runtime_id IS ?5 AND caller=?6 AND source_path IS ?7)",params![task.task.id.to_string(),task.task.project_id.to_string(),task.task.attempt_id.to_string(),runtime.map(|r|r.vm_id.to_string()),runtime.map(|r|r.runtime_id.to_string()),encode(&task.caller)?,task.project.source_path],|row|row.get(0))?;
    if !valid {
        return Err(corrupt());
    }
    let admission: Option<String> = conn
        .query_row(
            "SELECT identity FROM ai_sandbox_admissions WHERE task_id=?1",
            [task.task.id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(identity) = admission
        && admission_identity(task)? != identity
    {
        return Err(corrupt());
    }
    validate_outputs(task)?;
    Ok(())
}

fn admission_identity(task: &SavedTask) -> Result<String, PersistenceError> {
    encode(&(
        &task.task.request,
        &task.project,
        &task.task.selection,
        &task.task.git_commit,
        task.task.retry_root,
        task.task.retry_number,
        &task.task.environment,
        &task.execution_mode,
    ))
}

fn validate_outputs(task: &SavedTask) -> Result<(), PersistenceError> {
    if task.task.retry_number > 3
        || task.task.artifacts.len() > 16
        || task.task.tests.len() > 64
        || (task.results_exported
            && (task.task.state.active() || task.guest_pending || task.guest_staged))
    {
        return Err(corrupt());
    }
    let mut artifacts = std::collections::BTreeSet::new();
    let mut artifact_bytes = 0usize;
    for artifact in &task.task.artifacts {
        artifact_bytes = artifact_bytes
            .checked_add(artifact.size)
            .ok_or_else(corrupt)?;
        if artifact.task_id != task.task.id
            || artifact.attempt_id != task.task.attempt_id
            || !artifacts.insert(artifact.id)
            || !crate::ai_sandbox::files::allowed(&artifact.path)
            || artifact.size > crate::ai_sandbox::files::FILE_LIMIT
            || artifact.sha256.len() != 64
            || !artifact.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(corrupt());
        }
    }
    if artifact_bytes > 8 * crate::ai_sandbox::files::FILE_LIMIT {
        return Err(corrupt());
    }
    for test in &task.task.tests {
        let Some(event) = task.task.events.get(test.event_sequence) else {
            return Err(corrupt());
        };
        if !matches!(event.name.as_str(), "run_command" | "run_argv")
            || event.exit_code.is_none()
            || test.passed != (event.exit_code == Some(0))
        {
            return Err(corrupt());
        }
    }
    Ok(())
}

fn validate_runtime(task: &SavedTask) -> Result<(), PersistenceError> {
    let runtime = task.task.runtime;
    if let Some(runtime) = runtime
        && (task.task.request.kind != firecrab_api_types::SandboxTaskKind::Code
            || task.project.vm_id != Some(runtime.vm_id)
            || task.guest.is_none())
    {
        return Err(corrupt());
    }
    if task.guest.is_some_and(|guest| {
        Some(guest.vm_id) != task.project.vm_id
            || runtime.is_none_or(|original| original.vm_id != guest.vm_id)
    }) {
        return Err(corrupt());
    }
    Ok(())
}

#[cfg(test)]
#[path = "ai_sandbox_tests.rs"]
mod tests;

//! Durable Sandbox records share the private controller database.
use super::{PersistenceError, Store};
use crate::ai_sandbox::{ModelProof, SavedTask};
use firecrab_api_types::{SandboxProject, SandboxTaskState, SandboxTaskSummary};
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
CREATE TRIGGER IF NOT EXISTS ai_sandbox_index_update AFTER UPDATE OF spec ON ai_sandbox_tasks BEGIN DELETE FROM ai_sandbox_task_index WHERE id=NEW.id; END;
CREATE TRIGGER IF NOT EXISTS ai_sandbox_index_delete AFTER DELETE ON ai_sandbox_tasks BEGIN DELETE FROM ai_sandbox_task_index WHERE id=OLD.id; END;
CREATE TABLE IF NOT EXISTS ai_sandbox_models (connection_id TEXT NOT NULL, model TEXT NOT NULL, spec TEXT NOT NULL, PRIMARY KEY(connection_id,model)) STRICT;
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
        self.lock().execute("INSERT INTO ai_sandbox_projects(id,vm_id,spec) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET vm_id=excluded.vm_id,spec=excluded.spec",
            params![project.id.to_string(), project.spec.vm_id.map(|v|v.to_string()), encode(project)?])?;
        Ok(())
    }
    /// Delete metadata only; workspace removal is performed after ownership checks.
    pub fn delete_sandbox_project(&self, id: Uuid) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
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
    pub fn sandbox_summaries(
        &self,
        project: Uuid,
        since: Option<u64>,
        before: Option<i64>,
    ) -> Result<Vec<SandboxTaskSummary>, PersistenceError> {
        if since.is_some_and(|v| v > i64::MAX as u64) {
            return Ok(vec![]);
        }
        let conn = self.lock();
        let mut statement=conn.prepare("SELECT summary FROM ai_sandbox_task_index WHERE project_id=?1 AND (?2 IS NULL OR ordinal<?2) AND updated_at>=?3 ORDER BY ordinal DESC LIMIT 50")?;
        let mut rows = statement
            .query_map(
                params![project.to_string(), before, since.unwrap_or(0) as i64],
                |r| r.get(0),
            )?
            .map(|r| decode(r?))
            .collect::<Result<Vec<_>, _>>()?;
        rows.reverse();
        Ok(rows)
    }
    /// Read recent completed context without decoding any file changes or tool logs.
    pub fn sandbox_history(
        &self,
        project: Uuid,
    ) -> Result<Vec<SandboxTaskSummary>, PersistenceError> {
        let conn = self.lock();
        let mut statement=conn.prepare("SELECT summary FROM ai_sandbox_task_index WHERE project_id=?1 AND json_extract(summary,'$.state') IN ('result','merged') ORDER BY ordinal DESC LIMIT 12")?;
        let mut rows = statement
            .query_map([project.to_string()], |r| r.get(0))?
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
        self.lock()
            .query_row(
                "SELECT spec FROM ai_sandbox_tasks WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .map(decode)
            .transpose()
    }
    /// Save a checkpoint before starting provider or guest side effects.
    pub fn save_sandbox_task(&self, task: &SavedTask) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
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

#[cfg(test)]
#[path = "ai_sandbox_tests.rs"]
mod tests;

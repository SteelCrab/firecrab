use super::{PersistenceError, Store, code_runtime, code_verification::Snapshot, code_workspaces};
use firecrab_api_types::*;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;
pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS code_agent_runs(id TEXT PRIMARY KEY,workspace_id TEXT NOT NULL,request_hash TEXT NOT NULL,phase TEXT NOT NULL,record TEXT NOT NULL) STRICT; CREATE TABLE IF NOT EXISTS code_agent_cancellations(id TEXT PRIMARY KEY,workspace_id TEXT NOT NULL) STRICT; CREATE INDEX IF NOT EXISTS code_agent_history ON code_agent_runs(workspace_id); CREATE UNIQUE INDEX IF NOT EXISTS code_agent_one_active ON code_agent_runs(workspace_id) WHERE phase IN ('queued','starting','running','cancelling');";
fn corrupt() -> PersistenceError {
    PersistenceError::CorruptRecord {
        id: "code_agent_run".into(),
        reason: "invalid agent run record".into(),
    }
}
fn encode(run: &CodeAgentRun) -> Result<String, PersistenceError> {
    serde_json::to_string(run).map_err(|_| corrupt())
}
fn phase(run: &CodeAgentRun) -> String {
    serde_json::to_value(run.phase)
        .expect("phase")
        .as_str()
        .expect("phase string")
        .into()
}
impl Store {
    pub(crate) fn code_agent_run(
        &self,
        workspace: Uuid,
        id: Uuid,
    ) -> Result<Option<(CodeAgentRun, String)>, PersistenceError> {
        self.lock()
            .query_row(
                "SELECT record,request_hash FROM code_agent_runs WHERE workspace_id=?1 AND id=?2",
                params![workspace.to_string(), id.to_string()],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
            .map(|(raw, hash)| Ok((serde_json::from_str(&raw).map_err(|_| corrupt())?, hash)))
            .transpose()
    }
    pub(crate) fn code_agent_runs(
        &self,
        workspace: Uuid,
    ) -> Result<Vec<CodeAgentRun>, PersistenceError> {
        let conn = self.lock();
        let mut query = conn.prepare(
            "SELECT record FROM code_agent_runs WHERE workspace_id=?1 ORDER BY rowid DESC LIMIT 20",
        )?;
        query
            .query_map([workspace.to_string()], |r| r.get::<_, String>(0))?
            .map(|raw| serde_json::from_str(&raw?).map_err(|_| corrupt()))
            .collect()
    }
    pub(crate) fn admit_code_agent_run(
        &self,
        snapshot: &Snapshot,
        runtime: &code_runtime::Record,
        run: &CodeAgentRun,
        hash: &str,
    ) -> Result<bool, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = code_workspaces::read(&tx, run.workspace_id)?;
        let current_runtime = code_runtime::read(&tx, run.workspace_id)?;
        let ai: Option<String> = tx
            .query_row(
                "SELECT spec FROM pipe_ai_connections WHERE id=?1",
                [run.connection_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if current.is_none_or(|w| {
            w.id != snapshot.workspace.id
                || w.revision != snapshot.workspace.revision
                || w.spec != snapshot.workspace.spec
        }) || ai != snapshot.ai_json
            || current_runtime
                .is_none_or(|r| r.runtime != runtime.runtime || r.spec != runtime.spec)
        {
            return Ok(false);
        }
        let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM code_agent_runs WHERE workspace_id=?1 AND phase IN ('queued','starting','running','cancelling'))",[run.workspace_id.to_string()],|r|r.get(0))?;
        let cancelled: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM code_agent_cancellations WHERE id=?1)",
            [run.id.to_string()],
            |r| r.get(0),
        )?;
        if cancelled
            || busy
            || tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM code_agent_runs WHERE id=?1)",
                [run.id.to_string()],
                |r| r.get::<_, bool>(0),
            )?
        {
            return Ok(false);
        }
        tx.execute("INSERT INTO code_agent_runs(id,workspace_id,request_hash,phase,record) VALUES(?1,?2,?3,?4,?5)",params![run.id.to_string(),run.workspace_id.to_string(),hash,phase(run),encode(run)?])?;
        tx.commit()?;
        Ok(true)
    }
    pub(crate) fn save_code_agent_run(&self, run: &CodeAgentRun) -> Result<bool, PersistenceError> {
        // A worker cannot overwrite another process's cancellation or a terminal observation.
        Ok(self.lock().execute("UPDATE code_agent_runs SET phase=?1,record=?2 WHERE id=?3 AND workspace_id=?4 AND phase IN ('queued','starting','running')",params![phase(run),encode(run)?,run.id.to_string(),run.workspace_id.to_string()])?==1)
    }
    pub(crate) fn cancel_code_agent_run(
        &self,
        workspace: Uuid,
        id: Uuid,
        finish: bool,
    ) -> Result<Option<CodeAgentRun>, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Fence even a delayed POST whose reservation has not arrived yet.
        tx.execute(
            "INSERT OR IGNORE INTO code_agent_cancellations(id,workspace_id) VALUES(?1,?2)",
            params![id.to_string(), workspace.to_string()],
        )?;
        let raw: Option<String> = tx
            .query_row(
                "SELECT record FROM code_agent_runs WHERE id=?1 AND workspace_id=?2",
                params![id.to_string(), workspace.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            tx.commit()?;
            return Ok(None);
        };
        let mut run: CodeAgentRun = serde_json::from_str(&raw).map_err(|_| corrupt())?;
        if run.phase.active() {
            run.phase = if finish {
                CodeAgentRunPhase::Cancelled
            } else {
                CodeAgentRunPhase::Cancelling
            };
            run.updated_at_ms = chrono::Utc::now().timestamp_millis();
            tx.execute(
                "UPDATE code_agent_runs SET phase=?1,record=?2 WHERE id=?3",
                params![phase(&run), encode(&run)?, id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(Some(run))
    }
}

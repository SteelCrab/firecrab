//! Schedule mutation, execution claims, and results share SQLite transactions.

use firecrab_api_types::{
    ScheduleResponse, ScheduleRun, ScheduleRunStatus, ScheduleSpec, ScheduleTiming,
};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use super::{PersistenceError, Store};

/// Tables and indexes enforcing one claim per instant and one active run per schedule.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS schedules (
    id TEXT PRIMARY KEY,
    spec TEXT NOT NULL,
    enabled INTEGER NOT NULL,
    next_run_at_ms INTEGER,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS schedules_due ON schedules(next_run_at_ms) WHERE enabled = 1;
CREATE TABLE IF NOT EXISTS schedule_runs (
    id TEXT PRIMARY KEY,
    schedule_id TEXT NOT NULL,
    scheduled_at_ms INTEGER NOT NULL,
    started_at_ms INTEGER NOT NULL,
    finished_at_ms INTEGER,
    status TEXT NOT NULL CHECK(status IN ('running', 'succeeded', 'failed', 'interrupted')),
    error TEXT,
    UNIQUE(schedule_id, scheduled_at_ms)
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS schedule_one_running ON schedule_runs(schedule_id) WHERE status = 'running';
CREATE INDEX IF NOT EXISTS schedule_run_history ON schedule_runs(schedule_id);
";

/// Expected mutation outcomes, distinct from database or record-decoding failures.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ScheduleMutation {
    /// Applied successfully, including an idempotent enable/disable request.
    Done,
    /// No schedule has the requested identifier.
    Missing,
    /// Active execution prevents replacement, deletion, or re-enabling.
    Busy,
    /// The requested enabled configuration has no unconsumed future occurrence.
    Expired,
}

/// Wrap invalid persisted data or scheduler ownership failures with schedule context.
fn corrupt(error: impl std::fmt::Display) -> PersistenceError {
    PersistenceError::CorruptRecord {
        id: "schedule".to_owned(),
        reason: error.to_string(),
    }
}

/// Decode configuration columns; callers attach the latest execution separately.
fn decode_schedule(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduleResponse> {
    let id: String = row.get(0)?;
    let spec: String = row.get(1)?;
    let parse_error = |error: Box<dyn std::error::Error + Send + Sync>| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, error)
    };
    Ok(ScheduleResponse {
        id: Uuid::parse_str(&id).map_err(|e| parse_error(Box::new(e)))?,
        spec: serde_json::from_str(&spec).map_err(|e| parse_error(Box::new(e)))?,
        next_run_at_ms: row.get(2)?,
        created_at_ms: row.get(3)?,
        updated_at_ms: row.get(4)?,
        last_run: None,
    })
}

/// Read at most `limit` executions in reverse claim-insertion order.
fn runs(conn: &Connection, id: Uuid, limit: usize) -> Result<Vec<ScheduleRun>, PersistenceError> {
    let mut statement = conn.prepare(
        "SELECT id, scheduled_at_ms, started_at_ms, finished_at_ms, status, error
        FROM schedule_runs WHERE schedule_id = ?1 ORDER BY rowid DESC LIMIT ?2",
    )?;
    let rows = statement.query_map(params![id.to_string(), limit as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get::<_, String>(4)?,
            row.get(5)?,
        ))
    })?;
    rows.map(|row| {
        let (run_id, scheduled_at_ms, started_at_ms, finished_at_ms, status, error) = row?;
        let status = match status.as_str() {
            "running" => ScheduleRunStatus::Running,
            "succeeded" => ScheduleRunStatus::Succeeded,
            "failed" => ScheduleRunStatus::Failed,
            "interrupted" => ScheduleRunStatus::Interrupted,
            other => return Err(corrupt(format!("unknown run status {other}"))),
        };
        Ok(ScheduleRun {
            id: Uuid::parse_str(&run_id).map_err(corrupt)?,
            schedule_id: id,
            scheduled_at_ms,
            started_at_ms,
            finished_at_ms,
            status,
            error,
        })
    })
    .collect()
}

/// Read one configuration and its latest run using the caller's connection or transaction.
fn get(conn: &Connection, id: Uuid) -> Result<Option<ScheduleResponse>, PersistenceError> {
    let mut result = conn.query_row("SELECT id, spec, next_run_at_ms, created_at_ms, updated_at_ms FROM schedules WHERE id = ?1",
        [id.to_string()], decode_schedule).optional()?;
    if let Some(schedule) = &mut result {
        schedule.last_run = runs(conn, id, 1)?.pop();
    }
    Ok(result)
}

/// Insert or update scheduling state while preserving the original creation time.
/// Execution history is stored separately and is not changed here.
fn write(conn: &Connection, schedule: &ScheduleResponse) -> Result<(), PersistenceError> {
    let spec = serde_json::to_string(&schedule.spec).map_err(corrupt)?;
    conn.execute("INSERT INTO schedules (id, spec, enabled, next_run_at_ms, created_at_ms, updated_at_ms)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(id) DO UPDATE SET spec = excluded.spec,
        enabled = excluded.enabled, next_run_at_ms = excluded.next_run_at_ms, updated_at_ms = excluded.updated_at_ms",
        params![schedule.id.to_string(), spec, schedule.spec.enabled, schedule.next_run_at_ms, schedule.created_at_ms, schedule.updated_at_ms])?;
    Ok(())
}

/// Whether a durable claim exists whose completion or interruption is not recorded.
fn busy(conn: &Connection, id: Uuid) -> Result<bool, PersistenceError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM schedule_runs WHERE schedule_id = ?1 AND status = 'running')",
        [id.to_string()],
        |r| r.get(0),
    )?)
}

/// Find the next occurrence after both `now` and the latest retained claim instant.
/// Disabled schedules have no next occurrence. A backwards clock adjustment must
/// not reuse an already claimed instant.
fn next(
    conn: &Connection,
    schedule: &ScheduleResponse,
    now: i64,
) -> Result<Option<i64>, PersistenceError> {
    if !schedule.spec.enabled {
        return Ok(None);
    }
    let previous: Option<i64> = conn.query_row(
        "SELECT MAX(scheduled_at_ms) FROM schedule_runs WHERE schedule_id = ?1",
        [schedule.id.to_string()],
        |row| row.get(0),
    )?;
    crate::scheduler::next_run(
        &schedule.spec.timing,
        previous.map_or(now, |at| now.max(at)),
    )
    .map_err(corrupt)
}

impl Store {
    /// SQLite's POSIX locks are separate from this whole-process advisory lock.
    /// Hold it before recovery so a second API cannot interrupt a live worker.
    pub(crate) fn scheduler_lock(&self) -> Result<std::fs::File, PersistenceError> {
        let conn = self.lock();
        let path = conn
            .path()
            .ok_or_else(|| corrupt("scheduler requires a file-backed database"))?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(corrupt)?;
        file.try_lock().map_err(|error| {
            corrupt(format!(
                "scheduler database is already owned or cannot be locked: {error}"
            ))
        })?;
        Ok(file)
    }
    /// List schedules by display name and identifier, each with its latest run.
    pub(crate) fn list_schedules(&self) -> Result<Vec<ScheduleResponse>, PersistenceError> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT id, spec, next_run_at_ms, created_at_ms, updated_at_ms FROM schedules ORDER BY json_extract(spec, '$.name'), id")?;
        let mut schedules = stmt
            .query_map([], decode_schedule)?
            .collect::<Result<Vec<_>, _>>()?;
        for schedule in &mut schedules {
            schedule.last_run = runs(&conn, schedule.id, 1)?.pop();
        }
        Ok(schedules)
    }

    /// Read one schedule and its latest run; return `None` if it does not exist.
    pub(crate) fn schedule(&self, id: Uuid) -> Result<Option<ScheduleResponse>, PersistenceError> {
        get(&self.lock(), id)
    }

    /// Persist a newly validated schedule configuration without creating a run.
    pub(crate) fn insert_schedule(
        &self,
        schedule: &ScheduleResponse,
    ) -> Result<(), PersistenceError> {
        write(&self.lock(), schedule)
    }

    /// Active operations reject edits and deletion. Disabling is always allowed.
    ///
    /// `replacement` supplies a complete configuration; otherwise `enabled`
    /// selects a pause/resume request. With neither, delete configuration and
    /// history. Idempotent enable/disable requests return without changing state.
    /// Future timing is recalculated under the same immediate transaction.
    pub(crate) fn mutate_schedule(
        &self,
        id: Uuid,
        replacement: Option<&ScheduleSpec>,
        enabled: Option<bool>,
        now: i64,
    ) -> Result<ScheduleMutation, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let Some(mut schedule) = get(&tx, id)? else {
            return Ok(ScheduleMutation::Missing);
        };
        if replacement.is_none() && enabled == Some(schedule.spec.enabled) {
            return Ok(ScheduleMutation::Done);
        }
        let running = busy(&tx, id)?;
        if running && enabled != Some(false) {
            return Ok(ScheduleMutation::Busy);
        }
        if let Some(spec) = replacement {
            schedule.spec = spec.clone();
        } else if let Some(enabled) = enabled {
            schedule.spec.enabled = enabled;
        } else {
            tx.execute(
                "DELETE FROM schedule_runs WHERE schedule_id = ?1",
                [id.to_string()],
            )?;
            tx.execute("DELETE FROM schedules WHERE id = ?1", [id.to_string()])?;
            tx.commit()?;
            return Ok(ScheduleMutation::Done);
        }
        schedule.next_run_at_ms = next(&tx, &schedule, now)?;
        if schedule.spec.enabled && schedule.next_run_at_ms.is_none() {
            return Ok(ScheduleMutation::Expired);
        }
        schedule.updated_at_ms = now;
        write(&tx, &schedule)?;
        tx.commit()?;
        Ok(ScheduleMutation::Done)
    }

    /// Read up to 100 executions, newest claim first; unknown IDs return no rows.
    pub(crate) fn schedule_runs(&self, id: Uuid) -> Result<Vec<ScheduleRun>, PersistenceError> {
        runs(&self.lock(), id, 100)
    }

    /// Claim and consume due instants atomically, before calling a VM handler.
    ///
    /// Select up to `limit` idle schedules due at or before Unix millisecond `now`.
    /// Clear their next times and disable one-time schedules before committing.
    /// Each schedule yields one claim even if several recurring times were missed.
    /// Prune completed or interrupted history to keep at most 100 total runs.
    pub(crate) fn claim_schedules(
        &self,
        now: i64,
        limit: usize,
    ) -> Result<Vec<(ScheduleResponse, ScheduleRun)>, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let schedules = {
            let mut stmt = tx.prepare("SELECT id, spec, next_run_at_ms, created_at_ms, updated_at_ms FROM schedules
                WHERE enabled = 1 AND next_run_at_ms <= ?1
                AND NOT EXISTS(SELECT 1 FROM schedule_runs WHERE schedule_id = schedules.id AND status = 'running')
                ORDER BY next_run_at_ms, id LIMIT ?2")?;
            stmt.query_map(params![now, limit as i64], decode_schedule)?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut claimed = Vec::new();
        for mut schedule in schedules {
            let run = ScheduleRun {
                id: Uuid::new_v4(),
                schedule_id: schedule.id,
                scheduled_at_ms: schedule.next_run_at_ms.expect("due row has a timestamp"),
                started_at_ms: now,
                finished_at_ms: None,
                status: ScheduleRunStatus::Running,
                error: None,
            };
            tx.execute("INSERT INTO schedule_runs (id, schedule_id, scheduled_at_ms, started_at_ms, status) VALUES (?1, ?2, ?3, ?4, 'running')",
                params![run.id.to_string(), schedule.id.to_string(), run.scheduled_at_ms, now])?;
            // Prune on claim as interrupted runs may never reach completion.
            tx.execute("DELETE FROM schedule_runs WHERE schedule_id = ?1 AND status != 'running' AND id NOT IN
                (SELECT id FROM schedule_runs WHERE schedule_id = ?1 ORDER BY rowid DESC LIMIT 100)", [schedule.id.to_string()])?;
            schedule.next_run_at_ms = None;
            if matches!(schedule.spec.timing, ScheduleTiming::Once { .. }) {
                schedule.spec.enabled = false;
            }
            write(&tx, &schedule)?;
            claimed.push((schedule, run));
        }
        tx.commit()?;
        Ok(claimed)
    }

    /// Complete a still-running claim and calculate its schedule's next occurrence.
    ///
    /// `finished` is the operation's completion time; `now` is the persistence
    /// attempt time, both in Unix milliseconds. An error records failure. `disable`
    /// prevents future work for a missing VM. Repeated completion writes are inert.
    /// A user pause is preserved, and occurrences before `now` are not replayed.
    pub(crate) fn finish_schedule_run(
        &self,
        run: &ScheduleRun,
        finished: i64,
        error: Option<String>,
        disable: bool,
        now: i64,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute("UPDATE schedule_runs SET finished_at_ms = ?2, status = ?3, error = ?4 WHERE id = ?1 AND status = 'running'",
            params![run.id.to_string(), finished, if error.is_some() { "failed" } else { "succeeded" }, error])?;
        if changed != 0
            && let Some(mut schedule) = get(&tx, run.schedule_id)?
        {
            if disable {
                schedule.spec.enabled = false;
            }
            schedule.next_run_at_ms = next(&tx, &schedule, now)?;
            if schedule.next_run_at_ms.is_none() {
                schedule.spec.enabled = false;
            }
            write(&tx, &schedule)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Unknown side effects from a previous process are recorded, never replayed.
    /// Mark active claims interrupted at Unix millisecond `now` and advance their
    /// schedules to a future occurrence. Unclaimed overdue schedules remain due.
    pub(crate) fn recover_schedules(&self, now: i64) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let interrupted = {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT schedule_id FROM schedule_runs WHERE status = 'running'",
            )?;
            stmt.query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        tx.execute("UPDATE schedule_runs SET status = 'interrupted', finished_at_ms = ?1,
            error = 'API restarted during execution; result is unknown; not retried' WHERE status = 'running'", [now])?;
        for id in interrupted {
            if let Some(mut schedule) = get(&tx, Uuid::parse_str(&id).map_err(corrupt)?)? {
                schedule.next_run_at_ms = next(&tx, &schedule, now)?;
                if schedule.next_run_at_ms.is_none() {
                    schedule.spec.enabled = false;
                }
                write(&tx, &schedule)?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

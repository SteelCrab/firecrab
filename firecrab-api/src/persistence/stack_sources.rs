//! Atomic Git source generations and bounded, redacted observation history.

use firecrab_stack::{ConfigureGitSource, GitCheck, GitCheckState, GitSourceStatus};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use super::{PersistenceError, Store};

pub(super) const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS stack_git_sources (
    stack_id TEXT PRIMARY KEY REFERENCES stacks(id),
    status TEXT NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS stack_git_checks (
    id TEXT PRIMARY KEY,
    stack_id TEXT NOT NULL REFERENCES stacks(id),
    status TEXT NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS stack_git_check_history ON stack_git_checks(stack_id);
";

impl Store {
    /// Configure only the exact observed generation. Network access happens later.
    pub(crate) fn configure_stack_source(
        &self,
        name: &str,
        request: &ConfigureGitSource,
        now: i64,
    ) -> Result<GitSourceStatus, PersistenceError> {
        if request.source.validate().is_err() {
            return Err(PersistenceError::InvalidStackSource);
        }
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record = super::stacks::read_stack(&tx, name)?.ok_or(PersistenceError::MissingStack)?;
        let previous = read(&tx, record.id)?;
        let generation = previous.as_ref().map_or(0, |s| s.generation);
        if generation != request.expected_generation {
            return Err(PersistenceError::StaleStackSource);
        }
        if let Some(old) = &previous {
            if old.source == request.source {
                return Ok(old.clone());
            }
            ensure_idle(&tx, record.id)?;
        }
        let status = GitSourceStatus {
            stack_id: record.id,
            name: record.name,
            generation: generation
                .checked_add(1)
                .ok_or(PersistenceError::StaleStackSource)?,
            source: request.source.clone(),
            next_check_at: now,
            failures: 0,
            latest: None,
        };
        tx.execute("INSERT INTO stack_git_sources (stack_id,status) VALUES (?1,?2) ON CONFLICT(stack_id) DO UPDATE SET status=excluded.status",
            params![status.stack_id.to_string(), encode(&status)?])?;
        tx.commit()?;
        Ok(status)
    }

    /// Read one source without reading credentials or declaration values.
    pub(crate) fn stack_source(&self, name: &str) -> Result<GitSourceStatus, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let record = super::stacks::read_stack(&tx, name)?.ok_or(PersistenceError::MissingStack)?;
        let source = read(&tx, record.id)?.ok_or(PersistenceError::MissingStackSource)?;
        tx.commit()?;
        Ok(source)
    }

    /// List source snapshots for the scheduler. Admission rechecks pause, time and generation.
    pub(crate) fn stack_sources(&self) -> Result<Vec<GitSourceStatus>, PersistenceError> {
        let conn = self.lock();
        let mut statement =
            conn.prepare("SELECT status FROM stack_git_sources ORDER BY stack_id")?;
        let rows = statement.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| decode(&r?)).collect()
    }

    /// Claim a check durably. Concurrent manual requests share the same active observation.
    pub(crate) fn begin_stack_check(
        &self,
        name: &str,
        generation: u64,
        now: i64,
        periodic: bool,
    ) -> Result<Option<(GitSourceStatus, bool)>, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record = super::stacks::read_stack(&tx, name)?.ok_or(PersistenceError::MissingStack)?;
        let mut source = read(&tx, record.id)?.ok_or(PersistenceError::MissingStackSource)?;
        if source.generation != generation {
            return Err(PersistenceError::StaleStackSource);
        }
        if periodic && (source.source.paused || source.next_check_at > now) {
            return Ok(None);
        }
        if source
            .latest
            .as_ref()
            .is_some_and(|c| c.state == GitCheckState::Checking)
        {
            return Ok(Some((source, false)));
        }
        let check = GitCheck {
            id: Uuid::new_v4(),
            generation,
            source: source.source.clone(),
            started_at: now,
            finished_at: None,
            state: GitCheckState::Checking,
            commit: None,
            blob: None,
            desired_hash: None,
            plan: None,
            error: None,
        };
        source.latest = Some(check.clone());
        tx.execute(
            "INSERT INTO stack_git_checks (id,stack_id,status) VALUES (?1,?2,?3)",
            params![check.id.to_string(), record.id.to_string(), encode(&check)?],
        )?;
        save(&tx, &source)?;
        tx.commit()?;
        Ok(Some((source, true)))
    }

    /// Save only the current active check. An old worker cannot overwrite a new generation.
    pub(crate) fn finish_stack_check(
        &self,
        stack_id: Uuid,
        check: &GitCheck,
        now: i64,
    ) -> Result<(), PersistenceError> {
        if check.state == GitCheckState::Checking || check.finished_at.is_none() {
            return Err(PersistenceError::InvalidStackSource);
        }
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut source = read(&tx, stack_id)?.ok_or(PersistenceError::MissingStackSource)?;
        if source.generation != check.generation
            || !source
                .latest
                .as_ref()
                .is_some_and(|c| c.id == check.id && c.state == GitCheckState::Checking)
        {
            return Err(PersistenceError::StaleStackSource);
        }
        source.failures = if matches!(
            check.state,
            GitCheckState::Failed | GitCheckState::Interrupted
        ) {
            source.failures.saturating_add(1)
        } else {
            0
        };
        let interval = i64::from(source.source.poll_interval_seconds);
        let delay = interval
            .saturating_mul(1_i64 << source.failures.min(6))
            .min(86400);
        source.next_check_at = now.saturating_add(delay);
        source.latest = Some(check.clone());
        tx.execute(
            "UPDATE stack_git_checks SET status=?1 WHERE id=?2 AND stack_id=?3",
            params![encode(check)?, check.id.to_string(), stack_id.to_string()],
        )?;
        // Only observations are evicted. They never authorize mutations or own retry identities.
        tx.execute("DELETE FROM stack_git_checks WHERE stack_id=?1 AND rowid NOT IN (SELECT rowid FROM stack_git_checks WHERE stack_id=?1 ORDER BY rowid DESC LIMIT 50)",
            [stack_id.to_string()])?;
        save(&tx, &source)?;
        tx.commit()?;
        Ok(())
    }

    /// Newest first, at most fifty observations. Configuration changes keep earlier generations.
    pub(crate) fn stack_check_history(
        &self,
        name: &str,
    ) -> Result<Vec<GitCheck>, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let record = super::stacks::read_stack(&tx, name)?.ok_or(PersistenceError::MissingStack)?;
        let mut statement = tx.prepare(
            "SELECT status FROM stack_git_checks WHERE stack_id=?1 ORDER BY rowid DESC LIMIT 50",
        )?;
        let rows = statement.query_map([record.id.to_string()], |r| r.get::<_, String>(0))?;
        let history = rows.map(|r| decode(&r?)).collect::<Result<_, _>>()?;
        drop(statement);
        tx.commit()?;
        Ok(history)
    }

    /// Read-only checks can safely run again after restart, but never claim a previous success.
    pub(crate) fn interrupt_stack_checks(&self, now: i64) -> Result<(), PersistenceError> {
        for source in self.stack_sources()? {
            if let Some(mut check) = source.latest.filter(|c| c.state == GitCheckState::Checking) {
                check.state = GitCheckState::Interrupted;
                check.finished_at = Some(now);
                check.error = Some("server_restarted".into());
                self.finish_stack_check(source.stack_id, &check, now)?;
            }
        }
        Ok(())
    }
}

pub(super) fn ensure_idle(conn: &Connection, stack_id: Uuid) -> Result<(), PersistenceError> {
    if read(conn, stack_id)?
        .is_some_and(|s| s.latest.is_some_and(|c| c.state == GitCheckState::Checking))
    {
        return Err(PersistenceError::StackSourceBusy);
    }
    Ok(())
}

fn read(conn: &Connection, id: Uuid) -> Result<Option<GitSourceStatus>, PersistenceError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT status FROM stack_git_sources WHERE stack_id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|s| decode(&s)).transpose()
}

fn save(conn: &Connection, source: &GitSourceStatus) -> Result<(), PersistenceError> {
    conn.execute(
        "UPDATE stack_git_sources SET status=?1 WHERE stack_id=?2",
        params![encode(source)?, source.stack_id.to_string()],
    )?;
    Ok(())
}

fn encode(value: &impl serde::Serialize) -> Result<String, PersistenceError> {
    serde_json::to_string(value).map_err(|_| corrupt())
}

fn decode<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, PersistenceError> {
    serde_json::from_str(raw).map_err(|_| corrupt())
}

fn corrupt() -> PersistenceError {
    PersistenceError::CorruptRecord {
        id: "stack_git_source".into(),
        reason: "invalid redacted Git source record".into(),
    }
}

#[cfg(test)]
mod tests;

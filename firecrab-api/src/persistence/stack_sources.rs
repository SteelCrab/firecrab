//! Atomic Git source generations and bounded, redacted observation history.

use firecrab_stack::{
    ApplyState, ApplyStatus, ConfigureGitSource, GitCheck, GitCheckState, GitSourceStatus,
    GitSyncMode,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use super::{PersistenceError, Store};

mod removal;

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

/// Explicit checks stay read-only, even for an automatic source.
#[derive(Clone, Copy)]
pub(crate) enum SourceTrigger {
    Check,
    Poll,
    Sync(Option<Uuid>),
}

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
        if !super::iac_owners::public_write_allowed(
            &tx,
            &firecrab_stack::ManagedResource {
                kind: "stack_source".into(),
                id: record.id.to_string(),
            },
        )? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        let previous = read(&tx, record.id)?;
        let generation = removal::generation(&tx, record.id)?;
        if generation != request.expected_generation {
            return Err(PersistenceError::StaleStackSource);
        }
        if let Some(old) = &previous {
            if old.source == request.source {
                return Ok(old.clone());
            }
            ensure_idle(&tx, record.id)?;
        }
        super::stack_applies::ensure_idle(&tx, record.id)?;
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
            desired_revision: None,
            attempted_revision: previous.as_ref().and_then(|s| s.attempted_revision.clone()),
            successful_revision: previous
                .as_ref()
                .and_then(|s| s.successful_revision.clone()),
            last_apply: previous.as_ref().and_then(|s| s.last_apply.clone()),
            recovery_operation_id: previous.as_ref().and_then(|s| s.recovery_operation_id),
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
        trigger: SourceTrigger,
    ) -> Result<Option<(GitSourceStatus, bool)>, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record = super::stacks::read_stack(&tx, name)?.ok_or(PersistenceError::MissingStack)?;
        let mut source = read(&tx, record.id)?.ok_or(PersistenceError::MissingStackSource)?;
        if source.generation != generation {
            return Err(PersistenceError::StaleStackSource);
        }
        let periodic = matches!(trigger, SourceTrigger::Poll);
        if periodic && (source.source.paused || source.next_check_at > now) {
            return Ok(None);
        }
        if source.latest.as_ref().is_some_and(|c| c.state.is_active()) {
            if matches!(trigger, SourceTrigger::Sync(_)) && !source.latest.as_ref().unwrap().sync {
                return Err(PersistenceError::StackSourceBusy);
            }
            return Ok(Some((source, false)));
        }
        let sync = matches!(trigger, SourceTrigger::Sync(_))
            || (periodic && source.source.sync_mode == GitSyncMode::Automatic);
        let recovery = match trigger {
            SourceTrigger::Sync(recovery) => recovery,
            _ => None,
        };
        if sync && (recovery != source.recovery_operation_id) {
            if periodic {
                return Ok(None);
            }
            return Err(PersistenceError::StackSyncRecoveryRequired);
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
            sync,
            recover_operation_id: recovery,
            operation_id: None,
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
        if check.state.is_active() || check.finished_at.is_none() {
            return Err(PersistenceError::InvalidStackSource);
        }
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut source = read(&tx, stack_id)?.ok_or(PersistenceError::MissingStackSource)?;
        if source.generation != check.generation
            || !source.latest.as_ref().is_some_and(|c| {
                c.id == check.id
                    && c.state == GitCheckState::Checking
                    && c.sync == check.sync
                    && c.recover_operation_id == check.recover_operation_id
            })
        {
            return Err(PersistenceError::StaleStackSource);
        }
        if check.sync && check.state == GitCheckState::InSync {
            source.successful_revision = check.revision();
            source.recovery_operation_id = None;
        }
        finish(&tx, &mut source, check, now)?;
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
            if source
                .latest
                .as_ref()
                .is_some_and(|c| c.state == GitCheckState::Applying)
            {
                self.refresh_stack_sync(&source.name, now)?;
                continue;
            }
            if let Some(mut check) = source.latest.filter(|c| c.state == GitCheckState::Checking) {
                check.state = GitCheckState::Interrupted;
                check.finished_at = Some(now);
                check.error = Some("server_restarted".into());
                self.finish_stack_check(source.stack_id, &check, now)?;
            }
        }
        Ok(())
    }

    /// Read the linked apply journal. Restart recovery never starts another worker.
    pub(crate) fn refresh_stack_sync(
        &self,
        name: &str,
        now: i64,
    ) -> Result<GitSourceStatus, PersistenceError> {
        let snapshot = self.stack_source(name)?;
        if !snapshot
            .latest
            .as_ref()
            .is_some_and(|c| c.state == GitCheckState::Applying)
        {
            return Ok(snapshot);
        }
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record = super::stacks::read_stack(&tx, name)?.ok_or(PersistenceError::MissingStack)?;
        let mut source = read(&tx, record.id)?.ok_or(PersistenceError::MissingStackSource)?;
        let Some(mut check) = source
            .latest
            .clone()
            .filter(|c| c.state == GitCheckState::Applying)
        else {
            return Ok(source);
        };
        let id = check.operation_id.ok_or_else(corrupt)?;
        let apply = super::stack_applies::read(&tx, id)?
            .ok_or_else(corrupt)?
            .status;
        if apply.stack_id != source.stack_id || apply.git_revision != check.revision() {
            return Err(corrupt());
        }
        source.last_apply = Some(apply.clone());
        if apply.state.is_terminal() {
            check.finished_at = Some(now);
            if apply.state == ApplyState::Succeeded {
                check.state = GitCheckState::Applied;
                source.successful_revision = apply.git_revision.clone();
                source.recovery_operation_id = None;
            } else {
                check.state = GitCheckState::RecoveryRequired;
                check.error = Some("apply_recovery_required".into());
                source.recovery_operation_id = Some(id);
            }
            finish(&tx, &mut source, &check, now)?;
        } else {
            save(&tx, &source)?;
        }
        tx.commit()?;
        Ok(source)
    }
}

/// Runs inside the same transaction that journals apply admission and reserves UUIDs.
pub(super) fn admit_apply(
    conn: &Connection,
    stack_id: Uuid,
    check: &GitCheck,
    status: &ApplyStatus,
) -> Result<(), PersistenceError> {
    let mut source = read(conn, stack_id)?.ok_or(PersistenceError::MissingStackSource)?;
    if !check.sync
        || check.id != status.operation_id
        || check.generation != source.generation
        || check.source != source.source
        || !source.latest.as_ref().is_some_and(|c| {
            c.id == check.id
                && c.state == GitCheckState::Checking
                && c.sync
                && c.recover_operation_id == check.recover_operation_id
        })
    {
        return Err(PersistenceError::StaleStackSource);
    }
    if source.recovery_operation_id != check.recover_operation_id {
        return Err(PersistenceError::StackSyncRecoveryRequired);
    }
    let revision = check
        .revision()
        .ok_or(PersistenceError::InvalidStackSource)?;
    let mut check = check.clone();
    check.state = GitCheckState::Applying;
    check.operation_id = Some(status.operation_id);
    source.desired_revision = Some(revision.clone());
    source.attempted_revision = Some(revision);
    source.last_apply = Some(status.clone());
    source.recovery_operation_id = None;
    source.latest = Some(check.clone());
    conn.execute(
        "UPDATE stack_git_checks SET status=?1 WHERE id=?2 AND stack_id=?3",
        params![encode(&check)?, check.id.to_string(), stack_id.to_string()],
    )?;
    save(conn, &source)
}

fn finish(
    conn: &Connection,
    source: &mut GitSourceStatus,
    check: &GitCheck,
    now: i64,
) -> Result<(), PersistenceError> {
    if let Some(revision) = check.revision() {
        source.desired_revision = Some(revision);
    }
    source.failures = if matches!(
        check.state,
        GitCheckState::Failed | GitCheckState::Interrupted | GitCheckState::RecoveryRequired
    ) {
        source.failures.saturating_add(1)
    } else {
        0
    };
    let delay = i64::from(source.source.poll_interval_seconds)
        .saturating_mul(1_i64 << source.failures.min(6))
        .min(86400);
    source.next_check_at = now.saturating_add(delay);
    source.latest = Some(check.clone());
    conn.execute(
        "UPDATE stack_git_checks SET status=?1 WHERE id=?2 AND stack_id=?3",
        params![
            encode(check)?,
            check.id.to_string(),
            source.stack_id.to_string()
        ],
    )?;
    conn.execute("DELETE FROM stack_git_checks WHERE stack_id=?1 AND rowid NOT IN (SELECT rowid FROM stack_git_checks WHERE stack_id=?1 ORDER BY rowid DESC LIMIT 50)", [source.stack_id.to_string()])?;
    save(conn, source)
}

pub(super) fn ensure_idle(conn: &Connection, stack_id: Uuid) -> Result<(), PersistenceError> {
    if read(conn, stack_id)?.is_some_and(|s| s.latest.is_some_and(|c| c.state.is_active())) {
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

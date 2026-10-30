//! Redacted apply journal and atomic prospective ownership reservations.

use firecrab_stack::{
    Action, ApplyFailure, ApplyState, ApplyStatus, GitCheck, StackRecord, StepState,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use super::{PersistenceError, Store};

mod lifecycle;

pub(super) const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS stack_applies (
    operation_id TEXT PRIMARY KEY,
    stack_id TEXT NOT NULL REFERENCES stacks(id),
    desired_hash TEXT NOT NULL,
    allow_stop INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('queued','running','succeeded','failed','interrupted')),
    status TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS one_active_stack_apply
ON stack_applies ((1)) WHERE state IN ('queued','running');
CREATE TABLE IF NOT EXISTS stack_apply_reservations (
    kind TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    operation_id TEXT NOT NULL REFERENCES stack_applies(operation_id) ON DELETE CASCADE,
    PRIMARY KEY (kind, resource_id)
) STRICT;";

/// Private retry identity, separate from the public status response.
pub(crate) struct StoredApply {
    /// Redacted public progress and exact resource identities.
    pub status: ApplyStatus,
    /// Hash of the exact canonical declaration, never its values.
    pub desired_hash: String,
    /// Interrupting-operation authorization captured at admission.
    pub allow_stop: bool,
}

impl Store {
    /// Find a globally unique operation ID for exact retry checking.
    pub(crate) fn stored_stack_apply(
        &self,
        id: Uuid,
    ) -> Result<Option<StoredApply>, PersistenceError> {
        read(&self.lock(), id)
    }

    /// Atomically journal an operation and reserve every prospective create UUID.
    /// Returns false for an identical retry; callers must not spawn a second worker.
    pub(crate) fn begin_stack_apply(
        &self,
        expected: &StackRecord,
        hash: &str,
        allow_stop: bool,
        status: &ApplyStatus,
        source: Option<&GitCheck>,
    ) -> Result<(ApplyStatus, bool), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(previous) = read(&tx, status.operation_id)? {
            if previous.status.stack_id != expected.id
                || previous.desired_hash != hash
                || previous.allow_stop != allow_stop
                || previous.status.git_revision != status.git_revision
            {
                return Err(PersistenceError::StackApplyIdentity);
            }
            return Ok((previous.status, false));
        }
        let current = super::stacks::read_stack(&tx, &expected.name)?
            .ok_or(PersistenceError::MissingStack)?;
        if &current != expected {
            return Err(PersistenceError::StaleStackPlan);
        }
        if source.is_none()
            && tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM stack_git_sources WHERE stack_id=?1)",
                [current.id.to_string()],
                |r| r.get::<_, bool>(0),
            )?
        {
            return Err(PersistenceError::StackSourceManaged);
        }
        let active: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM stack_applies WHERE state IN ('queued','running'))",
            [],
            |r| r.get(0),
        )?;
        if active {
            return Err(PersistenceError::StackApplyBusy);
        }
        lifecycle::validate(&tx, &current, status)?;
        let creates = status
            .changes
            .iter()
            .filter(|s| s.change.action == Action::Create && s.replacement_for.is_none())
            .collect::<Vec<_>>();
        let pruned = status
            .changes
            .iter()
            .filter(|s| {
                s.change.action == Action::Delete
                    && !status.changes.iter().any(|new| {
                        new.replacement_for == s.change.resource_id
                            && new.change.kind == s.change.kind
                            && new.change.key == s.change.key
                    })
            })
            .count();
        if current.bindings.len() + creates.len() - pruned > firecrab_stack::MAX_RESOURCES {
            return Err(PersistenceError::StackBindingLimit);
        }
        tx.execute("INSERT INTO stack_applies (operation_id,stack_id,desired_hash,allow_stop,state,status) VALUES (?1,?2,?3,?4,'queued',?5)",
            params![status.operation_id.to_string(), current.id.to_string(), hash, allow_stop, encode(status)?])?;
        lifecycle::reserve(&tx, status)?;
        for step in &creates {
            let change = &step.change;
            let id = change
                .resource_id
                .ok_or(PersistenceError::StackApplyIdentity)?;
            if id.is_nil() || !firecrab_stack::logical_name(&change.key) {
                return Err(PersistenceError::StackApplyIdentity);
            }
            super::iac_owners::native_binding_changed(&tx, change.kind.as_str(), id)?;
            tx.execute("INSERT INTO stack_bindings (stack_id,kind,logical_key,resource_id) VALUES (?1,?2,?3,?4)",
                params![current.id.to_string(), change.kind.as_str(), change.key, id.to_string()])?;
        }
        if !creates.is_empty() {
            tx.execute(
                "UPDATE stacks SET revision = revision + 1 WHERE id = ?1",
                [current.id.to_string()],
            )?;
        }
        if let Some(check) = source {
            if check.desired_hash.as_deref() != Some(hash)
                || check.source.allow_stop != allow_stop
                || status.git_revision != check.revision()
                || check
                    .plan
                    .as_ref()
                    .is_none_or(|plan| plan.lifecycle != status.lifecycle)
                || check
                    .plan
                    .as_ref()
                    .is_none_or(|plan| plan.inputs != status.inputs)
            {
                return Err(PersistenceError::StackApplyIdentity);
            }
            super::stack_sources::admit_apply(&tx, current.id, check, status)?;
        }
        tx.commit()?;
        Ok((status.clone(), true))
    }

    /// Update only a still-active operation; an interrupted worker cannot revive its journal.
    pub(crate) fn save_stack_apply(&self, status: &ApplyStatus) -> Result<bool, PersistenceError> {
        let conn = self.lock();
        let Some(previous) = read(&conn, status.operation_id)? else {
            return Ok(false);
        };
        let before = previous.status;
        if before.state.is_terminal() {
            return Ok(false);
        }
        if before.stack_id != status.stack_id
            || before.name != status.name
            || before.lifecycle != status.lifecycle
            || before.inputs != status.inputs
            || before.git_revision != status.git_revision
            || before.changes.len() != status.changes.len()
            || before.changes.iter().zip(&status.changes).any(|(a, b)| {
                a.change != b.change
                    || a.replacement_for != b.replacement_for
                    || a.vm_disk != b.vm_disk
            })
        {
            return Err(PersistenceError::StackApplyIdentity);
        }
        Ok(conn.execute("UPDATE stack_applies SET state=?1,status=?2 WHERE operation_id=?3 AND stack_id=?4 AND state IN ('queued','running')",
            params![status.state.as_str(), encode(status)?, status.operation_id.to_string(), status.stack_id.to_string()])? == 1)
    }

    /// Mark unfinished work interrupted at API startup; never replay mutations implicitly.
    pub(crate) fn interrupt_stack_applies(&self) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement =
            tx.prepare("SELECT status FROM stack_applies WHERE state IN ('queued','running')")?;
        let records = statement
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for raw in records {
            let mut status: ApplyStatus = serde_json::from_str(&raw).map_err(|_| corrupt())?;
            status.state = ApplyState::Interrupted;
            status.error = Some(ApplyFailure {
                code: "server_restarted".into(),
                message:
                    "inspect the recorded UUIDs before explicit recovery; work was not replayed"
                        .into(),
                fields: vec![],
            });
            for step in &mut status.changes {
                if step.state == StepState::Pending {
                    step.state = StepState::Skipped;
                }
            }
            tx.execute(
                "UPDATE stack_applies SET state='interrupted',status=?1 WHERE operation_id=?2",
                params![encode(&status)?, status.operation_id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

pub(super) fn ensure_idle(conn: &Connection, stack_id: Uuid) -> Result<(), PersistenceError> {
    let active: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM stack_applies WHERE stack_id=?1 AND state IN ('queued','running'))", [stack_id.to_string()], |r| r.get(0))?;
    if active {
        Err(PersistenceError::StackApplyBusy)
    } else {
        Ok(())
    }
}

pub(super) fn read(conn: &Connection, id: Uuid) -> Result<Option<StoredApply>, PersistenceError> {
    let row: Option<(String, bool, String)> = conn
        .query_row(
            "SELECT desired_hash,allow_stop,status FROM stack_applies WHERE operation_id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    row.map(|(desired_hash, allow_stop, raw)| {
        Ok(StoredApply {
            status: serde_json::from_str(&raw).map_err(|_| corrupt())?,
            desired_hash,
            allow_stop,
        })
    })
    .transpose()
}

fn encode(status: &ApplyStatus) -> Result<String, PersistenceError> {
    serde_json::to_string(status).map_err(|_| corrupt())
}

fn corrupt() -> PersistenceError {
    PersistenceError::CorruptRecord {
        id: "stack_apply".into(),
        reason: "invalid redacted operation record".into(),
    }
}

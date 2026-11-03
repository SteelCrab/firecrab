//! Durable native mutation fences. Unknown effects never release authority by timeout.

use super::{PersistenceError, Store};
use firecrab_stack::{ManagedResource, NativeAction, NativeOperation, NativeOperationState};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "
CREATE UNIQUE INDEX IF NOT EXISTS iac_native_active_resource
 ON iac_native_operations(kind,resource_id) WHERE state IN ('running','needs_recovery');
CREATE TABLE IF NOT EXISTS iac_native_dependencies (
 operation_id TEXT NOT NULL REFERENCES iac_native_operations(operation_id),
 kind TEXT NOT NULL, resource_id TEXT NOT NULL, ownership TEXT NOT NULL,
 PRIMARY KEY(operation_id,kind,resource_id)
) STRICT;
CREATE INDEX IF NOT EXISTS iac_native_dependency_resource ON iac_native_dependencies(kind,resource_id);
CREATE TABLE IF NOT EXISTS iac_resource_plans (
 operation_id TEXT PRIMARY KEY, request_hash TEXT NOT NULL, definition TEXT NOT NULL, receipt TEXT
) STRICT;
CREATE TRIGGER IF NOT EXISTS iac_plan_native_identity BEFORE INSERT ON iac_native_operations
 WHEN EXISTS(SELECT 1 FROM iac_resource_plans WHERE operation_id=NEW.operation_id)
 AND json_extract(NEW.status,'$.action')<>'apply_resource_plan'
 BEGIN SELECT RAISE(ABORT,'iac_operation_identity_conflict'); END;
CREATE TRIGGER IF NOT EXISTS iac_native_plan_identity BEFORE INSERT ON iac_resource_plans
 WHEN EXISTS(SELECT 1 FROM iac_native_operations WHERE operation_id=NEW.operation_id)
 BEGIN SELECT RAISE(ABORT,'iac_operation_identity_conflict'); END;";

/// A delayed request cannot advance a newer uncertain attempt after another recovery already failed.
pub(super) fn check_recovery_attempt(
    operation: &NativeOperation,
    expected: u64,
) -> Result<(), PersistenceError> {
    if operation.execution_generation != expected {
        return Err(PersistenceError::StaleIacExecution);
    }
    Ok(())
}

pub(super) fn ensure_idle(
    conn: &Connection,
    resource: &ManagedResource,
) -> Result<(), PersistenceError> {
    let active:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM iac_native_operations o WHERE o.state IN ('running','needs_recovery') AND
        ((o.kind=?1 AND o.resource_id=?2) OR EXISTS(SELECT 1 FROM iac_native_dependencies d
         WHERE d.operation_id=o.operation_id AND d.kind=?1 AND d.resource_id=?2)))",
        params![resource.kind,resource.id],|row|row.get(0))?;
    if active {
        return Err(PersistenceError::IacResourceBusy);
    }
    Ok(())
}

/// Stack admission and native admission serialize in SQLite, including unrendered facets.
pub(super) fn ensure_stack_idle(conn: &Connection, stack: Uuid) -> Result<(), PersistenceError> {
    let busy: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM iac_native_operations
        WHERE state IN ('running','needs_recovery') AND json_extract(status,'$.ownership.owner.manager')='stack'
        AND json_extract(status,'$.ownership.owner.id')=?1)
        OR EXISTS(SELECT 1 FROM iac_native_dependencies d JOIN iac_native_operations o
        ON o.operation_id=d.operation_id WHERE o.state IN ('running','needs_recovery')
        AND json_extract(d.ownership,'$.owner.manager')='stack' AND json_extract(d.ownership,'$.owner.id')=?1)", [stack.to_string()], |row|row.get(0))?;
    if busy {
        return Err(PersistenceError::IacResourceBusy);
    }
    Ok(())
}

/// A completed destructive operation retires the identity for late creation/admission.
pub(super) fn ensure_project_available(
    conn: &Connection,
    project: Uuid,
) -> Result<(), PersistenceError> {
    ensure_idle(
        conn,
        &ManagedResource {
            kind: "sandbox_project".into(),
            id: project.to_string(),
        },
    )?;
    let retired:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM iac_native_operations WHERE kind='sandbox_project' AND resource_id=?1 AND state='succeeded' AND json_extract(status,'$.action')='delete_sandbox_project')",[project.to_string()],|row|row.get(0))?;
    if retired {
        return Err(PersistenceError::IacOperationIdentity);
    }
    Ok(())
}

pub(super) fn read(
    conn: &Connection,
    id: Uuid,
) -> Result<Option<NativeOperation>, PersistenceError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT status FROM iac_native_operations WHERE operation_id=?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if raw.is_none()
        && conn
            .prepare("SELECT 1 FROM iac_resource_plans WHERE operation_id=?1")?
            .exists([id.to_string()])?
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    raw.map(|raw| serde_json::from_str(&raw).map_err(|_| PersistenceError::IacOperationIdentity))
        .transpose()
}

pub(super) fn save(conn: &Connection, operation: &NativeOperation) -> Result<(), PersistenceError> {
    let state = match operation.state {
        NativeOperationState::Running => "running",
        NativeOperationState::NeedsRecovery => "needs_recovery",
        NativeOperationState::Succeeded => "succeeded",
        NativeOperationState::Failed => "failed",
    };
    let status =
        serde_json::to_string(operation).map_err(|_| PersistenceError::IacOperationIdentity)?;
    conn.execute("INSERT INTO iac_native_operations(operation_id,kind,resource_id,state,status) VALUES(?1,?2,?3,?4,?5)
        ON CONFLICT(operation_id) DO UPDATE SET state=excluded.state,status=excluded.status",
        params![operation.operation_id.to_string(),operation.ownership.resource.kind,operation.ownership.resource.id,state,status])?;
    Ok(())
}

fn project_idle(conn: &Connection, id: Uuid) -> Result<(), PersistenceError> {
    let busy:bool=conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM ai_sandbox_task_index WHERE project_id=?1 AND busy=1)
         OR EXISTS(SELECT 1 FROM ai_sandbox_environments WHERE project_id=?1 AND state<>'deleted')
         OR EXISTS(SELECT 1 FROM ai_sandbox_tunnels WHERE project_id=?1 AND (json_extract(spec,'$.state') NOT IN ('deleted','superseded') OR (json_extract(spec,'$.successorBindingId') IS NOT NULL AND json_extract(spec,'$.handoffConfirmedAt') IS NULL)))",
        [id.to_string()],|row|row.get(0))?;
    if busy {
        return Err(PersistenceError::IacResourceBusy);
    }
    Ok(())
}

impl Store {
    /// Observe recorded work without replaying it.
    pub(crate) fn iac_native_operation(
        &self,
        id: Uuid,
    ) -> Result<Option<NativeOperation>, PersistenceError> {
        read(&self.lock(), id)
    }

    /// Recover operation identities for a known resource after losing the HTTP acknowledgement.
    pub(crate) fn iac_native_operations(
        &self,
        resource: &ManagedResource,
    ) -> Result<Vec<NativeOperation>, PersistenceError> {
        let conn = self.lock();
        let mut statement=conn.prepare("SELECT status FROM iac_native_operations WHERE kind=?1 AND resource_id=?2 ORDER BY rowid DESC")?;
        statement
            .query_map(params![resource.kind, resource.id], |row| {
                row.get::<_, String>(0)
            })?
            .map(|raw| {
                serde_json::from_str(&raw?).map_err(|_| PersistenceError::IacOperationIdentity)
            })
            .collect()
    }

    /// Reserve authority before filesystem dispatch. The target comes from the typed action.
    pub(crate) fn begin_sandbox_project_delete(
        &self,
        project: Uuid,
        id: Uuid,
    ) -> Result<NativeOperation, PersistenceError> {
        if project.is_nil() || id.is_nil() {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let resource = ManagedResource {
            kind: "sandbox_project".into(),
            id: project.to_string(),
        };
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = read(&tx, id)? {
            if existing.ownership.resource != resource
                || existing.action != NativeAction::DeleteSandboxProject
            {
                return Err(PersistenceError::IacOperationIdentity);
            }
            return Ok(existing);
        }
        if !super::iac_owners::public_write_allowed(&tx, &resource)? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_sandbox_projects WHERE id=?1)",
            [project.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(PersistenceError::InvalidStackResource);
        }
        project_idle(&tx, project)?;
        let operation = NativeOperation {
            operation_id: id,
            ownership: super::iac_owners::read(&tx, &resource)?,
            action: NativeAction::DeleteSandboxProject,
            state: NativeOperationState::Running,
            error_code: None,
            execution_generation: 0,
            configuration_generation: None,
            retain_disks: None,
        };
        save(&tx, &operation)?;
        tx.commit()?;
        Ok(operation)
    }

    /// Persist an uncertain outcome. A later request must recover this same UUID.
    #[cfg(test)]
    pub(crate) fn fail_native_project_delete(&self, id: Uuid) -> Result<(), PersistenceError> {
        let operation = self
            .iac_native_operation(id)?
            .ok_or(PersistenceError::IacOperationIdentity)?;
        self.fail_native_project_delete_at(&operation)
    }
    /// Only the exact admitted cleanup attempt can retain an uncertain outcome.
    pub(crate) fn fail_native_project_delete_at(
        &self,
        expected: &NativeOperation,
    ) -> Result<(), PersistenceError> {
        let id = expected.operation_id;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut operation = read(&tx, id)?.ok_or(PersistenceError::IacOperationIdentity)?;
        if operation.state != NativeOperationState::Succeeded && operation != *expected {
            return Err(PersistenceError::StaleIacOwner);
        }

        if operation.state != NativeOperationState::Succeeded {
            operation.state = NativeOperationState::NeedsRecovery;
            operation.error_code = Some("project_cleanup_incomplete".into());
            save(&tx, &operation)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Explicit recovery preserves endpoint, identity and ownership generation.
    #[cfg(test)]
    pub(crate) fn recover_native_project_delete(
        &self,
        id: Uuid,
    ) -> Result<NativeOperation, PersistenceError> {
        let generation = self
            .iac_native_operation(id)?
            .ok_or(PersistenceError::IacOperationIdentity)?
            .execution_generation;
        self.recover_native_project_delete_at(id, generation)
    }
    /// Compare the observed attempt inside the transaction before resuming its original work.
    pub(crate) fn recover_native_project_delete_at(
        &self,
        id: Uuid,
        expected_generation: u64,
    ) -> Result<NativeOperation, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut operation = read(&tx, id)?.ok_or(PersistenceError::IacOperationIdentity)?;
        if operation.state == NativeOperationState::Succeeded {
            return Ok(operation);
        }
        crate::persistence::iac_native::check_recovery_attempt(&operation, expected_generation)?;
        if operation.state != NativeOperationState::NeedsRecovery {
            return Err(PersistenceError::IacResourceBusy);
        }
        if super::iac_owners::read(&tx, &operation.ownership.resource)? != operation.ownership {
            return Err(PersistenceError::StaleIacOwner);
        }
        project_idle(
            &tx,
            Uuid::parse_str(&operation.ownership.resource.id)
                .map_err(|_| PersistenceError::IacOperationIdentity)?,
        )?;
        operation.execution_generation = operation
            .execution_generation
            .checked_add(1)
            .filter(|v| *v <= i64::MAX as u64)
            .ok_or(PersistenceError::IacOperationIdentity)?;
        operation.state = NativeOperationState::Running;
        operation.error_code = None;
        save(&tx, &operation)?;
        tx.commit()?;
        Ok(operation)
    }

    /// Delete project metadata and finish its fence atomically after filesystem confirmation.
    #[cfg(test)]
    pub(crate) fn finish_native_project_delete(
        &self,
        id: Uuid,
    ) -> Result<NativeOperation, PersistenceError> {
        let operation = self
            .iac_native_operation(id)?
            .ok_or(PersistenceError::IacOperationIdentity)?;
        self.finish_native_project_delete_at(&operation)
    }
    /// Commit project removal only for the original running attempt; keep terminal receipts immutable.
    pub(crate) fn finish_native_project_delete_at(
        &self,
        expected: &NativeOperation,
    ) -> Result<NativeOperation, PersistenceError> {
        let id = expected.operation_id;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut operation = read(&tx, id)?.ok_or(PersistenceError::IacOperationIdentity)?;
        if operation.state != NativeOperationState::Succeeded && operation != *expected {
            return Err(PersistenceError::StaleIacOwner);
        }

        if operation.state == NativeOperationState::Succeeded {
            return Ok(operation);
        }
        if operation.state != NativeOperationState::Running
            || operation.action != NativeAction::DeleteSandboxProject
            || super::iac_owners::read(&tx, &operation.ownership.resource)? != operation.ownership
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let project = Uuid::parse_str(&operation.ownership.resource.id)
            .map_err(|_| PersistenceError::IacOperationIdentity)?;
        project_idle(&tx, project)?;
        super::ai_sandbox::delete_project_rows(&tx, project)?;
        operation.state = NativeOperationState::Succeeded;
        operation.error_code = None;
        save(&tx, &operation)?;
        tx.commit()?;
        Ok(operation)
    }

    /// Startup does not infer success or replay filesystem work.
    pub(crate) fn interrupt_native_iac_operations(&self) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement =
            tx.prepare("SELECT operation_id FROM iac_native_operations WHERE state='running'")?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for id in ids {
            let id = Uuid::parse_str(&id).map_err(|_| PersistenceError::IacOperationIdentity)?;
            let mut operation = read(&tx, id)?.ok_or(PersistenceError::IacOperationIdentity)?;
            operation.state = NativeOperationState::NeedsRecovery;
            operation.error_code = Some("server_restarted".into());
            save(&tx, &operation)?;
        }
        tx.commit()?;
        Ok(())
    }
}

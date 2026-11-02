//! Reserve the broker identity and authority before any external work.

use super::{PersistenceError, Store};
use crate::queues::EngineRecord;
use firecrab_api_types::{CreateQueueEngineRequest, QueueEnginePhase};
use firecrab_stack::{ManagedResource, NativeAction, NativeOperation, NativeOperationState};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS queue_engine_requests (
 operation_id TEXT PRIMARY KEY, request_hash TEXT NOT NULL
) STRICT;";

fn load(conn: &Connection) -> Result<Option<EngineRecord>, PersistenceError> {
    let raw: Option<String> = conn
        .query_row("SELECT record FROM queue_engine WHERE id=1", [], |row| {
            row.get(0)
        })
        .optional()?;
    raw.map(|raw| super::queues::decode(&raw)).transpose()
}

fn save(conn: &Connection, record: &EngineRecord) -> Result<(), PersistenceError> {
    conn.execute(
        "INSERT INTO queue_engine(id,record) VALUES(1,?1)
        ON CONFLICT(id) DO UPDATE SET record=excluded.record",
        [super::queues::encode(record)?],
    )?;
    Ok(())
}

pub(crate) fn is_engine_action(action: NativeAction) -> bool {
    matches!(
        action,
        NativeAction::InstallQueueEngine
            | NativeAction::StartQueueEngine
            | NativeAction::StopQueueEngine
    )
}

fn phase(action: NativeAction) -> Result<QueueEnginePhase, PersistenceError> {
    match action {
        NativeAction::InstallQueueEngine => Ok(QueueEnginePhase::Installing),
        NativeAction::StartQueueEngine => Ok(QueueEnginePhase::Starting),
        NativeAction::StopQueueEngine => Ok(QueueEnginePhase::Stopping),
        _ => Err(PersistenceError::IacOperationIdentity),
    }
}

fn fingerprint(
    action: NativeAction,
    spec: Option<&CreateQueueEngineRequest>,
) -> Result<String, PersistenceError> {
    if (action == NativeAction::InstallQueueEngine) != spec.is_some() {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let bytes =
        serde_json::to_vec(&(action, spec)).map_err(|_| PersistenceError::IacOperationIdentity)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn current(
    conn: &Connection,
    operation: &NativeOperation,
) -> Result<EngineRecord, PersistenceError> {
    if !is_engine_action(operation.action) || operation.ownership.resource.kind != "queue_engine" {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let record = load(conn)?.ok_or(PersistenceError::InvalidStackResource)?;
    if operation.ownership.resource.id != record.vm_id.to_string() {
        return Err(PersistenceError::IacOperationIdentity);
    }
    if super::iac_owners::read(conn, &operation.ownership.resource)? != operation.ownership {
        return Err(PersistenceError::StaleIacOwner);
    }
    Ok(record)
}

impl Store {
    /// Commit the original VM, private credentials, authority and retry receipt together.
    /// The bool is true only for newly admitted work; an exact retry never dispatches again.
    pub(crate) fn begin_queue_engine_operation(
        &self,
        id: Uuid,
        action: NativeAction,
        create: Option<&EngineRecord>,
    ) -> Result<(EngineRecord, NativeOperation, bool), PersistenceError> {
        if id.is_nil() {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let requested_phase = phase(action)?;
        let hash = fingerprint(action, create.map(|record| &record.spec))?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(operation) = super::iac_native::read(&tx, id)? {
            let original: Option<String> = tx
                .query_row(
                    "SELECT request_hash FROM queue_engine_requests WHERE operation_id=?1",
                    [id.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            if operation.action != action || original.as_deref() != Some(&hash) {
                return Err(PersistenceError::IacOperationIdentity);
            }
            // Completed acknowledgements survive subsequent management transfers.
            let record = load(&tx)?.ok_or(PersistenceError::InvalidStackResource)?;
            if operation.ownership.resource.kind != "queue_engine"
                || operation.ownership.resource.id != record.vm_id.to_string()
            {
                return Err(PersistenceError::IacOperationIdentity);
            }
            tx.commit()?;
            return Ok((record, operation, false));
        }
        let existing = load(&tx)?;
        let mut record = match (existing, create) {
            (None, Some(record)) => record.clone(),
            (Some(record), None) => record,
            (Some(_), Some(_)) => return Err(PersistenceError::IacResourceBusy),
            (None, None) => return Err(PersistenceError::InvalidStackResource),
        };
        if record.vm_id.is_nil() {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let resource = ManagedResource {
            kind: "queue_engine".into(),
            id: record.vm_id.to_string(),
        };
        if !super::iac_owners::public_write_allowed(&tx, &resource)? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        if create.is_none() {
            super::iac_resources::validate(&tx, &resource, true)?;
        }
        let operation = NativeOperation {
            operation_id: id,
            ownership: super::iac_owners::read(&tx, &resource)?,
            action,
            state: NativeOperationState::Running,
            error_code: None,
            execution_generation: 0,
            configuration_generation: None,
        };
        record.phase = requested_phase;
        record.error = None;
        save(&tx, &record)?;
        super::iac_native::save(&tx, &operation)?;
        tx.execute(
            "INSERT INTO queue_engine_requests(operation_id,request_hash) VALUES(?1,?2)",
            params![id.to_string(), hash],
        )?;
        tx.commit()?;
        Ok((record, operation, true))
    }

    /// A stale or interrupted worker cannot rewrite a newer checkpoint or release its fence.
    pub(crate) fn checkpoint_queue_engine_operation(
        &self,
        expected: &NativeOperation,
        record: &EngineRecord,
        finished: bool,
    ) -> Result<NativeOperation, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut operation = super::iac_native::read(&tx, expected.operation_id)?
            .ok_or(PersistenceError::IacOperationIdentity)?;
        if !is_engine_action(operation.action) {
            return Err(PersistenceError::IacOperationIdentity);
        }
        if operation.state == NativeOperationState::Succeeded {
            return Err(PersistenceError::IacOperationIdentity);
        }
        if operation.state != NativeOperationState::Running {
            return Err(PersistenceError::IacResourceBusy);
        }
        if operation != *expected {
            return Err(PersistenceError::StaleIacOwner);
        }
        let previous = current(&tx, &operation)?;
        if previous.vm_id != record.vm_id
            || previous.spec != record.spec
            || previous.username != record.username
            || previous.password != record.password
            || (previous.vm_created && !record.vm_created)
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        if finished {
            let expected = if operation.action == NativeAction::StopQueueEngine {
                QueueEnginePhase::Stopped
            } else {
                QueueEnginePhase::Running
            };
            if record.phase == expected {
                operation.state = NativeOperationState::Succeeded;
                operation.error_code = None;
            } else if record.phase == QueueEnginePhase::Failed {
                operation.state = NativeOperationState::NeedsRecovery;
                operation.error_code = Some("queue_engine_incomplete".into());
            } else {
                return Err(PersistenceError::IacOperationIdentity);
            }
        } else if !matches!(
            record.phase,
            QueueEnginePhase::Installing | QueueEnginePhase::Starting | QueueEnginePhase::Stopping
        ) {
            return Err(PersistenceError::IacOperationIdentity);
        }
        save(&tx, record)?;
        super::iac_native::save(&tx, &operation)?;
        tx.commit()?;
        Ok(operation)
    }

    /// Resume the original action with its original VM, disk and credentials.
    pub(crate) fn recover_queue_engine_operation(
        &self,
        id: Uuid,
    ) -> Result<(EngineRecord, NativeOperation), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut operation =
            super::iac_native::read(&tx, id)?.ok_or(PersistenceError::IacOperationIdentity)?;
        let mut record = current(&tx, &operation)?;
        if operation.state == NativeOperationState::Succeeded {
            return Ok((record, operation));
        }
        if operation.state != NativeOperationState::NeedsRecovery {
            return Err(PersistenceError::IacResourceBusy);
        }
        operation.execution_generation = operation
            .execution_generation
            .checked_add(1)
            .ok_or(PersistenceError::IacOperationIdentity)?;
        record.phase = phase(operation.action)?;
        record.error = None;
        operation.state = NativeOperationState::Running;
        operation.error_code = None;
        save(&tx, &record)?;
        super::iac_native::save(&tx, &operation)?;
        tx.commit()?;
        Ok((record, operation))
    }
}

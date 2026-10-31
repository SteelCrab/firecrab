//! Source removal and its acknowledgement commit together; generation tombstones prevent ABA retries.
use super::{PersistenceError, Store, ensure_idle, read};
use crate::persistence::{iac_native, iac_owners, stack_applies, stacks};
use firecrab_stack::{
    DeleteGitSource, GitSourceGeneration, ManagedResource, NativeAction, NativeOperation,
    NativeOperationState,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use uuid::Uuid;

/// Removed generations remain observable even when no source is configured.
pub(super) fn generation(conn: &Connection, id: Uuid) -> Result<u64, PersistenceError> {
    if let Some(source) = read(conn, id)? {
        return Ok(source.generation);
    }
    let raw:Option<String>=conn.query_row("SELECT status FROM iac_native_operations WHERE kind='stack_source' AND resource_id=?1 AND state='succeeded' AND json_extract(status,'$.action')='delete_stack_source' ORDER BY rowid DESC LIMIT 1",
        [id.to_string()],|row|row.get(0)).optional()?;
    match raw {
        None => Ok(0),
        Some(raw) => serde_json::from_str::<NativeOperation>(&raw)
            .map_err(|_| PersistenceError::IacOperationIdentity)?
            .configuration_generation
            .and_then(|generation| generation.checked_add(1))
            .ok_or(PersistenceError::StaleStackSource),
    }
}

impl Store {
    /// Read an absent-source tombstone before recreating its configuration.
    pub(crate) fn stack_source_generation(
        &self,
        name: &str,
    ) -> Result<GitSourceGeneration, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let record = stacks::read_stack(&tx, name)?.ok_or(PersistenceError::MissingStack)?;
        Ok(GitSourceGeneration {
            generation: generation(&tx, record.id)?,
        })
    }

    /// Disconnect only the expected generation and preserve all bound infrastructure/history.
    pub(crate) fn remove_stack_source(
        &self,
        name: &str,
        request: &DeleteGitSource,
    ) -> Result<NativeOperation, PersistenceError> {
        if request.operation_id.is_nil()
            || request.expected_stack_id.is_nil()
            || request.expected_endpoint_id.is_nil()
            || request.expected_generation == 0
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let resource = ManagedResource {
            kind: "stack_source".into(),
            id: request.expected_stack_id.to_string(),
        };
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if iac_owners::endpoint(&tx)? != request.expected_endpoint_id {
            return Err(PersistenceError::IacEndpointConflict);
        }
        if let Some(existing) = iac_native::read(&tx, request.operation_id)? {
            if existing.action != NativeAction::DeleteStackSource
                || existing.ownership.resource != resource
                || existing.ownership.endpoint_id != request.expected_endpoint_id
                || existing.configuration_generation != Some(request.expected_generation)
            {
                return Err(PersistenceError::IacOperationIdentity);
            }
            return Ok(existing);
        }
        let record = stacks::read_stack(&tx, name)?.ok_or(PersistenceError::MissingStack)?;
        if record.id != request.expected_stack_id {
            return Err(PersistenceError::IacOperationIdentity);
        }
        if !iac_owners::public_write_allowed(&tx, &resource)? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        let source = read(&tx, record.id)?.ok_or(PersistenceError::MissingStackSource)?;
        if source.generation != request.expected_generation || source.generation == u64::MAX {
            return Err(PersistenceError::StaleStackSource);
        }
        ensure_idle(&tx, record.id)?;
        stack_applies::ensure_idle(&tx, record.id)?;
        let operation = NativeOperation {
            operation_id: request.operation_id,
            ownership: iac_owners::read(&tx, &resource)?,
            action: NativeAction::DeleteStackSource,
            state: NativeOperationState::Succeeded,
            error_code: None,
            configuration_generation: Some(source.generation),
        };
        tx.execute(
            "DELETE FROM stack_git_sources WHERE stack_id=?1",
            [record.id.to_string()],
        )?;
        iac_native::save(&tx, &operation)?;
        tx.commit()?;
        Ok(operation)
    }
}

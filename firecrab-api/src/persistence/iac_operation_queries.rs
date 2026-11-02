//! Bounded, redacted observations of active work; completion never expires a pending fence.
use super::{PersistenceError, Store};
use firecrab_stack::{NativeOperation, NativeOperationState};
use rusqlite::params;
use uuid::Uuid;

impl Store {
    /// Active work has no timeout-based expiry. Canonical UUID ordering is stable across VACUUM.
    pub(crate) fn active_native_operations(
        &self,
        before: Option<Uuid>,
        limit: u16,
    ) -> Result<firecrab_stack::NativeOperationsPage, PersistenceError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT operation_id,status FROM iac_native_operations
            WHERE state IN ('running','needs_recovery') AND (?1 IS NULL OR operation_id<?1)
            ORDER BY operation_id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![before.map(|id| id.to_string()), i64::from(limit) + 1],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        let mut operations = Vec::new();
        for row in rows {
            let (id, raw) = row?;
            let operation: NativeOperation =
                serde_json::from_str(&raw).map_err(|_| PersistenceError::IacOperationIdentity)?;
            if id != operation.operation_id.to_string()
                || operation.state == NativeOperationState::Succeeded
            {
                return Err(PersistenceError::IacOperationIdentity);
            }
            operations.push(operation);
        }
        let next_before = if operations.len() > usize::from(limit) {
            operations.pop();
            operations.last().map(|op| op.operation_id)
        } else {
            None
        };
        Ok(firecrab_stack::NativeOperationsPage {
            operations,
            next_before,
        })
    }
}

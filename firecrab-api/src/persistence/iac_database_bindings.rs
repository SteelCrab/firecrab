//! Binding intents reserve their account, service and database before remote grants.
use super::{
    PersistenceError, Store,
    database_services::{decode, encode},
    iac_native, iac_owners,
};
use crate::pipe_databases::{
    DatabaseInput, DatabaseRecord,
    bindings::{BindingInput, BindingRecord},
};
use firecrab_stack::{ManagedResource, NativeAction, NativeOperation, NativeOperationState};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

mod admission;
pub(super) use admission::{service_idle, validate_owner};

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS iac_database_binding_mutations (
 operation_id TEXT PRIMARY KEY REFERENCES iac_native_operations(operation_id),
 request_hash TEXT NOT NULL, definition TEXT NOT NULL, prepared TEXT,
 effects_started INTEGER NOT NULL DEFAULT 0 CHECK(effects_started IN (0,1))
) STRICT;";

#[derive(Serialize)]
pub(crate) enum BindingChange {
    Create { database: Uuid, input: BindingInput },
    Delete { id: Uuid },
}
#[derive(Clone, Serialize, Deserialize)]
struct Definition {
    database_id: Uuid,
    database: DatabaseInput,
    input: BindingInput,
    service_spec_hash: String,
    before: Option<BindingRecord>,
}
/// No public JSON can construct this server execution witness.
#[derive(Clone)]
pub(crate) struct BindingIntent {
    pub operation: NativeOperation,
    definition: Definition,
    request_hash: String,
}
impl BindingIntent {
    pub(crate) fn id(&self) -> Uuid {
        self.definition.input.request_id
    }
    pub(crate) fn database(&self) -> DatabaseRecord {
        DatabaseRecord {
            id: self.definition.database_id,
            input: self.definition.database.clone(),
        }
    }
    pub(crate) fn input(&self) -> &BindingInput {
        &self.definition.input
    }
}
pub(crate) enum Admission {
    Ready(Box<BindingIntent>, bool),
    Missing,
    Invalid(&'static str),
}
fn hash<T: Serialize>(value: &T) -> Result<String, PersistenceError> {
    Ok(format!("{:x}", Sha256::digest(encode(value)?)))
}
pub(super) fn resource(id: Uuid) -> ManagedResource {
    ManagedResource {
        kind: "database_binding".into(),
        id: id.to_string(),
    }
}
fn binding(conn: &Connection, id: Uuid) -> Result<Option<BindingRecord>, PersistenceError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT spec FROM database_bindings WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    raw.as_deref().map(decode).transpose()
}
fn service_spec(conn: &Connection, id: Uuid) -> Result<Option<String>, PersistenceError> {
    Ok(conn
        .query_row(
            "SELECT spec FROM micro_services WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?)
}
fn prepared(conn: &Connection, id: Uuid) -> Result<Option<BindingRecord>, PersistenceError> {
    let raw: Option<String> = conn.query_row(
        "SELECT prepared FROM iac_database_binding_mutations WHERE operation_id=?1",
        [id.to_string()],
        |r| r.get(0),
    )?;
    raw.as_deref().map(decode).transpose()
}
fn read(conn: &Connection, id: Uuid) -> Result<Option<BindingIntent>, PersistenceError> {
    let Some(operation) = iac_native::read(conn, id)? else {
        return Ok(None);
    };
    if operation.operation_id != id
        || !matches!(
            operation.action,
            NativeAction::CreateDatabaseBinding | NativeAction::DeleteDatabaseBinding
        )
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let (request_hash, raw): (String, String) = conn.query_row(
        "SELECT request_hash,definition FROM iac_database_binding_mutations WHERE operation_id=?1",
        [id.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let definition: Definition = decode(&raw)?;
    if definition.input.request_id.is_nil()
        || operation.ownership.resource != resource(definition.input.request_id)
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    Ok(Some(BindingIntent {
        operation,
        definition,
        request_hash,
    }))
}
fn current(conn: &Connection, intent: &BindingIntent) -> Result<(), PersistenceError> {
    if intent.operation.state != NativeOperationState::Running
        || iac_native::read(conn, intent.operation.operation_id)?.as_ref()
            != Some(&intent.operation)
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let stored =
        read(conn, intent.operation.operation_id)?.ok_or(PersistenceError::IacOperationIdentity)?;
    if stored.request_hash != intent.request_hash
        || hash(&stored.definition)? != hash(&intent.definition)?
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    if iac_owners::read(conn, &resource(intent.id()))? != intent.operation.ownership {
        return Err(PersistenceError::StaleIacOwner);
    }
    super::iac_dependencies::validate(conn, intent.operation.operation_id)?;
    let database: Option<String> = conn
        .query_row(
            "SELECT spec FROM pipe_databases WHERE id=?1",
            [intent.definition.database_id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    let database: Option<DatabaseInput> = database.as_deref().map(decode).transpose()?;
    if database.as_ref().map(hash).transpose()? != Some(hash(&intent.definition.database)?)
        || service_spec(conn, intent.input().service_id)?
            .as_ref()
            .map(hash)
            .transpose()?
            != Some(intent.definition.service_spec_hash.clone())
    {
        return Err(PersistenceError::StaleIacOwner);
    }
    let expected =
        prepared(conn, intent.operation.operation_id)?.or_else(|| intent.definition.before.clone());
    if binding(conn, intent.id())?.as_ref().map(hash).transpose()?
        != expected.as_ref().map(hash).transpose()?
    {
        return Err(PersistenceError::StaleIacOwner);
    }
    Ok(())
}
pub(super) fn write(conn: &Connection, record: &BindingRecord) -> Result<(), PersistenceError> {
    conn.execute("INSERT INTO database_bindings VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec",params![record.id.to_string(),record.input.service_id.to_string(),record.database_id.to_string(),encode(record)?])?;
    Ok(())
}
impl Store {
    pub(crate) fn database_binding_prepared(
        &self,
        intent: &BindingIntent,
    ) -> Result<Option<BindingRecord>, PersistenceError> {
        let conn = self.lock();
        current(&conn, intent)?;
        Ok(prepared(&conn, intent.operation.operation_id)?
            .or_else(|| intent.definition.before.clone()))
    }
    /// Credential generation is committed once before any account/grant side effect.
    pub(crate) fn prepare_database_binding(
        &self,
        intent: &BindingIntent,
        record: &BindingRecord,
    ) -> Result<BindingRecord, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        if let Some(saved) = prepared(&tx, intent.operation.operation_id)? {
            return Ok(saved);
        }
        if record.id != intent.id()
            || record.database_id != intent.definition.database_id
            || hash(&record.input)? != hash(intent.input())?
            || record.ready
            || intent.operation.action != NativeAction::CreateDatabaseBinding
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        write(&tx, record)?;
        tx.execute("UPDATE iac_database_binding_mutations SET prepared=?2,effects_started=1 WHERE operation_id=?1",params![intent.operation.operation_id.to_string(),encode(record)?])?;
        tx.commit()?;
        Ok(record.clone())
    }
    pub(crate) fn begin_database_binding_effects(
        &self,
        intent: &BindingIntent,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        tx.execute(
            "UPDATE iac_database_binding_mutations SET effects_started=1 WHERE operation_id=?1",
            [intent.operation.operation_id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn finish_database_binding(
        &self,
        intent: &BindingIntent,
    ) -> Result<NativeOperation, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        if intent.operation.action == NativeAction::CreateDatabaseBinding {
            let mut record = prepared(&tx, intent.operation.operation_id)?
                .or_else(|| intent.definition.before.clone())
                .ok_or(PersistenceError::IacOperationIdentity)?;
            record.ready = true;
            write(&tx, &record)?;
        } else {
            tx.execute(
                "DELETE FROM database_bindings WHERE id=?1",
                [intent.id().to_string()],
            )?;
        }
        let mut operation = intent.operation.clone();
        operation.state = NativeOperationState::Succeeded;
        iac_native::save(&tx, &operation)?;
        tx.execute(
            "DELETE FROM iac_database_targets WHERE operation_id=?1",
            [operation.operation_id.to_string()],
        )?;
        tx.commit()?;
        Ok(operation)
    }
    pub(crate) fn fail_database_binding(
        &self,
        intent: &BindingIntent,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if intent.operation.state != NativeOperationState::Running
            || iac_native::read(&tx, intent.operation.operation_id)?.as_ref()
                != Some(&intent.operation)
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let effects: bool = tx.query_row(
            "SELECT effects_started FROM iac_database_binding_mutations WHERE operation_id=?1",
            [intent.operation.operation_id.to_string()],
            |r| r.get(0),
        )?;
        let mut operation = intent.operation.clone();
        operation.state = if effects {
            NativeOperationState::NeedsRecovery
        } else {
            NativeOperationState::Failed
        };
        operation.error_code = Some(
            if effects {
                "database_binding_incomplete"
            } else {
                "database_binding_unverified"
            }
            .into(),
        );
        iac_native::save(&tx, &operation)?;
        if !effects {
            tx.execute(
                "DELETE FROM iac_database_targets WHERE operation_id=?1",
                [operation.operation_id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn recover_database_binding(
        &self,
        id: Uuid,
    ) -> Result<(BindingIntent, bool), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut intent = read(&tx, id)?.ok_or(PersistenceError::IacOperationIdentity)?;
        if matches!(
            intent.operation.state,
            NativeOperationState::Succeeded | NativeOperationState::Failed
        ) {
            return Ok((intent, false));
        }
        if intent.operation.state != NativeOperationState::NeedsRecovery {
            return Err(PersistenceError::IacResourceBusy);
        }
        intent.operation.state = NativeOperationState::Running;
        intent.operation.error_code = None;
        intent.operation.execution_generation = intent
            .operation
            .execution_generation
            .checked_add(1)
            .filter(|v| *v <= i64::MAX as u64)
            .ok_or(PersistenceError::IacOperationIdentity)?;
        iac_native::save(&tx, &intent.operation)?;
        current(&tx, &intent)?;
        tx.commit()?;
        Ok((intent, true))
    }
    /// Public summaries never need a live connection or current mutable configuration.
    pub(crate) fn database_binding_receipt(
        &self,
        intent: &BindingIntent,
    ) -> Result<Option<crate::pipe_databases::bindings::BindingSummary>, PersistenceError> {
        let conn = self.lock();
        let record = prepared(&conn, intent.operation.operation_id)?
            .or_else(|| intent.definition.before.clone());
        let operation = iac_native::read(&conn, intent.operation.operation_id)?
            .ok_or(PersistenceError::IacOperationIdentity)?;
        Ok(record.map(|mut r| {
            r.ready = operation.state == NativeOperationState::Succeeded;
            r.summary()
        }))
    }
}

//! Immutable event setup/control intents and their private, once-prepared accounts.
use super::{
    PersistenceError, Store,
    database_services::{decode, encode},
    iac_native, iac_owners,
};
use crate::pipe_databases::{
    DatabaseInput, DatabaseRecord,
    bindings::BindingRecord,
    events::{RabbitMQInput, Trigger, TriggerInput},
};
use firecrab_stack::{ManagedResource, NativeAction, NativeOperation, NativeOperationState};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

mod admission;
pub(super) use admission::validate_owner;
mod progress;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS iac_database_event_mutations (
 operation_id TEXT PRIMARY KEY REFERENCES iac_native_operations(operation_id),
 request_hash TEXT NOT NULL, definition TEXT NOT NULL, prepared_trigger TEXT, prepared_binding TEXT,
 effects_started INTEGER NOT NULL DEFAULT 0 CHECK(effects_started IN (0,1)), outcome TEXT
) STRICT;";
#[derive(Serialize)]
pub(crate) enum EventChange {
    Create { database: Uuid, input: TriggerInput },
    Pause { id: Uuid, paused: bool },
    Detach { id: Uuid },
    Delete { id: Uuid },
}
#[derive(Clone, Serialize, Deserialize)]
struct Setup {
    broker: RabbitMQInput,
    service_hash: String,
    binding_id: Uuid,
    binding_before: Option<BindingRecord>,
    change_binding: bool,
}
#[derive(Clone, Serialize, Deserialize)]
enum Work {
    Create(Box<Setup>),
    Pause(bool),
    Detach,
    Delete(Box<RabbitMQInput>),
}
impl Work {
    fn action(&self) -> NativeAction {
        match self {
            Self::Create(_) => NativeAction::CreateDatabaseEvents,
            Self::Pause(_) => NativeAction::PauseDatabaseEvents,
            Self::Detach => NativeAction::DetachDatabaseEvents,
            Self::Delete(_) => NativeAction::DeleteDatabaseEvents,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Definition {
    database: DatabaseInput,
    trigger: Trigger,
    before: Option<Trigger>,
    work: Work,
}
/// Only this module can construct or change the execution witness.
#[derive(Clone)]
pub(crate) struct EventIntent {
    pub operation: NativeOperation,
    request_hash: String,
    definition: Definition,
}
impl EventIntent {
    pub(crate) fn trigger(&self) -> &Trigger {
        &self.definition.trigger
    }
    pub(crate) fn database(&self) -> DatabaseRecord {
        DatabaseRecord {
            id: self.trigger().database_id,
            input: self.definition.database.clone(),
        }
    }
    pub(crate) fn broker(&self) -> Option<&RabbitMQInput> {
        match &self.definition.work {
            Work::Create(setup) => Some(&setup.broker),
            Work::Delete(broker) => Some(broker),
            _ => None,
        }
    }
    pub(crate) fn changes_binding(&self) -> bool {
        matches!(&self.definition.work,Work::Create(setup) if setup.change_binding)
    }
    pub(crate) fn binding_id(&self) -> Option<Uuid> {
        match &self.definition.work {
            Work::Create(setup) => Some(setup.binding_id),
            _ => None,
        }
    }
    pub(crate) fn creation_noop(&self) -> bool {
        matches!(self.definition.work, Work::Create(_)) && self.trigger().installed
    }
    pub(crate) fn paused(&self) -> Option<bool> {
        match self.definition.work {
            Work::Pause(paused) => Some(paused),
            _ => None,
        }
    }
}
pub(crate) enum Admission {
    Ready(Box<EventIntent>, bool),
    Missing,
    Invalid(&'static str),
}
pub(super) fn resource(database: Uuid, id: Uuid) -> ManagedResource {
    ManagedResource {
        kind: "database_events".into(),
        id: format!("{database}:{id}"),
    }
}
fn hash<T: Serialize>(value: &T) -> Result<String, PersistenceError> {
    Ok(format!("{:x}", Sha256::digest(encode(value)?)))
}
fn trigger(conn: &Connection, id: Uuid) -> Result<Option<Trigger>, PersistenceError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT spec FROM database_triggers WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    raw.as_deref().map(decode).transpose()
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
fn staged_trigger(
    conn: &Connection,
    intent: &EventIntent,
) -> Result<Option<Trigger>, PersistenceError> {
    let raw: Option<String> = conn.query_row(
        "SELECT prepared_trigger FROM iac_database_event_mutations WHERE operation_id=?1",
        [intent.operation.operation_id.to_string()],
        |r| r.get(0),
    )?;
    Ok(raw
        .as_deref()
        .map(decode)
        .transpose()?
        .or_else(|| intent.definition.before.clone()))
}
fn staged_binding(
    conn: &Connection,
    intent: &EventIntent,
) -> Result<Option<BindingRecord>, PersistenceError> {
    let raw: Option<String> = conn.query_row(
        "SELECT prepared_binding FROM iac_database_event_mutations WHERE operation_id=?1",
        [intent.operation.operation_id.to_string()],
        |r| r.get(0),
    )?;
    Ok(raw
        .as_deref()
        .map(decode)
        .transpose()?
        .or_else(|| match &intent.definition.work {
            Work::Create(setup) => setup.binding_before.clone(),
            _ => None,
        }))
}
fn read(conn: &Connection, id: Uuid) -> Result<Option<EventIntent>, PersistenceError> {
    let Some(operation) = iac_native::read(conn, id)? else {
        return Ok(None);
    };
    if operation.operation_id != id
        || !matches!(
            operation.action,
            NativeAction::CreateDatabaseEvents
                | NativeAction::PauseDatabaseEvents
                | NativeAction::DetachDatabaseEvents
                | NativeAction::DeleteDatabaseEvents
        )
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let (request_hash, raw): (String, String) = conn.query_row(
        "SELECT request_hash,definition FROM iac_database_event_mutations WHERE operation_id=?1",
        [id.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let definition: Definition = decode(&raw)?;
    if definition.trigger.id.is_nil()
        || definition.trigger.database_id.is_nil()
        || definition.trigger.input.request_id != definition.trigger.id
        || definition.work.action() != operation.action
        || operation.ownership.resource
            != resource(definition.trigger.database_id, definition.trigger.id)
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    Ok(Some(EventIntent {
        operation,
        request_hash,
        definition,
    }))
}
fn current(conn: &Connection, intent: &EventIntent) -> Result<(), PersistenceError> {
    if intent.operation.state != NativeOperationState::Running
        || iac_native::read(conn, intent.operation.operation_id)?.as_ref()
            != Some(&intent.operation)
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let original =
        read(conn, intent.operation.operation_id)?.ok_or(PersistenceError::IacOperationIdentity)?;
    if original.request_hash != intent.request_hash
        || hash(&original.definition)? != hash(&intent.definition)?
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    if iac_owners::read(conn, &intent.operation.ownership.resource)? != intent.operation.ownership {
        return Err(PersistenceError::StaleIacOwner);
    }
    super::iac_dependencies::validate(conn, intent.operation.operation_id)?;
    let raw: Option<String> = conn
        .query_row(
            "SELECT spec FROM pipe_databases WHERE id=?1",
            [intent.trigger().database_id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    let actual: Option<DatabaseInput> = raw.as_deref().map(decode).transpose()?;
    if actual.as_ref().map(hash).transpose()? != Some(hash(&intent.definition.database)?)
        || trigger(conn, intent.trigger().id)?
            .as_ref()
            .map(hash)
            .transpose()?
            != staged_trigger(conn, intent)?
                .as_ref()
                .map(hash)
                .transpose()?
    {
        return Err(PersistenceError::StaleIacOwner);
    }
    if let Work::Create(setup) = &intent.definition.work {
        let spec: Option<String> = conn
            .query_row(
                "SELECT spec FROM micro_services WHERE id=?1",
                [intent.trigger().input.service_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if spec.as_ref().map(hash).transpose()? != Some(setup.service_hash.clone())
            || binding(conn, setup.binding_id)?
                .as_ref()
                .map(hash)
                .transpose()?
                != staged_binding(conn, intent)?
                    .as_ref()
                    .map(hash)
                    .transpose()?
        {
            return Err(PersistenceError::StaleIacOwner);
        }
    }
    if let Some(broker) = intent.broker() {
        let raw: Option<String> = conn
            .query_row("SELECT spec FROM database_rabbitmq WHERE id=1", [], |r| {
                r.get(0)
            })
            .optional()?;
        let actual: Option<RabbitMQInput> = raw.as_deref().map(decode).transpose()?;
        if actual.as_ref().map(hash).transpose()? != Some(hash(broker)?) {
            return Err(PersistenceError::StaleIacOwner);
        }
    }
    Ok(())
}
fn write_trigger(conn: &Connection, value: &Trigger) -> Result<(), PersistenceError> {
    conn.execute("INSERT INTO database_triggers VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec",params![value.id.to_string(),encode(value)?])?;
    Ok(())
}

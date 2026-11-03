//! Private, immutable database connection intents retained before external SQL.

use super::{PersistenceError, iac_native, iac_owners};
use crate::pipe_databases::{DatabaseInput, DatabaseRecord};
use firecrab_stack::{ManagedResource, NativeAction, NativeOperation, NativeOperationState};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

mod connection;

pub(super) const SCHEMA:&str="CREATE TABLE IF NOT EXISTS iac_database_mutations (
 operation_id TEXT PRIMARY KEY REFERENCES iac_native_operations(operation_id), request_hash TEXT NOT NULL,
 payload TEXT NOT NULL, effects_started INTEGER NOT NULL DEFAULT 0 CHECK(effects_started IN (0,1))
) STRICT;";

#[derive(Clone, Serialize, Deserialize)]
struct Payload {
    verify_connection: bool,
    input: DatabaseInput,
    before_hash: Option<String>,
    dependencies_hash: String,
}

/// Only the server can construct this witness. Public operation JSON grants no write.
#[derive(Clone)]
pub(crate) struct DatabaseIntent {
    pub operation: NativeOperation,
    resource_id: Uuid,
    request_hash: String,
    payload: Payload,
}
impl DatabaseIntent {
    pub(crate) fn id(&self) -> Uuid {
        self.resource_id
    }
    pub(crate) fn verify_connection(&self) -> bool {
        self.payload.verify_connection
    }
    pub(crate) fn input(&self) -> &DatabaseInput {
        &self.payload.input
    }
}

#[derive(Serialize)]
pub(crate) enum DatabaseChange {
    Create(DatabaseInput),
    Update(Uuid, DatabaseInput),
    Access(Uuid, bool),
}

pub(crate) enum Admission {
    Ready(Box<DatabaseIntent>, bool),
    Invalid(&'static str),
    Missing,
}

fn hash<T: Serialize>(value: &T) -> Result<String, PersistenceError> {
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(value).map_err(|_| PersistenceError::IacOperationIdentity)?
        )
    ))
}
fn resource(id: Uuid) -> ManagedResource {
    ManagedResource {
        kind: "database_connection".into(),
        id: id.to_string(),
    }
}
fn record(conn: &Connection, id: Uuid) -> Result<Option<DatabaseRecord>, PersistenceError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT spec FROM pipe_databases WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|raw| {
        Ok(DatabaseRecord {
            id,
            input: serde_json::from_str(&raw)
                .map_err(|_| PersistenceError::IacOperationIdentity)?,
        })
    })
    .transpose()
}
fn dependencies(conn: &Connection, id: Uuid) -> Result<String, PersistenceError> {
    let mut values = Vec::new();
    for query in [
        "SELECT spec FROM database_bindings WHERE database_id=?1 ORDER BY id",
        "SELECT spec FROM database_triggers WHERE json_extract(spec,'$.databaseId')=?1 ORDER BY id",
    ] {
        let mut statement = conn.prepare(query)?;
        let rows = statement.query_map([id.to_string()], |r| r.get::<_, String>(0))?;
        values.push(rows.collect::<Result<Vec<_>, _>>()?);
    }
    hash(&values)
}
fn validate_update(
    conn: &Connection,
    old: &DatabaseRecord,
    input: &DatabaseInput,
) -> Result<Result<(), &'static str>, PersistenceError> {
    let next = match input.settings(old.id) {
        Ok(v) => v,
        Err(message) => return Ok(Err(message)),
    };
    let previous = old
        .input
        .settings(old.id)
        .map_err(|_| PersistenceError::IacOperationIdentity)?;
    let bindings: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM database_bindings WHERE database_id=?1)",
        [old.id.to_string()],
        |r| r.get(0),
    )?;
    let triggers: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM database_triggers WHERE json_extract(spec,'$.databaseId')=?1)",
        [old.id.to_string()],
        |r| r.get(0),
    )?;
    if (bindings || triggers)
        && (next.summary.host != previous.summary.host
            || next.summary.port != previous.summary.port
            || next.summary.database != previous.summary.database
            || next.summary.engine != previous.summary.engine
            || input.write_enabled != old.input.write_enabled)
    {
        return Ok(Err(
            "Remove service bindings and triggers before changing the database target or write access",
        ));
    }
    if bindings
        && (input.ca_certificate != old.input.ca_certificate
            || next.summary.tls_mode != previous.summary.tls_mode)
    {
        return Ok(Err(
            "Remove bindings before changing their TLS trust; deploy new bindings with the updated certificate",
        ));
    }
    Ok(Ok(()))
}
fn read(conn: &Connection, id: Uuid) -> Result<Option<DatabaseIntent>, PersistenceError> {
    let Some(operation) = iac_native::read(conn, id)? else {
        return Ok(None);
    };
    let resource_id = Uuid::parse_str(&operation.ownership.resource.id)
        .ok()
        .filter(|id| !id.is_nil())
        .ok_or(PersistenceError::IacOperationIdentity)?;
    if operation.operation_id != id
        || operation.ownership.resource != resource(resource_id)
        || !matches!(
            operation.action,
            NativeAction::CreateDatabaseConnection | NativeAction::UpdateDatabaseConnection
        )
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let (request_hash, raw): (String, String) = conn.query_row(
        "SELECT request_hash,payload FROM iac_database_mutations WHERE operation_id=?1",
        [id.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let payload = serde_json::from_str(&raw).map_err(|_| PersistenceError::IacOperationIdentity)?;
    Ok(Some(DatabaseIntent {
        operation,
        resource_id,
        request_hash,
        payload,
    }))
}
fn current(conn: &Connection, intent: &DatabaseIntent) -> Result<(), PersistenceError> {
    if intent.operation.state != NativeOperationState::Running
        || iac_native::read(conn, intent.operation.operation_id)?.as_ref()
            != Some(&intent.operation)
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let stored =
        read(conn, intent.operation.operation_id)?.ok_or(PersistenceError::IacOperationIdentity)?;
    if stored.resource_id != intent.resource_id
        || stored.request_hash != intent.request_hash
        || hash(&stored.payload)? != hash(&intent.payload)?
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    if iac_owners::read(conn, &resource(intent.id()))? != intent.operation.ownership {
        return Err(PersistenceError::StaleIacOwner);
    }
    let actual = record(conn, intent.id())?
        .map(|r| hash(&r.input))
        .transpose()?;
    if actual != intent.payload.before_hash
        || dependencies(conn, intent.id())? != intent.payload.dependencies_hash
    {
        return Err(PersistenceError::StaleIacOwner);
    }
    Ok(())
}

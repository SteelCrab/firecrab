//! Retain original credential-source generations without exposing credential fingerprints.

use super::PersistenceError;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use uuid::Uuid;

pub(super) fn initialize(conn: &Connection) -> Result<(), PersistenceError> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS iac_ai_connection_versions (
        connection_id TEXT PRIMARY KEY,generation INTEGER NOT NULL CHECK(generation>=0)
    ) STRICT;
    INSERT INTO iac_ai_connection_versions(connection_id,generation)
        SELECT id,0 FROM pipe_ai_connections WHERE true ON CONFLICT(connection_id) DO NOTHING;",
    )?;
    tx.commit()?;
    Ok(())
}

pub(super) fn generation(conn: &Connection, id: Uuid) -> Result<u64, PersistenceError> {
    let value: Option<i64> = conn
        .query_row(
            "SELECT generation FROM iac_ai_connection_versions WHERE connection_id=?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    value
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(PersistenceError::StaleIacExecution)
}

/// A tombstone survives disconnect so recreating a client UUID cannot revive an older plan.
pub(super) fn advance(conn: &Connection, id: Uuid) -> Result<(), PersistenceError> {
    let current: Option<i64> = conn
        .query_row(
            "SELECT generation FROM iac_ai_connection_versions WHERE connection_id=?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    let next = current
        .unwrap_or(0)
        .checked_add(1)
        .filter(|value| *value > 0)
        .ok_or(PersistenceError::StaleIacExecution)?;
    conn.execute(
        "INSERT INTO iac_ai_connection_versions(connection_id,generation) VALUES(?1,?2)
        ON CONFLICT(connection_id) DO UPDATE SET generation=excluded.generation",
        rusqlite::params![id.to_string(), next],
    )?;
    Ok(())
}

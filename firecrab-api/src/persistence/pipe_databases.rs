//! Credentials live in the existing owner-only SQLite store, never in responses.

use super::{PersistenceError, Store};
use crate::pipe_databases::{DatabaseInput, DatabaseRecord};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

pub(super) const CREATE_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS pipe_databases (
    id TEXT PRIMARY KEY,
    spec TEXT NOT NULL
) STRICT";

impl Store {
    pub fn list_pipe_databases(&self) -> Result<Vec<DatabaseRecord>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare("SELECT id, spec FROM pipe_databases ORDER BY rowid")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.map(|row| {
            let (id, spec) = row?;
            decode(id, spec)
        })
        .collect()
    }

    pub fn pipe_database(&self, id: Uuid) -> Result<Option<DatabaseRecord>, PersistenceError> {
        let row: Option<(String, String)> = self
            .lock()
            .query_row(
                "SELECT id, spec FROM pipe_databases WHERE id = ?1",
                [id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(id, spec)| decode(id, spec)).transpose()
    }

    pub fn save_pipe_database(
        &self,
        id: Uuid,
        input: &DatabaseInput,
    ) -> Result<(), PersistenceError> {
        let spec = serde_json::to_string(input).map_err(|_| PersistenceError::CorruptRecord {
            id: id.to_string(),
            reason: "invalid database configuration".into(),
        })?;
        self.lock().execute(
            "INSERT INTO pipe_databases (id, spec) VALUES (?1, ?2)",
            params![id.to_string(), spec],
        )?;
        Ok(())
    }

    pub fn delete_pipe_database(&self, id: Uuid) -> Result<bool, PersistenceError> {
        Ok(self
            .lock()
            .execute("DELETE FROM pipe_databases WHERE id = ?1", [id.to_string()])?
            != 0)
    }
}

fn decode(id: String, spec: String) -> Result<DatabaseRecord, PersistenceError> {
    let corrupt = || PersistenceError::CorruptRecord {
        id: id.clone(),
        reason: "invalid database configuration".into(),
    };
    Ok(DatabaseRecord {
        id: Uuid::parse_str(&id).map_err(|_| corrupt())?,
        input: serde_json::from_str(&spec).map_err(|_| corrupt())?,
    })
}

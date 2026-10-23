//! Private S3 connection records in the existing owner-only SQLite store.
use super::{PersistenceError, Store};
use crate::pipe_storage::{StorageRecord, corrupt};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

pub(super) const CREATE_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS pipe_storage (
    id TEXT PRIMARY KEY,
    record TEXT NOT NULL
) STRICT";

impl Store {
    pub fn list_pipe_storage(&self) -> Result<Vec<StorageRecord>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare("SELECT id, record FROM pipe_storage ORDER BY rowid")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (id, text) = row?;
            decode(&id, &text)
        })
        .collect()
    }
    pub fn pipe_storage(&self, id: Uuid) -> Result<Option<StorageRecord>, PersistenceError> {
        let text: Option<String> = self
            .lock()
            .query_row(
                "SELECT record FROM pipe_storage WHERE id = ?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        text.map(|text| decode(&id.to_string(), &text)).transpose()
    }
    pub fn save_pipe_storage(&self, record: &StorageRecord) -> Result<(), PersistenceError> {
        let text = serde_json::to_string(record).map_err(|_| corrupt(record.id))?;
        self.lock().execute("INSERT INTO pipe_storage (id, record) VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET record=excluded.record", params![record.id.to_string(),text])?;
        Ok(())
    }
    pub fn delete_pipe_storage(&self, id: Uuid) -> Result<bool, PersistenceError> {
        Ok(self
            .lock()
            .execute("DELETE FROM pipe_storage WHERE id = ?1", [id.to_string()])?
            != 0)
    }
}
fn decode(id: &str, text: &str) -> Result<StorageRecord, PersistenceError> {
    let id = Uuid::parse_str(id).map_err(|_| corrupt(Uuid::nil()))?;
    let record: StorageRecord = serde_json::from_str(text).map_err(|_| corrupt(id))?;
    if record.id != id {
        return Err(corrupt(id));
    }
    Ok(record)
}

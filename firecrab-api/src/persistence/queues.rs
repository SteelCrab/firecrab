//! Private engine credentials and application queue declarations.
use super::{PersistenceError, Store};
use crate::queues::{EngineRecord, QueueRecord};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS queue_engine (
    id INTEGER PRIMARY KEY CHECK (id = 1), record TEXT NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS application_queues (
    id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, record TEXT NOT NULL
) STRICT;";

fn decode<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, PersistenceError> {
    serde_json::from_str(text).map_err(|_| PersistenceError::CorruptRecord {
        id: "queue".into(),
        reason: "invalid Queue record".into(),
    })
}
fn decode_queue(id: &str, name: &str, text: &str) -> Result<QueueRecord, PersistenceError> {
    let record: QueueRecord = decode(text)?;
    if Uuid::parse_str(id).ok() != Some(record.id) || name != record.name {
        return Err(PersistenceError::CorruptRecord {
            id: "queue".into(),
            reason: "Queue identity does not match its stored key".into(),
        });
    }
    Ok(record)
}
fn encode<T: serde::Serialize>(record: &T) -> Result<String, PersistenceError> {
    serde_json::to_string(record).map_err(|_| PersistenceError::CorruptRecord {
        id: "queue".into(),
        reason: "could not serialize Queue record".into(),
    })
}
impl Store {
    pub(crate) fn queue_engine(&self) -> Result<Option<EngineRecord>, PersistenceError> {
        let text: Option<String> = self
            .lock()
            .query_row("SELECT record FROM queue_engine WHERE id=1", [], |r| {
                r.get(0)
            })
            .optional()?;
        text.map(|s| decode(&s)).transpose()
    }
    pub(crate) fn save_queue_engine(&self, record: &EngineRecord) -> Result<(), PersistenceError> {
        self.lock().execute("INSERT INTO queue_engine(id,record) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET record=excluded.record", [encode(record)?])?;
        Ok(())
    }
    pub(crate) fn list_queues(&self) -> Result<Vec<QueueRecord>, PersistenceError> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT id,name,record FROM application_queues ORDER BY name,id")?;
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .map(|r| {
            let (id, name, text) = r?;
            decode_queue(&id, &name, &text)
        })
        .collect()
    }
    pub(crate) fn queue(&self, id: Uuid) -> Result<Option<QueueRecord>, PersistenceError> {
        let row: Option<(String, String)> = self
            .lock()
            .query_row(
                "SELECT name,record FROM application_queues WHERE id=?1",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(name, text)| decode_queue(&id.to_string(), &name, &text))
            .transpose()
    }
    pub(crate) fn save_queue(&self, record: &QueueRecord) -> Result<(), PersistenceError> {
        self.lock().execute("INSERT INTO application_queues(id,name,record) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET name=excluded.name,record=excluded.record", params![record.id.to_string(),record.name,encode(record)?])?;
        Ok(())
    }
    pub(crate) fn delete_queue(&self, id: Uuid) -> Result<(), PersistenceError> {
        self.lock().execute(
            "DELETE FROM application_queues WHERE id=?1",
            [id.to_string()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use firecrab_api_types::QueuePhase;

    #[test]
    fn mismatched_queue_identity_fails_closed_without_exposing_credentials() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("state.db")).unwrap();
        let queue = QueueRecord {
            id: Uuid::new_v4(),
            name: "notifications".into(),
            delivery_limit: 5,
            max_bytes: 64 * 1024 * 1024,
            phase: QueuePhase::Ready,
            error: None,
            publish_password: "private-publisher-password".into(),
            consume_password: "private-consumer-password".into(),
        };
        store.save_queue(&queue).unwrap();
        for (field, value) in [("id", Uuid::new_v4().to_string()), ("name", "other".into())] {
            let mut damaged = serde_json::to_value(&queue).unwrap();
            damaged[field] = value.into();
            store
                .lock()
                .execute(
                    "UPDATE application_queues SET record=?1 WHERE id=?2",
                    params![damaged.to_string(), queue.id.to_string()],
                )
                .unwrap();
            for result in [
                store.queue(queue.id).map(|_| ()),
                store.list_queues().map(|_| ()),
            ] {
                let error = result.unwrap_err();
                assert!(matches!(error, PersistenceError::CorruptRecord { .. }));
                assert!(!error.to_string().contains("private"));
            }
        }
    }
}

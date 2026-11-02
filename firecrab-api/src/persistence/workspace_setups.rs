use super::{PersistenceError, Store};
use crate::workspace_setup::SavedSetup;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS workspace_setups (
 id TEXT PRIMARY KEY, vm_id TEXT NOT NULL UNIQUE, record TEXT NOT NULL
) STRICT;";

fn decode(id: &str, vm: &str, json: &str) -> Result<SavedSetup, PersistenceError> {
    let saved: SavedSetup = serde_json::from_str(json).map_err(|_| corrupt())?;
    if saved.progress.id.to_string() != id
        || saved.progress.vm_id.to_string() != vm
        || saved.request.id != saved.progress.id
    {
        return Err(corrupt());
    }
    Ok(saved)
}
fn corrupt() -> PersistenceError {
    PersistenceError::CorruptRecord {
        id: "workspace_setup".into(),
        reason: "invalid workspace setup identity".into(),
    }
}

impl Store {
    pub(crate) fn workspace_setup(&self, id: Uuid) -> Result<Option<SavedSetup>, PersistenceError> {
        let row: Option<(String, String)> = self
            .lock()
            .query_row(
                "SELECT vm_id,record FROM workspace_setups WHERE id=?1",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(vm, json)| decode(&id.to_string(), &vm, &json))
            .transpose()
    }
    pub(crate) fn workspace_setups(&self) -> Result<Vec<SavedSetup>, PersistenceError> {
        let conn = self.lock();
        let mut statement =
            conn.prepare("SELECT id,vm_id,record FROM workspace_setups ORDER BY rowid DESC")?;
        statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .map(|row| {
                let (id, vm, json) = row?;
                decode(&id, &vm, &json)
            })
            .collect()
    }
    pub(crate) fn save_workspace_setup(&self, saved: &SavedSetup) -> Result<(), PersistenceError> {
        let json = serde_json::to_string(saved).map_err(|_| corrupt())?;
        if saved.request.id != saved.progress.id {
            return Err(corrupt());
        }
        let updated=self.lock().execute("INSERT INTO workspace_setups(id,vm_id,record) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET record=excluded.record WHERE workspace_setups.vm_id=excluded.vm_id",params![saved.progress.id.to_string(),saved.progress.vm_id.to_string(),json])?;
        if updated != 1 {
            return Err(corrupt());
        }
        Ok(())
    }
}

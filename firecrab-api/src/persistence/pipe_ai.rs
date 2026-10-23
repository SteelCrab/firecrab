//! AI records share the owner-only SQLite database. No credentials in public views.
use super::{PersistenceError, Store};
use crate::pipe_ai::{OAuthRegistration, SavedConnection};
use firecrab_api_types::AiModelSelection;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

pub(super) const CONNECTIONS_SQL: &str = "CREATE TABLE IF NOT EXISTS pipe_ai_connections (
    id TEXT PRIMARY KEY, spec TEXT NOT NULL
) STRICT";
pub(super) const DEFAULTS_SQL: &str = "CREATE TABLE IF NOT EXISTS pipe_ai_defaults (
    project_id TEXT PRIMARY KEY, connection_id TEXT NOT NULL, model TEXT NOT NULL
) STRICT";
pub(super) const SETTINGS_SQL: &str = "CREATE TABLE IF NOT EXISTS pipe_ai_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT";
pub(super) const REGISTRATIONS_SQL: &str = "CREATE TABLE IF NOT EXISTS pipe_ai_registrations (id TEXT PRIMARY KEY, spec TEXT NOT NULL) STRICT";

fn decode(spec: String) -> Result<SavedConnection, PersistenceError> {
    serde_json::from_str(&spec).map_err(|_| PersistenceError::CorruptRecord {
        id: "pipe_ai".into(),
        reason: "invalid AI connection".into(),
    })
}

impl Store {
    pub fn ai_host_id(&self) -> Result<String, PersistenceError> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR IGNORE INTO pipe_ai_settings (key,value) VALUES ('host_id',?1)",
            [format!("urn:uuid:{}", Uuid::new_v4())],
        )?;
        Ok(conn.query_row(
            "SELECT value FROM pipe_ai_settings WHERE key='host_id'",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn ai_registration(&self, id: Uuid) -> Result<Option<OAuthRegistration>, PersistenceError> {
        let spec: Option<String> = self
            .lock()
            .query_row(
                "SELECT spec FROM pipe_ai_registrations WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        spec.map(|s| {
            serde_json::from_str(&s).map_err(|_| PersistenceError::CorruptRecord {
                id: "pipe_ai".into(),
                reason: "invalid AI registration".into(),
            })
        })
        .transpose()
    }
    pub fn ai_connections(&self) -> Result<Vec<SavedConnection>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare("SELECT spec FROM pipe_ai_connections ORDER BY id")?;
        statement
            .query_map([], |row| row.get(0))?
            .map(|row| decode(row?))
            .collect()
    }

    pub fn ai_connection(&self, id: Uuid) -> Result<Option<SavedConnection>, PersistenceError> {
        let spec = self
            .lock()
            .query_row(
                "SELECT spec FROM pipe_ai_connections WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        spec.map(decode).transpose()
    }

    /// Credential replacement invalidates old model proofs and stale project defaults.
    pub fn save_ai_connection(&self, saved: &SavedConnection) -> Result<(), PersistenceError> {
        let spec = serde_json::to_string(saved).map_err(|_| PersistenceError::CorruptRecord {
            id: "pipe_ai".into(),
            reason: "invalid AI connection".into(),
        })?;
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let id = saved.connection.id.to_string();
        if let Some(session) = &saved.oauth {
            let spec = serde_json::json!({"client_id":session.client_id,"subject":session.subject})
                .to_string();
            tx.execute("INSERT INTO pipe_ai_registrations (id,spec) VALUES (?1,?2) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec", params![id, spec])?;
        }
        tx.execute(
            "INSERT INTO pipe_ai_connections (id, spec) VALUES (?1, ?2)
            ON CONFLICT(id) DO UPDATE SET spec=excluded.spec",
            params![id, spec],
        )?;
        let mut statement =
            tx.prepare("SELECT project_id, model FROM pipe_ai_defaults WHERE connection_id=?1")?;
        let defaults = statement
            .query_map([&id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for (project, model) in defaults {
            if !saved.connection.models.contains(&model) {
                tx.execute(
                    "DELETE FROM pipe_ai_defaults WHERE project_id=?1",
                    [project],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_ai_connection(&self, id: Uuid) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM pipe_ai_defaults WHERE connection_id=?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM pipe_ai_connections WHERE id=?1",
            [id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn ai_default(&self, project: Uuid) -> Result<Option<AiModelSelection>, PersistenceError> {
        let row: Option<(String, String)> = self
            .lock()
            .query_row(
                "SELECT connection_id, model FROM pipe_ai_defaults WHERE project_id=?1",
                [project.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(id, model)| {
            Ok(AiModelSelection {
                connection_id: Uuid::parse_str(&id).map_err(|_| {
                    PersistenceError::CorruptRecord {
                        id: "pipe_ai".into(),
                        reason: "invalid AI selection".into(),
                    }
                })?,
                model,
            })
        })
        .transpose()
    }

    pub fn save_ai_default(
        &self,
        project: Uuid,
        selection: &AiModelSelection,
    ) -> Result<(), PersistenceError> {
        self.lock().execute("INSERT INTO pipe_ai_defaults (project_id, connection_id, model) VALUES (?1, ?2, ?3)
            ON CONFLICT(project_id) DO UPDATE SET connection_id=excluded.connection_id, model=excluded.model",
            params![project.to_string(), selection.connection_id.to_string(), selection.model])?;
        Ok(())
    }
}

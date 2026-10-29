//! Per-service, credential-free OTLP endpoint settings.

use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use super::{PersistenceError, Store};

/// The endpoint is a current operator setting, independent of release snapshots.
pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS service_otlp_config (
    service_id TEXT PRIMARY KEY,
    endpoint TEXT NOT NULL,
    enabled_at_ms INTEGER NOT NULL DEFAULT 0
) STRICT";

/// Add the opt-in boundary to databases created before the column existed.
pub(super) fn migrate(conn: &Connection) -> Result<(), PersistenceError> {
    let tx = conn.unchecked_transaction()?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('service_otlp_config') WHERE name='enabled_at_ms')",
        [], |row| row.get(0),
    )?;
    if !exists {
        // Existing experimental settings have no trustworthy consent timestamp.
        // Start their export window at migration rather than exposing older logs.
        tx.execute(
            "ALTER TABLE service_otlp_config ADD COLUMN enabled_at_ms INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
        tx.execute(
            "UPDATE service_otlp_config SET enabled_at_ms=?1",
            [chrono::Utc::now().timestamp_millis().saturating_add(1)],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Remove a setting inside the service deletion transaction.
pub(super) fn delete(conn: &Connection, service_id: Uuid) -> Result<(), PersistenceError> {
    conn.execute(
        "DELETE FROM service_otlp_config WHERE service_id=?1",
        [service_id.to_string()],
    )?;
    Ok(())
}

impl Store {
    /// Return the current endpoint, or `None` when export is disabled.
    pub(crate) fn otlp_endpoint(&self, service: Uuid) -> Result<Option<String>, PersistenceError> {
        Ok(self
            .otlp_configuration(service)?
            .map(|(endpoint, _)| endpoint))
    }

    /// Return the endpoint and its durable opt-in boundary.
    pub(crate) fn otlp_configuration(
        &self,
        service: Uuid,
    ) -> Result<Option<(String, i64)>, PersistenceError> {
        self.lock()
            .query_row(
                "SELECT endpoint, enabled_at_ms FROM service_otlp_config WHERE service_id=?1",
                [service.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Store a validated endpoint, or disable export by clearing the setting.
    /// The owner check and update use the same connection lock as service deletion.
    pub(crate) fn set_otlp_endpoint(
        &self,
        service: Uuid,
        endpoint: Option<&str>,
    ) -> Result<i64, PersistenceError> {
        let endpoint = endpoint
            .map(|value| {
                crate::otlp::validate_endpoint(value)
                    .map(|url| url.to_string())
                    .map_err(|_| PersistenceError::CorruptRecord {
                        id: service.to_string(),
                        reason: "invalid OTLP endpoint".into(),
                    })
            })
            .transpose()?;
        let conn = self.lock();
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM micro_services WHERE id=?1)",
            [service.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(rusqlite::Error::QueryReturnedNoRows.into());
        }
        let enabled_at_ms = chrono::Utc::now().timestamp_millis().saturating_add(1);
        if let Some(endpoint) = endpoint {
            conn.execute(
                "INSERT INTO service_otlp_config(service_id, endpoint, enabled_at_ms) VALUES (?1, ?2, ?3)
                 ON CONFLICT(service_id) DO UPDATE SET endpoint=excluded.endpoint, enabled_at_ms=excluded.enabled_at_ms",
                params![service.to_string(), endpoint, enabled_at_ms],
            )?;
        } else {
            delete(&conn, service)?;
        }
        Ok(enabled_at_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_excludes_preexisting_buffer_and_preserves_the_boundary_on_reopen() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE service_otlp_config (service_id TEXT PRIMARY KEY, endpoint TEXT NOT NULL) STRICT", []).unwrap();
        conn.execute(
            "INSERT INTO service_otlp_config VALUES ('service', 'http://collector.test/')",
            [],
        )
        .unwrap();
        let before = chrono::Utc::now().timestamp_millis();
        migrate(&conn).unwrap();
        let (endpoint, boundary): (String, i64) = conn
            .query_row(
                "SELECT endpoint, enabled_at_ms FROM service_otlp_config",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(endpoint, "http://collector.test/");
        assert!(boundary > before);
        migrate(&conn).unwrap();
        let after: i64 = conn
            .query_row("SELECT enabled_at_ms FROM service_otlp_config", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(after, boundary);
    }
}

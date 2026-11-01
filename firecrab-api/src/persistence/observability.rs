//! One operator-managed SigNoz connection, linked to existing service exporters.
use super::{PersistenceError, Store};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS observability_signoz (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    endpoint TEXT NOT NULL,
    paused INTEGER NOT NULL CHECK(paused IN (0,1))
) STRICT;
CREATE TABLE IF NOT EXISTS observability_signoz_sources (
    service_id TEXT PRIMARY KEY REFERENCES micro_services(id) ON DELETE CASCADE
) STRICT;";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SignozConfig {
    pub endpoint: String,
    pub service_ids: Vec<Uuid>,
    pub paused: bool,
}

fn read(conn: &Connection) -> Result<Option<SignozConfig>, PersistenceError> {
    let row = conn
        .query_row(
            "SELECT endpoint, paused FROM observability_signoz WHERE singleton=1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
        )
        .optional()?;
    let Some((endpoint, paused)) = row else {
        return Ok(None);
    };
    let mut statement =
        conn.prepare("SELECT service_id FROM observability_signoz_sources ORDER BY service_id")?;
    let service_ids = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .map(|row| {
            let id = row?;
            Uuid::parse_str(&id).map_err(|_| PersistenceError::CorruptRecord {
                id,
                reason: "invalid observability source".into(),
            })
        })
        .collect::<Result<Vec<_>, PersistenceError>>()?;
    Ok(Some(SignozConfig {
        endpoint,
        service_ids,
        paused,
    }))
}

impl Store {
    pub(crate) fn signoz_config(&self) -> Result<Option<SignozConfig>, PersistenceError> {
        read(&self.lock())
    }

    pub(crate) fn signoz_source(&self, service: Uuid) -> Result<bool, PersistenceError> {
        Ok(self.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM observability_signoz_sources WHERE service_id=?1)",
            [service.to_string()],
            |row| row.get(0),
        )?)
    }

    /// Caller holds service_mutations and preflights all sources before committing.
    /// Keep unchanged source boundaries, so a pause/resume never replays history.
    pub(crate) fn save_signoz(
        &self,
        config: Option<&SignozConfig>,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let previous = read(&tx)?;
        if let Some(previous) = &previous {
            for service in &previous.service_ids {
                if config.is_none_or(|next| !next.service_ids.contains(service)) {
                    super::otlp_config::delete(&tx, *service)?;
                }
            }
        }
        tx.execute("DELETE FROM observability_signoz_sources", [])?;
        if let Some(config) = config {
            tx.execute("INSERT INTO observability_signoz VALUES (1, ?1, ?2) ON CONFLICT(singleton) DO UPDATE SET endpoint=excluded.endpoint, paused=excluded.paused", params![config.endpoint, config.paused])?;
            for service in &config.service_ids {
                let same = previous.as_ref().is_some_and(|old| {
                    old.endpoint == config.endpoint && old.service_ids.contains(service)
                });
                if !same {
                    tx.execute("INSERT INTO service_otlp_config(service_id,endpoint,enabled_at_ms) VALUES (?1,?2,?3) ON CONFLICT(service_id) DO UPDATE SET endpoint=excluded.endpoint,enabled_at_ms=excluded.enabled_at_ms",
                        params![service.to_string(), config.endpoint, chrono::Utc::now().timestamp_millis().saturating_add(1)])?;
                }
                tx.execute(
                    "INSERT INTO observability_signoz_sources VALUES (?1)",
                    [service.to_string()],
                )?;
            }
        } else {
            tx.execute("DELETE FROM observability_signoz", [])?;
        }
        tx.commit()?;
        Ok(())
    }
}

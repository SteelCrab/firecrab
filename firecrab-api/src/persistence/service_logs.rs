//! Durable, bounded service log pages and per-runtime collection cursors.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use uuid::Uuid;

use super::{PersistenceError, Store};

/// Tables for bounded app output and collection progress.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS service_observation_logs (
 id INTEGER PRIMARY KEY AUTOINCREMENT, service_id TEXT NOT NULL, vm_id TEXT NOT NULL,
 runtime_id TEXT NOT NULL, sequence INTEGER NOT NULL, timestamp_ms INTEGER NOT NULL,
 stream TEXT NOT NULL, message TEXT NOT NULL, UNIQUE(service_id, vm_id, runtime_id, sequence)
) STRICT;
CREATE INDEX IF NOT EXISTS service_observation_logs_owner ON service_observation_logs(service_id, id);
CREATE TABLE IF NOT EXISTS service_observation_cursors (
 service_id TEXT NOT NULL, vm_id TEXT NOT NULL, runtime_id TEXT NOT NULL,
 sequence INTEGER NOT NULL, status TEXT NOT NULL, loss_at INTEGER, updated_at INTEGER NOT NULL,
 PRIMARY KEY(service_id, vm_id, runtime_id)
) STRICT;
";

/// Maximum retained app output chunks per service.
const MAX_LOGS: i64 = 2000;
/// Retention interval for app output and collection state.
const HISTORY_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// Maximum UTF-8 bytes retained for one app output chunk.
const MAX_MESSAGE_BYTES: usize = 4096;

/// A runtime's sequenced record, before it is committed to service history.
pub(crate) struct InputLog {
    /// Sequence assigned by the VM's log collector.
    pub sequence: u64,
    /// Host collection time in Unix milliseconds.
    pub timestamp: i64,
    /// App output stream, either stdout or stderr.
    pub stream: String,
    /// Captured app output chunk.
    pub message: String,
}

/// One persisted app output line. VM identity distinguishes replicas.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ServiceLog {
    /// Database sequence used to page records.
    pub id: i64,
    /// Collection time in RFC 3339 format.
    pub timestamp: String,
    /// App output stream, either stdout or stderr.
    pub stream: String,
    /// Captured app output chunk, limited in size.
    pub message: String,
    /// VM identifier for the replica that produced the output.
    pub instance_id: String,
}

/// Convert Unix milliseconds into the API timestamp format.
fn timestamp(millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(millis)
        .unwrap_or(chrono::DateTime::UNIX_EPOCH)
        .to_rfc3339()
}

/// Truncate a message at a UTF-8 boundary and report any truncation.
fn bounded(message: &str) -> (String, bool) {
    if message.len() <= MAX_MESSAGE_BYTES {
        return (message.to_owned(), false);
    }
    let mut end = MAX_MESSAGE_BYTES;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}…", &message[..end]), true)
}

impl Store {
    /// Remove expired history even when a service no longer emits records.
    pub(crate) fn prune_service_observations(&self) -> Result<(), PersistenceError> {
        self.prune_service_observations_at(chrono::Utc::now().timestamp_millis())
    }

    /// Prune history and cursors older than the retention interval.
    fn prune_service_observations_at(&self, now: i64) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let cutoff = now.saturating_sub(HISTORY_MS);
        tx.execute(
            "DELETE FROM service_observation_logs WHERE timestamp_ms < ?1",
            [cutoff],
        )?;
        tx.execute(
            "DELETE FROM service_observation_cursors WHERE updated_at < ?1",
            [cutoff],
        )?;
        tx.execute(
            "DELETE FROM service_observation_events WHERE timestamp < ?1",
            [now.div_euclid(1000).saturating_sub(HISTORY_MS / 1000)],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// A cursor is committed with its records and is scoped to this runtime.
    pub(crate) fn service_log_cursor(
        &self,
        service: Uuid,
        vm: Uuid,
        runtime: Uuid,
    ) -> Result<u64, PersistenceError> {
        let value: Option<i64> = self.lock().query_row(
            "SELECT sequence FROM service_observation_cursors WHERE service_id=?1 AND vm_id=?2 AND runtime_id=?3",
            params![service.to_string(), vm.to_string(), runtime.to_string()], |row| row.get(0),
        ).optional()?;
        Ok(value.unwrap_or(0).max(0) as u64)
    }

    /// Commit a page once, even if the collector and VM cleanup race.
    pub(crate) fn append_service_logs(
        &self,
        service: Uuid,
        identity: (Uuid, Uuid),
        next: u64,
        status: &str,
        lost: bool,
        records: &[InputLog],
    ) -> Result<(), PersistenceError> {
        let (vm, runtime) = identity;
        if !matches!(
            status,
            "active" | "waiting" | "inactive" | "unavailable" | "error"
        ) {
            return Err(PersistenceError::CorruptRecord {
                id: vm.to_string(),
                reason: "invalid app log status".into(),
            });
        }
        let next = i64::try_from(next).map_err(|_| PersistenceError::CorruptRecord {
            id: vm.to_string(),
            reason: "invalid app log cursor".into(),
        })?;
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let service_id = service.to_string();
        let vm_id = vm.to_string();
        let runtime_id = runtime.to_string();
        let now = chrono::Utc::now().timestamp_millis();
        let owner_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM micro_services WHERE id=?1)",
            [&service_id],
            |row| row.get(0),
        )?;
        if !owner_exists {
            return Ok(());
        }
        let mut gap = lost;
        let mut added = 0;
        for record in records {
            let sequence =
                i64::try_from(record.sequence).map_err(|_| PersistenceError::CorruptRecord {
                    id: vm_id.clone(),
                    reason: "invalid app log sequence".into(),
                })?;
            if !matches!(record.stream.as_str(), "stdout" | "stderr") || sequence > next {
                return Err(PersistenceError::CorruptRecord {
                    id: vm_id.clone(),
                    reason: "invalid app log record".into(),
                });
            }
            let (message, clipped) = bounded(&record.message);
            gap |= clipped;
            added += tx.execute("INSERT OR IGNORE INTO service_observation_logs(service_id,vm_id,runtime_id,sequence,timestamp_ms,stream,message) VALUES (?1,?2,?3,?4,?5,?6,?7)", params![service_id, vm_id, runtime_id, sequence, record.timestamp, record.stream, message])?;
        }
        let previous_loss: Option<i64> = tx.query_row("SELECT loss_at FROM service_observation_cursors WHERE service_id=?1 AND vm_id=?2 AND runtime_id=?3", params![service_id, vm_id, runtime_id], |row| row.get(0)).optional()?.flatten();
        tx.execute("INSERT INTO service_observation_cursors(service_id,vm_id,runtime_id,sequence,status,loss_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(service_id,vm_id,runtime_id) DO UPDATE SET sequence=MAX(sequence,excluded.sequence),status=excluded.status,loss_at=COALESCE(excluded.loss_at,loss_at),updated_at=excluded.updated_at", params![service_id, vm_id, runtime_id, next, status, gap.then_some(now), now])?;
        if gap && previous_loss.is_none_or(|last| last < now.saturating_sub(HISTORY_MS)) {
            super::service_observation::event(
                &tx,
                service,
                now / 1000,
                "log_gap",
                "warning",
                "Some application log output exceeded the local buffer",
            )?;
        }
        if added > 0 {
            tx.execute("DELETE FROM service_observation_logs WHERE service_id=?1 AND (timestamp_ms<?2 OR id NOT IN (SELECT id FROM service_observation_logs WHERE service_id=?1 ORDER BY id DESC LIMIT ?3))", params![service_id, now.saturating_sub(HISTORY_MS), MAX_LOGS])?;
        }
        // Replica replacements must not grow cursor storage without a bound.
        // The limit exceeds the maximum of 100 active replicas per service.
        tx.execute("DELETE FROM service_observation_cursors WHERE service_id=?1 AND (updated_at<?2 OR rowid NOT IN (SELECT rowid FROM service_observation_cursors WHERE service_id=?1 ORDER BY updated_at DESC, rowid DESC LIMIT 512))", params![service_id, now.saturating_sub(HISTORY_MS)])?;
        tx.commit()?;
        Ok(())
    }

    /// Return recent app output without exposing another service's logs.
    pub(crate) fn service_logs(&self, service: Uuid) -> Result<Vec<ServiceLog>, PersistenceError> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT id,timestamp_ms,stream,message,vm_id FROM service_observation_logs WHERE service_id=?1 AND timestamp_ms>=?2 ORDER BY id DESC LIMIT 200")?;
        let rows = stmt.query_map(
            params![
                service.to_string(),
                chrono::Utc::now()
                    .timestamp_millis()
                    .saturating_sub(HISTORY_MS)
            ],
            |row| {
                Ok(ServiceLog {
                    id: row.get(0)?,
                    timestamp: timestamp(row.get(1)?),
                    stream: row.get(2)?,
                    message: row.get(3)?,
                    instance_id: row.get(4)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Bounded ascending pages let the exporter resume after its in-memory cursor.
    pub(crate) fn service_logs_after(
        &self,
        service: Uuid,
        after: i64,
    ) -> Result<Vec<ServiceLog>, PersistenceError> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT id,timestamp_ms,stream,message,vm_id FROM service_observation_logs WHERE service_id=?1 AND id>?2 AND timestamp_ms>=?3 ORDER BY id LIMIT 128")?;
        let rows = stmt.query_map(
            params![
                service.to_string(),
                after,
                chrono::Utc::now()
                    .timestamp_millis()
                    .saturating_sub(HISTORY_MS)
            ],
            |row| {
                Ok(ServiceLog {
                    id: row.get(0)?,
                    timestamp: timestamp(row.get(1)?),
                    stream: row.get(2)?,
                    message: row.get(3)?,
                    instance_id: row.get(4)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Current collection state and any gap inside the retained history window.
    pub(crate) fn service_log_state(
        &self,
        service: Uuid,
        vm: Uuid,
        runtime: Uuid,
    ) -> Result<(String, bool), PersistenceError> {
        let conn = self.lock();
        let value: Option<(String, Option<i64>)> = conn.query_row("SELECT status,loss_at FROM service_observation_cursors WHERE service_id=?1 AND vm_id=?2 AND runtime_id=?3", params![service.to_string(),vm.to_string(),runtime.to_string()], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
        Ok(value.map_or(("waiting".into(), false), |(status, loss)| {
            (
                status,
                loss.is_some_and(|t| {
                    t >= chrono::Utc::now()
                        .timestamp_millis()
                        .saturating_sub(HISTORY_MS)
                }),
            )
        }))
    }

    /// Preserve a recent loss indication after a replica or runtime is replaced.
    pub(crate) fn service_has_log_gap(&self, service: Uuid) -> Result<bool, PersistenceError> {
        let conn = self.lock();
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM service_observation_cursors WHERE service_id=?1 AND loss_at>=?2)",
            params![service.to_string(), chrono::Utc::now().timestamp_millis().saturating_sub(HISTORY_MS)],
            |row| row.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_pages_are_idempotent_service_scoped_and_report_a_gap() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("state.db")).unwrap();
        let service = Uuid::new_v4();
        let spec = serde_json::from_value(serde_json::json!({"name":"log-test", "image":"ghcr.io/team/app:v1", "port":8080,"ram":128,"cpu":1})).unwrap();
        store
            .insert_micro_service(&firecrab_api_types::MicroServiceResponse { id: service, spec })
            .unwrap();
        let vm = Uuid::new_v4();
        let runtime = Uuid::new_v4();
        let logs = vec![InputLog {
            sequence: 1,
            timestamp: chrono::Utc::now().timestamp_millis(),
            stream: "stdout".into(),
            message: "line".into(),
        }];
        store
            .append_service_logs(service, (vm, runtime), 1, "active", false, &logs)
            .unwrap();
        store
            .append_service_logs(service, (vm, runtime), 1, "active", false, &logs)
            .unwrap();
        assert_eq!(store.service_logs(service).unwrap().len(), 1);
        assert!(store.service_logs(Uuid::new_v4()).unwrap().is_empty());
        assert_eq!(store.service_log_cursor(service, vm, runtime).unwrap(), 1);
        store
            .append_service_logs(service, (vm, runtime), 2, "active", true, &[])
            .unwrap();
        assert!(store.service_log_state(service, vm, runtime).unwrap().1);
        assert!(store.service_has_log_gap(service).unwrap());
        assert!(!store.service_has_log_gap(Uuid::new_v4()).unwrap());
        assert_eq!(
            store
                .service_events(service)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "log_gap")
                .count(),
            1
        );
        store
            .append_service_logs(service, (vm, runtime), 2, "active", true, &[])
            .unwrap();
        assert_eq!(
            store
                .service_events(service)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "log_gap")
                .count(),
            1
        );
    }

    #[test]
    fn inactive_service_history_is_deleted_without_new_records() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("state.db")).unwrap();
        let now = 2_000_000_000_000_i64;
        let cutoff = now - HISTORY_MS;
        let service = Uuid::new_v4().to_string();
        {
            let conn = store.lock();
            for (vm, at) in [("old", cutoff - 1_000), ("new", cutoff + 1_000)] {
                conn.execute("INSERT INTO service_observation_logs(service_id, vm_id, runtime_id, sequence, timestamp_ms, stream, message) VALUES (?1, ?2, 'runtime', 1, ?3, 'stdout', 'line')", params![service, vm, at]).unwrap();
                conn.execute("INSERT INTO service_observation_cursors(service_id, vm_id, runtime_id, sequence, status, updated_at) VALUES (?1, ?2, 'runtime', 1, 'inactive', ?3)", params![service, vm, at]).unwrap();
                conn.execute("INSERT INTO service_observation_events(service_id, timestamp, kind, severity, message) VALUES (?1, ?2, 'test', 'info', 'event')", params![service, at / 1000]).unwrap();
            }
        }
        store.prune_service_observations_at(now).unwrap();
        let conn = store.lock();
        for table in [
            "service_observation_logs",
            "service_observation_events",
            "service_observation_cursors",
        ] {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 1, "{table}");
        }
        let retained: String = conn
            .query_row("SELECT vm_id FROM service_observation_logs", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(retained, "new");
        let retained: String = conn
            .query_row("SELECT vm_id FROM service_observation_cursors", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(retained, "new");
    }
}

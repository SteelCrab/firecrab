//! Bounded, credential-free service events and health alerts.

use firecrab_api_types::{MicroServiceDeployment, ServicePhase};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use uuid::Uuid;

use super::{PersistenceError, Store};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS service_observation_events (
 id INTEGER PRIMARY KEY AUTOINCREMENT, service_id TEXT NOT NULL, timestamp INTEGER NOT NULL,
 kind TEXT NOT NULL, severity TEXT NOT NULL, message TEXT NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS service_observation_events_owner ON service_observation_events(service_id, id);
CREATE TABLE IF NOT EXISTS service_observation_alerts (
 service_id TEXT NOT NULL, kind TEXT NOT NULL, timestamp INTEGER NOT NULL,
 active INTEGER NOT NULL, message TEXT NOT NULL, PRIMARY KEY(service_id, kind)
) STRICT;
";

const HISTORY_LIMIT: i64 = 1000;
const HISTORY_SECONDS: i64 = 7 * 24 * 60 * 60;

/// A bounded lifecycle or alert transition. Messages never contain error details or secrets.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Event {
    pub id: i64,
    pub timestamp: String,
    pub kind: String,
    pub severity: String,
    pub message: String,
}

/// The latest state of an actionable alert.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Alert {
    pub timestamp: String,
    pub kind: String,
    pub message: String,
    pub active: bool,
}

fn timestamp(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0)
        .unwrap_or(chrono::DateTime::UNIX_EPOCH)
        .to_rfc3339()
}

fn phase_name(phase: ServicePhase) -> &'static str {
    match phase {
        ServicePhase::SwitchingTraffic => "switching traffic",
        ServicePhase::Draining => "draining",
        ServicePhase::RollingBack => "rolling back",
        ServicePhase::Queued => "queued",
        ServicePhase::Importing => "importing",
        ServicePhase::Starting => "starting",
        ServicePhase::CheckingHealth => "checking health",
        ServicePhase::ConnectingDomain => "connecting domain",
        ServicePhase::Running => "running",
        ServicePhase::Degraded => "degraded",
        ServicePhase::Stopping => "stopping",
        ServicePhase::Stopped => "stopped",
        ServicePhase::Cancelled => "cancelled",
        ServicePhase::Failed => "failed",
    }
}

pub(super) fn event(
    conn: &Connection,
    service: Uuid,
    now: i64,
    kind: &str,
    severity: &str,
    message: &str,
) -> Result<(), PersistenceError> {
    let id = service.to_string();
    conn.execute(
        "INSERT INTO service_observation_events(service_id,timestamp,kind,severity,message) VALUES (?1,?2,?3,?4,?5)",
        params![id, now, kind, severity, message],
    )?;
    conn.execute(
        "DELETE FROM service_observation_events WHERE service_id=?1 AND (timestamp < ?2 OR id NOT IN (SELECT id FROM service_observation_events WHERE service_id=?1 ORDER BY id DESC LIMIT ?3))",
        params![id, now.saturating_sub(HISTORY_SECONDS), HISTORY_LIMIT],
    )?;
    Ok(())
}

fn alert(
    conn: &Connection,
    service: Uuid,
    now: i64,
    kind: &str,
    active: bool,
    message: &str,
) -> Result<(), PersistenceError> {
    let id = service.to_string();
    let previous: Option<bool> = conn
        .query_row(
            "SELECT active FROM service_observation_alerts WHERE service_id=?1 AND kind=?2",
            params![id, kind],
            |row| row.get(0),
        )
        .optional()?;
    if previous == Some(active) || previous.is_none() && !active {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO service_observation_alerts(service_id,kind,timestamp,active,message) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(service_id,kind) DO UPDATE SET timestamp=excluded.timestamp,active=excluded.active,message=excluded.message",
        params![id, kind, now, active, message],
    )?;
    let transition = if active { "active" } else { "cleared" };
    event(
        conn,
        service,
        now,
        kind,
        if active { "warning" } else { "info" },
        &format!("{message}: {transition}"),
    )
}

/// Observe persisted state inside its owning transaction, never a transient live probe.
pub(super) fn observe(
    conn: &Connection,
    previous: Option<&MicroServiceDeployment>,
    current: &MicroServiceDeployment,
) -> Result<(), PersistenceError> {
    let now = chrono::Utc::now().timestamp();
    let service = current.service_id;
    if previous.is_none_or(|old| old.id != current.id || old.phase != current.phase) {
        event(
            conn,
            service,
            now,
            "deployment",
            if current.phase == ServicePhase::Failed {
                "error"
            } else {
                "info"
            },
            &format!("Deployment: {}", phase_name(current.phase)),
        )?;
    }

    let failed = current.phase == ServicePhase::Failed
        || current
            .update
            .as_ref()
            .is_some_and(|update| update.failure.is_some());
    if failed {
        alert(
            conn,
            service,
            now,
            "deployment_failed",
            true,
            "Deployment failed",
        )?;
    } else if current.phase == ServicePhase::Running
        && previous.is_some_and(|old| old.id == current.id && old.phase.is_busy())
        && current
            .recovery
            .as_ref()
            .is_none_or(|recovery| recovery.repair.is_none())
    {
        // Restoring the previous release is not a successful new deployment.
        alert(
            conn,
            service,
            now,
            "deployment_failed",
            false,
            "Deployment failed",
        )?;
    }

    let repair = current
        .recovery
        .as_ref()
        .and_then(|health| health.repair.as_ref());
    let stopped = matches!(
        current.phase,
        ServicePhase::Stopping | ServicePhase::Stopped | ServicePhase::Cancelled
    );
    let unhealthy = current.phase == ServicePhase::Degraded || repair.is_some() && !stopped;
    if unhealthy {
        alert(
            conn,
            service,
            now,
            "health_degraded",
            true,
            "Service health is degraded",
        )?;
    } else if stopped || current.phase == ServicePhase::Running && repair.is_none() {
        alert(
            conn,
            service,
            now,
            "health_degraded",
            false,
            "Service health is degraded",
        )?;
    }

    let repeated = current.recovery.as_ref().is_some_and(|health| {
        health
            .replicas
            .values()
            .any(|replica| replica.attempts >= 5)
    });
    if repeated {
        alert(
            conn,
            service,
            now,
            "restart_loop",
            true,
            "Replica recovery repeatedly failed",
        )?;
    } else if stopped || current.phase == ServicePhase::Running && repair.is_none() {
        alert(
            conn,
            service,
            now,
            "restart_loop",
            false,
            "Replica recovery repeatedly failed",
        )?;
    }
    Ok(())
}

/// Decode the prior snapshot without holding any lock outside the transaction.
pub(super) fn previous(
    conn: &Connection,
    service: Uuid,
) -> Result<Option<MicroServiceDeployment>, PersistenceError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT deployment FROM micro_service_deployments WHERE service_id=?1",
            [service.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    raw.map(|raw| {
        serde_json::from_str(&raw).map_err(|_| PersistenceError::CorruptRecord {
            id: service.to_string(),
            reason: "invalid service deployment".into(),
        })
    })
    .transpose()
}

impl Store {
    /// Read only recent events for this exact service.
    pub(crate) fn service_events(&self, service: Uuid) -> Result<Vec<Event>, PersistenceError> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT id,timestamp,kind,severity,message FROM service_observation_events WHERE service_id=?1 AND timestamp>=?2 ORDER BY id DESC LIMIT 200")?;
        let rows = stmt.query_map(
            params![
                service.to_string(),
                chrono::Utc::now()
                    .timestamp()
                    .saturating_sub(HISTORY_SECONDS)
            ],
            |row| {
                Ok(Event {
                    id: row.get(0)?,
                    timestamp: timestamp(row.get(1)?),
                    kind: row.get(2)?,
                    severity: row.get(3)?,
                    message: row.get(4)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Read a bounded page after the exporter's current cursor.
    pub(crate) fn service_events_after(
        &self,
        service: Uuid,
        after: i64,
    ) -> Result<Vec<Event>, PersistenceError> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT id,timestamp,kind,severity,message FROM service_observation_events WHERE service_id=?1 AND id>?2 AND timestamp>=?3 ORDER BY id LIMIT 128")?;
        let rows = stmt.query_map(
            params![
                service.to_string(),
                after,
                chrono::Utc::now()
                    .timestamp()
                    .saturating_sub(HISTORY_SECONDS)
            ],
            |row| {
                Ok(Event {
                    id: row.get(0)?,
                    timestamp: timestamp(row.get(1)?),
                    kind: row.get(2)?,
                    severity: row.get(3)?,
                    message: row.get(4)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Starting an export does not transmit observations from before opt-in.
    pub(crate) fn service_observation_highwater(
        &self,
        service: Uuid,
    ) -> Result<(i64, i64), PersistenceError> {
        let conn = self.lock();
        let events = conn.query_row(
            "SELECT COALESCE(MAX(id),0) FROM service_observation_events WHERE service_id=?1",
            [service.to_string()],
            |row| row.get(0),
        )?;
        let logs = conn.query_row(
            "SELECT COALESCE(MAX(id),0) FROM service_observation_logs WHERE service_id=?1",
            [service.to_string()],
            |row| row.get(0),
        )?;
        Ok((events, logs))
    }

    /// Alert state changes only when durable Deploy or health state changes.
    pub(crate) fn service_alerts(&self, service: Uuid) -> Result<Vec<Alert>, PersistenceError> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT timestamp,kind,message,active FROM service_observation_alerts WHERE service_id=?1 ORDER BY active DESC,timestamp DESC,kind")?;
        let rows = stmt.query_map([service.to_string()], |row| {
            Ok(Alert {
                timestamp: timestamp(row.get(0)?),
                kind: row.get(1)?,
                message: row.get(2)?,
                active: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use firecrab_api_types::{
        DeployMicroServiceRequest, MicroServiceResponse, MicroServiceSpec, ReplicaHealth,
        ReplicaRepair, ReplicaRepairStage, ServiceRecovery,
    };
    use std::collections::BTreeMap;

    fn deployment(service: Uuid) -> MicroServiceDeployment {
        let spec: MicroServiceSpec = serde_json::from_value(serde_json::json!({
            "name": format!("app-{}", service.simple()), "image": "ghcr.io/team/app:v1",
            "port": 8080, "ram": 128, "cpu": 1
        }))
        .unwrap();
        MicroServiceDeployment {
            recovery: None,
            started_at: 0,
            finished_at: None,
            update: None,
            id: Uuid::new_v4(),
            service_id: service,
            phase: ServicePhase::Running,
            spec,
            request: DeployMicroServiceRequest {
                micro_network_id: None,
                host_port: 0,
            },
            digest: None,
            instances: vec![],
            log: vec![],
            domain_route_owned: false,
            internal_route_owned: false,
            error: None,
        }
    }

    fn fixture() -> (tempfile::TempDir, Store, MicroServiceDeployment) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("state.db")).unwrap();
        let deployment = deployment(Uuid::new_v4());
        store
            .insert_micro_service(&MicroServiceResponse {
                id: deployment.service_id,
                spec: deployment.spec.clone(),
            })
            .unwrap();
        (root, store, deployment)
    }

    #[test]
    fn failed_attempt_stays_alerted_after_restore_until_a_new_deploy_succeeds() {
        let (_root, store, previous) = fixture();
        store.save_service_deployment(&previous).unwrap();
        let mut failed = previous.clone();
        failed.id = Uuid::new_v4();
        failed.phase = ServicePhase::Failed;
        failed.error = Some("private-value-not-for-events".into());
        store.save_service_deployment(&failed).unwrap();
        store.restore_service_release(&failed, &previous).unwrap();
        assert!(
            store
                .service_alerts(previous.service_id)
                .unwrap()
                .iter()
                .any(|item| item.kind == "deployment_failed" && item.active)
        );
        assert!(
            !serde_json::to_string(&store.service_events(previous.service_id).unwrap())
                .unwrap()
                .contains("private-value-not-for-events")
        );

        let mut next = previous.clone();
        next.id = Uuid::new_v4();
        next.phase = ServicePhase::Queued;
        store.save_service_deployment(&next).unwrap();
        next.phase = ServicePhase::Running;
        store.save_service_deployment(&next).unwrap();
        assert!(
            store
                .service_alerts(previous.service_id)
                .unwrap()
                .iter()
                .any(|item| item.kind == "deployment_failed" && !item.active)
        );
    }

    #[test]
    fn health_and_restart_alerts_follow_persisted_transitions_without_duplicates() {
        let (_root, store, mut current) = fixture();
        store.save_service_deployment(&current).unwrap();
        current.phase = ServicePhase::Degraded;
        let vm = Uuid::new_v4();
        current.recovery = Some(Box::new(ServiceRecovery {
            replicas: BTreeMap::from([(
                vm,
                ReplicaHealth {
                    attempts: 5,
                    ..Default::default()
                },
            )]),
            repair: Some(ReplicaRepair {
                id: Uuid::new_v4(),
                vm_id: vm,
                replacement_id: None,
                stage: ReplicaRepairStage::Withdraw,
                transition: None,
            }),
        }));
        store.save_service_deployment(&current).unwrap();
        let count = store.service_events(current.service_id).unwrap().len();
        store.save_service_deployment(&current).unwrap();
        assert_eq!(
            store.service_events(current.service_id).unwrap().len(),
            count
        );
        current.phase = ServicePhase::Queued;
        store.save_service_deployment(&current).unwrap();
        let alerts = store.service_alerts(current.service_id).unwrap();
        assert!(
            alerts
                .iter()
                .any(|a| a.kind == "health_degraded" && a.active)
        );
        assert!(alerts.iter().any(|a| a.kind == "restart_loop" && a.active));
        assert!(store.service_alerts(Uuid::new_v4()).unwrap().is_empty());

        current.phase = ServicePhase::Running;
        current.recovery = None;
        store.save_service_deployment(&current).unwrap();
        assert!(
            store
                .service_alerts(current.service_id)
                .unwrap()
                .iter()
                .all(|a| !a.active)
        );
    }

    #[test]
    fn event_history_is_bounded_and_deleted_with_its_service() {
        let (_root, store, deployment) = fixture();
        let now = chrono::Utc::now().timestamp();
        {
            let mut conn = store.lock();
            let tx = conn.transaction().unwrap();
            for _ in 0..HISTORY_LIMIT + 4 {
                event(&tx, deployment.service_id, now, "test", "info", "bounded").unwrap();
            }
            tx.commit().unwrap();
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM service_observation_events WHERE service_id=?1",
                    [deployment.service_id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, HISTORY_LIMIT);
        }
        assert!(store.delete_micro_service(deployment.service_id).unwrap());
        assert!(
            store
                .service_events(deployment.service_id)
                .unwrap()
                .is_empty()
        );
    }
}

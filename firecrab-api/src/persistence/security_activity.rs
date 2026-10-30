//! Retained, aggregate application-log signals with release and replica identities.
use super::{PersistenceError, Store};
use firecrab_api_types::MicroServiceDeployment;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS service_security_activity (
 service_id TEXT NOT NULL, deployment_id TEXT NOT NULL, vm_id TEXT NOT NULL,
 bucket INTEGER NOT NULL, denied INTEGER NOT NULL,
 PRIMARY KEY(service_id,deployment_id,vm_id,bucket)
) STRICT;
CREATE INDEX IF NOT EXISTS service_security_activity_owner ON service_security_activity(service_id,bucket);
";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Activity {
    pub deployment_id: String,
    pub replica_ids: Vec<String>,
    pub bucket_start: i64,
    pub count: u64,
    pub severity: &'static str,
    pub source: &'static str,
    pub recommendation: &'static str,
}

/// Resolve ownership at collection time, before replica removal loses it.
pub(super) fn release_for_replica(
    conn: &Connection,
    service: Uuid,
    vm: Uuid,
) -> Result<Option<Uuid>, PersistenceError> {
    let encoded: Option<String> = conn
        .query_row(
            "SELECT deployment FROM micro_service_deployments WHERE service_id=?1",
            [service.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    let Some(encoded) = encoded else {
        return Ok(None);
    };
    let deployment: MicroServiceDeployment =
        serde_json::from_str(&encoded).map_err(|error| PersistenceError::CorruptRecord {
            id: service.to_string(),
            reason: error.to_string(),
        })?;
    if let Some(update) = deployment.update.as_ref()
        && update
            .previous
            .instances
            .iter()
            .any(|instance| instance.vm_id == vm)
    {
        return Ok(Some(update.previous.id));
    }
    Ok(deployment
        .instances
        .iter()
        .any(|instance| instance.vm_id == vm)
        .then_some(deployment.id))
}

/// Called only for newly inserted log chunks, so retries cannot double-count.
pub(super) fn record(
    conn: &Connection,
    service: Uuid,
    deployment: Uuid,
    vm: Uuid,
    timestamp_ms: i64,
    message: &str,
    now_ms: i64,
) -> Result<(), PersistenceError> {
    if timestamp_ms < now_ms.saturating_sub(7 * 24 * 60 * 60 * 1000)
        || timestamp_ms > now_ms.saturating_add(60_000)
    {
        return Ok(());
    }
    let denied = message
        .lines()
        .filter(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|value| value.get("status").and_then(|status| status.as_u64()))
                .is_some_and(|status| matches!(status, 401 | 403))
        })
        .count();
    if denied == 0 {
        return Ok(());
    }
    let bucket = timestamp_ms.div_euclid(60_000) * 60;
    conn.execute("INSERT INTO service_security_activity(service_id,deployment_id,vm_id,bucket,denied) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(service_id,deployment_id,vm_id,bucket) DO UPDATE SET denied=MIN(1000000,denied+excluded.denied)", params![service.to_string(),deployment.to_string(),vm.to_string(),bucket,denied as i64])?;
    conn.execute("DELETE FROM service_security_activity WHERE service_id=?1 AND (bucket < ?2 OR rowid NOT IN (SELECT rowid FROM service_security_activity WHERE service_id=?1 ORDER BY bucket DESC,rowid DESC LIMIT 10000))", params![service.to_string(),now_ms.div_euclid(1000).saturating_sub(7*24*60*60)])?;
    Ok(())
}

impl Store {
    pub(crate) fn security_activity(
        &self,
        service: Uuid,
    ) -> Result<Vec<Activity>, PersistenceError> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT deployment_id,vm_id,bucket,denied FROM service_security_activity WHERE service_id=?1 AND bucket>=?2 ORDER BY bucket DESC LIMIT 10000")?;
        let rows = stmt.query_map(
            params![
                service.to_string(),
                chrono::Utc::now()
                    .timestamp()
                    .saturating_sub(7 * 24 * 60 * 60)
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )?;
        let mut grouped: BTreeMap<(String, i64), (u64, BTreeSet<String>)> = BTreeMap::new();
        for row in rows {
            let (release, replica, bucket, count) = row?;
            let item = grouped.entry((release, bucket)).or_default();
            item.0 = item
                .0
                .saturating_add(u64::try_from(count).unwrap_or_default());
            item.1.insert(replica);
        }
        let mut findings = Vec::new();
        for (deployment_id, end) in grouped.keys() {
            let mut count = 0u64;
            let mut replica_ids = BTreeSet::new();
            for (_, (value, replicas)) in grouped.range(
                (deployment_id.clone(), end.saturating_sub(240))..=(deployment_id.clone(), *end),
            ) {
                count = count.saturating_add(*value);
                replica_ids.extend(replicas.iter().cloned());
            }
            if count >= 10 {
                findings.push(Activity {
                    deployment_id:deployment_id.clone(), replica_ids:replica_ids.into_iter().collect(),
                    bucket_start:end.saturating_sub(240), count, severity:"medium",
                    source:"structured application logs with status codes",
                    recommendation:"Review the matching application logs and authentication policy for repeated 401/403 status codes.",
                });
            }
        }
        findings.sort_by_key(|finding| std::cmp::Reverse(finding.bucket_start));
        findings.truncate(100);
        Ok(findings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        persistence::service_logs::InputLog, state::AppState, templates::TemplateRegistry,
    };
    use firecrab_api_types::{MicroServiceDeployment, MicroServiceResponse, MicroServiceSpec};
    use serde_json::json;

    #[tokio::test]
    async fn retained_signal_spans_minute_boundaries_and_replicas() {
        let root = tempfile::tempdir().unwrap();
        let state = AppState::with_db_file(
            TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap(),
            root.path().join("state.db"),
        )
        .await
        .unwrap();
        let service = Uuid::new_v4();
        let release = Uuid::new_v4();
        let first_vm = Uuid::new_v4();
        let second_vm = Uuid::new_v4();
        let now_ms = chrono::Utc::now().timestamp_millis();
        let minute = now_ms.div_euclid(60_000) * 60_000;
        {
            let conn = state.store.lock();
            record(
                &conn,
                service,
                release,
                first_vm,
                minute - 1,
                &"{\"status\":403}\n".repeat(9),
                now_ms,
            )
            .unwrap();
            record(
                &conn,
                service,
                release,
                second_vm,
                minute,
                "{\"status\":401}",
                now_ms,
            )
            .unwrap();
        }
        let findings = state.store.security_activity(service).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].count, 10);
        assert_eq!(findings[0].replica_ids.len(), 2);
    }

    #[tokio::test]
    async fn retained_signals_keep_release_and_replica_after_replacement_without_duplicates() {
        let root = tempfile::tempdir().unwrap();
        let state = AppState::with_db_file(
            TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap(),
            root.path().join("state.db"),
        )
        .await
        .unwrap();
        let service = Uuid::new_v4();
        let first_release = Uuid::new_v4();
        let second_release = Uuid::new_v4();
        let first_vm = Uuid::new_v4();
        let second_vm = Uuid::new_v4();
        let spec: MicroServiceSpec = serde_json::from_value(
            json!({"name":"web","image":"ghcr.io/team/web:v1","port":8080,"ram":128,"cpu":1}),
        )
        .unwrap();
        state
            .store
            .insert_micro_service(&MicroServiceResponse {
                id: service,
                spec: spec.clone(),
            })
            .unwrap();
        let deployment = |id, vm| -> MicroServiceDeployment {
            serde_json::from_value(json!({"id":id,"serviceId":service,"phase":"running","spec":spec,"request":{"hostPort":0},"digest":null,"log":[],"error":null,"instances":[{"vmId":vm,"ipv4":"172.30.0.5","hostPort":18080,"state":"running","healthy":true}]})).unwrap()
        };
        state
            .store
            .save_service_deployment(&deployment(first_release, first_vm))
            .unwrap();
        let entry = InputLog {
            sequence: 1,
            timestamp: chrono::Utc::now().timestamp_millis(),
            stream: "stdout".into(),
            message: "{\"status\":403,\"token\":\"secret\"}\n".repeat(10),
        };
        let runtime = Uuid::new_v4();
        state
            .store
            .append_service_logs(
                service,
                (first_vm, runtime),
                1,
                "active",
                false,
                std::slice::from_ref(&entry),
            )
            .unwrap();
        state
            .store
            .append_service_logs(
                service,
                (first_vm, runtime),
                1,
                "active",
                false,
                std::slice::from_ref(&entry),
            )
            .unwrap();
        state
            .store
            .save_service_deployment(&deployment(second_release, second_vm))
            .unwrap();
        state
            .store
            .append_service_logs(
                service,
                (second_vm, Uuid::new_v4()),
                1,
                "active",
                false,
                &[entry],
            )
            .unwrap();
        let history = state.store.security_activity(service).unwrap();
        assert_eq!(history.len(), 2);
        for (release, vm) in [(first_release, first_vm), (second_release, second_vm)] {
            let entry = history
                .iter()
                .find(|entry| entry.deployment_id == release.to_string())
                .unwrap();
            assert_eq!(entry.count, 10);
            assert_eq!(entry.replica_ids, vec![vm.to_string()]);
            assert!(!serde_json::to_string(entry).unwrap().contains("secret"));
        }
    }
}

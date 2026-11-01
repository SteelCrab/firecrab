//! Durable image-scan outcomes scoped to an exact service release.
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{PersistenceError, Store};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS service_security_scans (
 service_id TEXT NOT NULL, deployment_id TEXT NOT NULL, digest TEXT NOT NULL,
 scanned_at INTEGER NOT NULL, status TEXT NOT NULL, data TEXT NOT NULL,
 PRIMARY KEY(service_id, deployment_id, digest)
) STRICT;
CREATE INDEX IF NOT EXISTS service_security_scans_owner ON service_security_scans(service_id, scanned_at);
";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Scan {
    pub deployment_id: Uuid,
    #[serde(default)]
    pub request_id: Option<Uuid>,
    pub digest: String,
    #[serde(default)]
    pub replica_ids: Vec<Uuid>,
    pub scanned_at: i64,
    pub status: String,
    pub findings: Vec<crate::service_security::cve::Finding>,
    #[serde(default)]
    pub limitations: Vec<String>,
    pub database_updated_at: Option<String>,
}

impl Store {
    pub(crate) fn security_scan(
        &self,
        service: Uuid,
        deployment: Uuid,
        digest: &str,
    ) -> Result<Option<Scan>, PersistenceError> {
        let data: Option<String> = self.lock().query_row(
            "SELECT data FROM service_security_scans WHERE service_id=?1 AND deployment_id=?2 AND digest=?3",
            params![service.to_string(),deployment.to_string(),digest], |row| row.get(0),
        ).optional()?;
        data.map(|value| {
            serde_json::from_str(&value).map_err(|error| PersistenceError::CorruptRecord {
                id: service.to_string(),
                reason: error.to_string(),
            })
        })
        .transpose()
    }

    /// List retained scans for prior release identities.
    pub(crate) fn security_scans(&self, service: Uuid) -> Result<Vec<Scan>, PersistenceError> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT data FROM service_security_scans WHERE service_id=?1 ORDER BY scanned_at DESC,rowid DESC LIMIT 20")?;
        let rows = stmt.query_map([service.to_string()], |row| row.get::<_, String>(0))?;
        rows.map(|row| {
            let encoded = row?;
            serde_json::from_str(&encoded).map_err(|error| PersistenceError::CorruptRecord {
                id: service.to_string(),
                reason: error.to_string(),
            })
        })
        .collect()
    }

    /// Persist only while the scanned service still exists. Keep bounded release history.
    pub(crate) fn save_security_scan(
        &self,
        service: Uuid,
        scan: &Scan,
    ) -> Result<bool, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let changed = write_scan(&tx, service, scan)?;
        tx.commit()?;
        Ok(changed)
    }

    /// A scan started before a new request must not overwrite that request.
    pub(crate) fn save_security_scan_if_unchanged(
        &self,
        service: Uuid,
        scan: &Scan,
        expected: Option<&Scan>,
    ) -> Result<bool, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let encoded: Option<String> = tx.query_row("SELECT data FROM service_security_scans WHERE service_id=?1 AND deployment_id=?2 AND digest=?3", params![service.to_string(),scan.deployment_id.to_string(),scan.digest], |row| row.get(0)).optional()?;
        let current: Option<Scan> = encoded
            .map(|value| {
                serde_json::from_str(&value).map_err(|error| PersistenceError::CorruptRecord {
                    id: service.to_string(),
                    reason: error.to_string(),
                })
            })
            .transpose()?;
        if current.as_ref() != expected {
            return Ok(false);
        }
        let changed = write_scan(&tx, service, scan)?;
        tx.commit()?;
        Ok(changed)
    }
}

fn write_scan(
    conn: &rusqlite::Connection,
    service: Uuid,
    scan: &Scan,
) -> Result<bool, PersistenceError> {
    let data = serde_json::to_string(scan).map_err(|error| PersistenceError::CorruptRecord {
        id: service.to_string(),
        reason: error.to_string(),
    })?;
    let changed = conn.execute("INSERT INTO service_security_scans(service_id,deployment_id,digest,scanned_at,status,data) SELECT ?1,?2,?3,?4,?5,?6 WHERE EXISTS(SELECT 1 FROM micro_services WHERE id=?1) ON CONFLICT(service_id,deployment_id,digest) DO UPDATE SET scanned_at=excluded.scanned_at,status=excluded.status,data=excluded.data", params![service.to_string(),scan.deployment_id.to_string(),scan.digest,scan.scanned_at,scan.status,data])?;
    conn.execute("DELETE FROM service_security_scans WHERE service_id=?1 AND rowid NOT IN (SELECT rowid FROM service_security_scans WHERE service_id=?1 ORDER BY scanned_at DESC,rowid DESC LIMIT 20)", [service.to_string()])?;
    Ok(changed > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{state::AppState, templates::TemplateRegistry};
    use firecrab_api_types::{MicroServiceResponse, MicroServiceSpec};
    use serde_json::json;

    #[tokio::test]
    async fn scan_is_scoped_to_service_deployment_and_digest_and_removed_with_service() {
        let root = tempfile::tempdir().unwrap();
        let state = AppState::with_db_file(
            TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap(),
            root.path().join("state.db"),
        )
        .await
        .unwrap();
        let service = Uuid::new_v4();
        let deployment = Uuid::new_v4();
        let spec: MicroServiceSpec = serde_json::from_value(
            json!({"name":"web","image":"ghcr.io/team/web:v1","port":8080,"ram":128,"cpu":1}),
        )
        .unwrap();
        state
            .store
            .insert_micro_service(&MicroServiceResponse { id: service, spec })
            .unwrap();
        let scan = Scan {
            deployment_id: deployment,
            request_id: None,
            digest: "sha256:abc".into(),
            replica_ids: vec![],
            scanned_at: 1,
            status: "complete".into(),
            findings: vec![],
            limitations: vec![],
            database_updated_at: Some("2026-10-30T00:00:00Z".into()),
        };
        assert!(state.store.save_security_scan(service, &scan).unwrap());
        assert!(
            state
                .store
                .security_scan(service, deployment, "sha256:abc")
                .unwrap()
                .is_some()
        );
        assert!(
            state
                .store
                .security_scan(service, deployment, "sha256:other")
                .unwrap()
                .is_none()
        );
        assert!(
            state
                .store
                .security_scan(service, Uuid::new_v4(), "sha256:abc")
                .unwrap()
                .is_none()
        );
        assert!(
            state
                .store
                .security_scan(Uuid::new_v4(), deployment, "sha256:abc")
                .unwrap()
                .is_none()
        );
        let mut queued = scan.clone();
        queued.status = "queued".into();
        queued.request_id = Some(Uuid::new_v4());
        assert!(state.store.save_security_scan(service, &queued).unwrap());
        assert!(
            !state
                .store
                .save_security_scan_if_unchanged(service, &scan, None)
                .unwrap()
        );
        assert!(
            !state
                .store
                .save_security_scan_if_unchanged(service, &scan, Some(&scan))
                .unwrap()
        );
        assert_eq!(
            state
                .store
                .security_scan(service, deployment, "sha256:abc")
                .unwrap()
                .unwrap()
                .request_id,
            queued.request_id
        );
        let mut finished = scan.clone();
        finished.request_id = queued.request_id;
        assert!(
            state
                .store
                .save_security_scan_if_unchanged(service, &finished, Some(&queued))
                .unwrap()
        );
        state.store.delete_micro_service(service).unwrap();
        assert!(
            state
                .store
                .security_scan(service, deployment, "sha256:abc")
                .unwrap()
                .is_none()
        );
        assert!(!state.store.save_security_scan(service, &scan).unwrap());
    }
}

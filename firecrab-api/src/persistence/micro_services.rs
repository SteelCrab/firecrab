//! SQLite storage for service configuration and the latest deployment snapshot.

use firecrab_api_types::{MicroServiceDeployment, MicroServiceResponse, MicroServiceSpec};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use super::{PersistenceError, Store};

/// Store configuration as JSON while enforcing unique service names in SQLite.
pub(super) const CREATE_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS micro_services (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    spec TEXT NOT NULL
) STRICT";

/// Keep one durable deployment snapshot per service.
pub(super) const CREATE_DEPLOYMENTS_SQL: &str =
    "CREATE TABLE IF NOT EXISTS micro_service_deployments (
    service_id TEXT PRIMARY KEY,
    deployment TEXT NOT NULL
) STRICT";

/// Separate immutable successful releases from mutable deployment attempts.
pub(super) const HISTORY_SQL: &str = "
CREATE TABLE IF NOT EXISTS micro_service_attempts (
 id TEXT PRIMARY KEY, service_id TEXT NOT NULL, deployment TEXT NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS service_attempts_owner ON micro_service_attempts(service_id);
CREATE TABLE IF NOT EXISTS micro_service_releases (
 id TEXT PRIMARY KEY, service_id TEXT NOT NULL, deployment TEXT NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS service_releases_owner ON micro_service_releases(service_id);
";

/// Import the active pre-Deploy release once; never infer success from failed attempts.
pub(super) fn migrate_history(conn: &rusqlite::Connection) -> Result<(), PersistenceError> {
    let mut statement =
        conn.prepare("SELECT service_id, deployment FROM micro_service_deployments")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (id, text) = row?;
        let mut deployment: MicroServiceDeployment =
            serde_json::from_str(&text).map_err(|error| PersistenceError::CorruptRecord {
                id: id.clone(),
                reason: error.to_string(),
            })?;
        conn.execute("INSERT OR IGNORE INTO micro_service_attempts (id, service_id, deployment) VALUES (?1, ?2, ?3)",
            params![deployment.id.to_string(), id, text])?;
        if matches!(
            deployment.phase,
            firecrab_api_types::ServicePhase::Running | firecrab_api_types::ServicePhase::Degraded
        ) && deployment.digest.is_some()
            && deployment
                .update
                .as_ref()
                .is_none_or(|update| update.completed)
        {
            deployment.instances.clear();
            deployment.recovery = None;
            deployment.update = None;
            deployment.domain_route_owned = false;
            deployment.internal_route_owned = false;
            let text = serde_json::to_string(&deployment).map_err(|error| {
                PersistenceError::CorruptRecord {
                    id: id.clone(),
                    reason: error.to_string(),
                }
            })?;
            conn.execute("INSERT OR IGNORE INTO micro_service_releases (id, service_id, deployment) VALUES (?1, ?2, ?3)",
                params![deployment.id.to_string(), id, text])?;
        }
    }
    Ok(())
}

pub(super) fn ensure_children_released(
    conn: &rusqlite::Connection,
    id: Uuid,
) -> Result<(), PersistenceError> {
    let managed: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM iac_resource_owners WHERE manager IS NOT NULL AND ((kind IN ('service_otlp','service_auto_deploy') AND resource_id=?1) OR (kind='service_deploy_token' AND resource_id LIKE ?1||':%')))", [id.to_string()], |row| row.get(0))?;
    let database_dependencies: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM database_bindings WHERE service_id=?1) OR EXISTS(SELECT 1 FROM database_triggers WHERE json_extract(spec,'$.serviceId')=?1)", [id.to_string()], |r|r.get(0))?;
    if managed || database_dependencies {
        return Err(PersistenceError::IacManagedDependent);
    }
    Ok(())
}

impl Store {
    pub(crate) fn ensure_service_children_released(
        &self,
        id: Uuid,
    ) -> Result<(), PersistenceError> {
        ensure_children_released(&self.lock(), id)
    }

    /// Pausing a replica independently would make service health/replica control replace it.
    pub fn vm_has_service_owner(&self, id: Uuid) -> Result<bool, PersistenceError> {
        Ok(self.service_deployment_for_vm(id)?.is_some())
    }

    /// Find the persisted deployment that owns a replica, including during boot.
    pub fn service_deployment_for_vm(
        &self,
        id: Uuid,
    ) -> Result<Option<MicroServiceDeployment>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare("SELECT deployment FROM micro_service_deployments")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        for row in rows {
            let deployment: MicroServiceDeployment =
                serde_json::from_str(&row?).map_err(|error| PersistenceError::CorruptRecord {
                    id: id.to_string(),
                    reason: error.to_string(),
                })?;
            if deployment
                .instances
                .iter()
                .any(|instance| instance.vm_id == id)
                || deployment
                    .update
                    .as_ref()
                    .is_some_and(|u| u.retiring.contains(&id))
            {
                return Ok(Some(deployment));
            }
            if let Some(update) = &deployment.update
                && update
                    .previous
                    .instances
                    .iter()
                    .any(|instance| instance.vm_id == id)
            {
                return Ok(Some((*update.previous).clone()));
            }
        }
        Ok(None)
    }

    /// List saved configurations in stable name and identifier order.
    pub fn list_micro_services(&self) -> Result<Vec<MicroServiceResponse>, PersistenceError> {
        let conn = self.lock();
        let mut statement =
            conn.prepare("SELECT id, spec FROM micro_services ORDER BY name, id")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.map(|row| {
            let (id, spec) = row?;
            decode(id, spec)
        })
        .collect()
    }

    /// Read one configuration, returning `None` if the identifier is unknown.
    pub fn micro_service(
        &self,
        id: Uuid,
    ) -> Result<Option<MicroServiceResponse>, PersistenceError> {
        let row: Option<(String, String)> = self
            .lock()
            .query_row(
                "SELECT id, spec FROM micro_services WHERE id = ?1",
                [id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(id, spec)| decode(id, spec)).transpose()
    }

    /// Insert a configuration, rejecting names already assigned to another service.
    pub fn insert_micro_service(
        &self,
        service: &MicroServiceResponse,
    ) -> Result<(), PersistenceError> {
        let spec = encode(service)?;
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO micro_services (id, name, spec) VALUES (?1, ?2, ?3)",
            params![service.id.to_string(), service.spec.name, spec],
        )
        .map_err(|error| write_error(error, &service.spec.name))?;
        super::auto_deploy::configuration(&tx, service)?;
        tx.commit()?;
        Ok(())
    }

    /// Commit configuration and policy together; a failed edit leaves both unchanged.
    #[cfg(test)]
    pub fn update_micro_service(
        &self,
        service: &MicroServiceResponse,
    ) -> Result<bool, PersistenceError> {
        self.update_micro_service_authority(service, None, None)
    }

    pub(crate) fn update_micro_service_checked(
        &self,
        service: &MicroServiceResponse,
        authority: &super::iac_auto_deploy::Authority,
        operation: Option<Uuid>,
    ) -> Result<bool, PersistenceError> {
        self.update_micro_service_authority(service, Some(authority), operation)
    }

    fn update_micro_service_authority(
        &self,
        service: &MicroServiceResponse,
        authority: Option<&super::iac_auto_deploy::Authority>,
        operation: Option<Uuid>,
    ) -> Result<bool, PersistenceError> {
        let spec = encode(service)?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(authority) = authority {
            super::iac_auto_deploy::compare(&tx, service.id, authority)?;
        }
        if !super::stacks::service_mutation_allowed(&tx, service.id, operation)? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        super::iac_auto_deploy::policy_change(&tx, service)?;
        let changed = tx
            .execute(
                "UPDATE micro_services SET name = ?2, spec = ?3 WHERE id = ?1",
                params![service.id.to_string(), service.spec.name, spec],
            )
            .map_err(|error| write_error(error, &service.spec.name))?;
        if changed != 0 {
            super::auto_deploy::configuration(&tx, service)?;
        }
        tx.commit()?;
        Ok(changed != 0)
    }

    /// Delete a configuration and its deployment record in one transaction.
    /// Return whether the service existed; the caller must clean up its VMs first.
    #[cfg(test)]
    pub fn delete_micro_service(&self, id: Uuid) -> Result<bool, PersistenceError> {
        self.delete_micro_service_checked(id, None)
    }

    pub(crate) fn delete_micro_service_checked(
        &self,
        id: Uuid,
        operation: Option<Uuid>,
    ) -> Result<bool, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        super::iac_native::ensure_idle(
            &tx,
            &firecrab_stack::ManagedResource {
                kind: "service".into(),
                id: id.to_string(),
            },
        )?;
        if !super::stacks::service_mutation_allowed(&tx, id, operation)? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        ensure_children_released(&tx, id)?;
        tx.execute(
            "DELETE FROM service_deploy_queue WHERE service_id = ?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM service_security_database_links WHERE service_id = ?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM service_security_database_activity WHERE service_id = ?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM service_security_activity WHERE service_id = ?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM service_security_ingress WHERE service_id = ?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM service_security_scans WHERE service_id = ?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM micro_service_attempts WHERE service_id = ?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM micro_service_releases WHERE service_id = ?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM micro_service_deployments WHERE service_id = ?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM service_tag_watches WHERE service_id=?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM service_deploy_tokens WHERE service_id=?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM service_ci_requests WHERE service_id=?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM service_observation_events WHERE service_id=?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM service_observation_alerts WHERE service_id=?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM service_observation_logs WHERE service_id=?1",
            [id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM service_observation_cursors WHERE service_id=?1",
            [id.to_string()],
        )?;
        super::otlp_config::delete(&tx, id)?;
        let changed = tx.execute("DELETE FROM micro_services WHERE id = ?1", [id.to_string()])?;
        tx.commit()?;
        Ok(changed != 0)
    }

    /// Read the latest saved deployment, reporting malformed JSON as a corrupt record.
    pub fn service_deployment(
        &self,
        id: Uuid,
    ) -> Result<Option<MicroServiceDeployment>, PersistenceError> {
        let text: Option<String> = self
            .lock()
            .query_row(
                "SELECT deployment FROM micro_service_deployments WHERE service_id = ?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        text.map(|text| {
            serde_json::from_str(&text).map_err(|error| PersistenceError::CorruptRecord {
                id: id.to_string(),
                reason: error.to_string(),
            })
        })
        .transpose()
    }

    /// Insert or replace the latest deployment snapshot for its service.
    pub fn save_service_deployment(
        &self,
        deployment: &MicroServiceDeployment,
    ) -> Result<(), PersistenceError> {
        self.save_deployment_with_ci(deployment, None, None)
    }

    /// Commit CI authorization/identity and queue intent as one transaction.
    pub(crate) fn save_ci_deployment(
        &self,
        deployment: &MicroServiceDeployment,
        admission: &super::ci_deploy::Admission,
    ) -> Result<(), PersistenceError> {
        self.save_deployment_with_ci(deployment, Some(admission), None)
    }

    pub(crate) fn save_authorized_deployment(
        &self,
        deployment: &MicroServiceDeployment,
        claim: &super::iac_auto_deploy::DeploymentAuthority,
    ) -> Result<(), PersistenceError> {
        self.save_deployment_with_ci(deployment, None, Some(claim))
    }

    /// Ordinary worker snapshots do not re-authorize an already accepted CI credential.
    fn save_deployment_with_ci(
        &self,
        deployment: &MicroServiceDeployment,
        admission: Option<&super::ci_deploy::Admission>,
        automatic: Option<&super::iac_auto_deploy::DeploymentAuthority>,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(claim) = automatic {
            match claim {
                super::iac_auto_deploy::DeploymentAuthority::Automatic(watch) => {
                    super::iac_auto_deploy::admit(&tx, deployment, watch)?
                }
                super::iac_auto_deploy::DeploymentAuthority::Manual(authority, operation) => {
                    super::iac_auto_deploy::compare(&tx, deployment.service_id, authority)?;
                    if !super::stacks::service_mutation_allowed(
                        &tx,
                        deployment.service_id,
                        *operation,
                    )? {
                        return Err(PersistenceError::IacOwnershipConflict);
                    }
                }
            }
        }
        if let Some(admission) = admission {
            super::ci_deploy::admit(&tx, deployment, admission)?;
        }
        let previous = super::service_observation::previous(&tx, deployment.service_id)?;
        let mut observation = deployment.clone();
        if admission.is_none()
            && automatic.is_none()
            && let Some(previous) = previous.as_ref().filter(|p| p.id == observation.id)
        {
            // Runtime health/progress cannot revert an independently edited policy.
            observation.spec.auto_deploy = previous.spec.auto_deploy.clone();
        }
        let deployment = &observation;
        super::iac_auto_deploy::policy_change(
            &tx,
            &MicroServiceResponse {
                id: deployment.service_id,
                spec: deployment.spec.clone(),
            },
        )?;
        let text =
            serde_json::to_string(deployment).map_err(|error| PersistenceError::CorruptRecord {
                id: deployment.id.to_string(),
                reason: error.to_string(),
            })?;
        super::service_observation::observe(&tx, previous.as_ref(), deployment)?;
        tx.execute(
            "INSERT INTO micro_service_deployments (service_id, deployment) VALUES (?1, ?2)
             ON CONFLICT(service_id) DO UPDATE SET deployment = excluded.deployment",
            params![deployment.service_id.to_string(), text],
        )?;
        tx.execute(
            "INSERT INTO micro_service_attempts (id, service_id, deployment) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET deployment = excluded.deployment",
            params![
                deployment.id.to_string(),
                deployment.service_id.to_string(),
                text
            ],
        )?;
        super::deploy_queue::save(&tx, deployment)?;
        super::auto_deploy::deployment(&tx, deployment)?;
        super::ci_deploy::observe(&tx, deployment)?;
        if deployment.phase == firecrab_api_types::ServicePhase::Running
            && deployment.digest.is_some()
            && deployment
                .update
                .as_ref()
                .is_none_or(|update| update.completed)
        {
            let mut release = deployment.clone();
            release.recovery = None;
            release.update = None;
            // Releases describe reproducible inputs, not ownership of retired VMs.
            release.instances.clear();
            release.domain_route_owned = false;
            release.internal_route_owned = false;
            let release = serde_json::to_string(&release).map_err(|error| {
                PersistenceError::CorruptRecord {
                    id: deployment.id.to_string(),
                    reason: error.to_string(),
                }
            })?;
            tx.execute(
                "INSERT OR IGNORE INTO micro_service_releases (id, service_id, deployment) VALUES (?1, ?2, ?3)",
                params![deployment.id.to_string(), deployment.service_id.to_string(), release],
            )?;
            // Health maintenance keeps the running release, not a new desired configuration.
            if deployment.update.is_some() && deployment.recovery.is_none() {
                let service = MicroServiceResponse {
                    id: deployment.service_id,
                    spec: deployment.spec.clone(),
                };
                tx.execute(
                    "UPDATE micro_services SET name = ?2, spec = ?3 WHERE id = ?1",
                    params![service.id.to_string(), service.spec.name, encode(&service)?],
                )?;
            }
            tx.execute("DELETE FROM micro_service_releases WHERE service_id = ?1 AND id NOT IN
                (SELECT id FROM micro_service_releases WHERE service_id = ?1 ORDER BY rowid DESC LIMIT 20)",
                [deployment.service_id.to_string()])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Commit a failed attempt and its restored active release together.
    pub fn restore_service_release(
        &self,
        failed: &MicroServiceDeployment,
        restored: &MicroServiceDeployment,
    ) -> Result<(), PersistenceError> {
        let serialize = |deployment: &MicroServiceDeployment| {
            serde_json::to_string(deployment).map_err(|error| PersistenceError::CorruptRecord {
                id: deployment.id.to_string(),
                reason: error.to_string(),
            })
        };
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut current: MicroServiceSpec = serde_json::from_str(&tx.query_row(
            "SELECT spec FROM micro_services WHERE id=?1",
            [restored.service_id.to_string()],
            |row| row.get::<_, String>(0),
        )?)
        .map_err(|_| PersistenceError::IacOperationIdentity)?;
        let mut observation = restored.clone();
        observation.spec.auto_deploy = current.auto_deploy.take();
        let restored = &observation;
        let failed_text = serialize(failed)?;
        let restored_text = serialize(restored)?;
        let previous = super::service_observation::previous(&tx, failed.service_id)?;
        super::service_observation::observe(&tx, previous.as_ref(), failed)?;
        super::service_observation::observe(&tx, Some(failed), restored)?;
        tx.execute(
            "DELETE FROM service_deploy_queue WHERE operation_id = ?1",
            [failed.id.to_string()],
        )?;
        tx.execute(
            "UPDATE micro_service_attempts SET deployment = ?2 WHERE id = ?1",
            params![failed.id.to_string(), failed_text],
        )?;
        tx.execute(
            "UPDATE micro_service_attempts SET deployment = ?2 WHERE id = ?1",
            params![restored.id.to_string(), restored_text],
        )?;
        tx.execute(
            "UPDATE micro_service_deployments SET deployment = ?2 WHERE service_id = ?1",
            params![restored.service_id.to_string(), restored_text],
        )?;
        let service = MicroServiceResponse {
            id: restored.service_id,
            spec: restored.spec.clone(),
        };
        tx.execute(
            "UPDATE micro_services SET name = ?2, spec = ?3 WHERE id = ?1",
            params![service.id.to_string(), service.spec.name, encode(&service)?],
        )?;
        super::auto_deploy::deployment(&tx, failed)?;
        super::ci_deploy::observe(&tx, failed)?;
        tx.commit()?;
        Ok(())
    }

    /// Read an operation's last durable outcome, including failed attempts.
    pub fn service_attempt(
        &self,
        id: Uuid,
    ) -> Result<Option<MicroServiceDeployment>, PersistenceError> {
        let text: Option<String> = self
            .lock()
            .query_row(
                "SELECT deployment FROM micro_service_attempts WHERE id = ?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        text.map(|text| {
            serde_json::from_str(&text).map_err(|error| PersistenceError::CorruptRecord {
                id: id.to_string(),
                reason: error.to_string(),
            })
        })
        .transpose()
    }

    /// Most recent successful releases, scoped to one exact service.
    pub fn service_releases(
        &self,
        id: Uuid,
    ) -> Result<Vec<MicroServiceDeployment>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT deployment FROM micro_service_releases WHERE service_id = ?1 ORDER BY rowid DESC LIMIT 20")?;
        let rows = statement.query_map([id.to_string()], |row| row.get::<_, String>(0))?;
        rows.map(|row| {
            serde_json::from_str(&row?).map_err(|error| PersistenceError::CorruptRecord {
                id: id.to_string(),
                reason: error.to_string(),
            })
        })
        .collect()
    }
}

/// Serialize configuration with its service identifier attached to any error.
fn encode(service: &MicroServiceResponse) -> Result<String, PersistenceError> {
    serde_json::to_string(&service.spec).map_err(|error| PersistenceError::CorruptRecord {
        id: service.id.to_string(),
        reason: error.to_string(),
    })
}

/// Validate a stored UUID and deserialize its configuration JSON.
fn decode(id: String, spec: String) -> Result<MicroServiceResponse, PersistenceError> {
    let parsed = Uuid::parse_str(&id).map_err(|error| PersistenceError::CorruptRecord {
        id: id.clone(),
        reason: error.to_string(),
    })?;
    let spec: MicroServiceSpec =
        serde_json::from_str(&spec).map_err(|error| PersistenceError::CorruptRecord {
            id,
            reason: error.to_string(),
        })?;
    Ok(MicroServiceResponse { id: parsed, spec })
}

/// Map the unique-name constraint to a service error; preserve other database errors.
fn write_error(error: rusqlite::Error, name: &str) -> PersistenceError {
    if let rusqlite::Error::SqliteFailure(code, _) = &error
        && code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
    {
        return PersistenceError::DuplicateMicroServiceName {
            name: name.to_owned(),
        };
    }
    PersistenceError::Database(error)
}

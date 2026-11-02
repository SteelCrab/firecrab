//! Durable registry observations and exact automatic Deploy identities.

use super::{PersistenceError, Store};
use firecrab_api_types::{
    AutoDeployPause, AutoDeployPolicy, AutoDeployStatus, MicroServiceDeployment, ServicePhase,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS service_tag_watches(service_id TEXT PRIMARY KEY, state TEXT NOT NULL) STRICT";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Pending {
    pub operation: Uuid,
    pub previous: Uuid,
    pub digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Watch {
    pub generation: Uuid,
    #[serde(default)]
    pub authority: Option<super::iac_auto_deploy::Authority>,
    pub status: AutoDeployStatus,
    pub handled: Option<String>,
    pub blocked: Option<String>,
    pub pending: Option<Pending>,
    #[serde(default)]
    pub rollback_operation: Option<Uuid>,
}

impl Watch {
    pub fn new(policy: AutoDeployPolicy) -> Self {
        Self {
            generation: Uuid::new_v4(),
            authority: None,
            status: AutoDeployStatus {
                policy: Some(policy),
                pause: None,
                observed_digest: None,
                observed_tag: None,
                next_check_at: 0,
                operation_id: None,
                failures: 0,
                error: None,
            },
            handled: None,
            blocked: None,
            pending: None,
            rollback_operation: None,
        }
    }

    pub fn pause(&mut self, reason: AutoDeployPause, digest: Option<String>) {
        self.generation = Uuid::new_v4();
        self.status.pause = Some(reason);
        self.blocked = digest;
    }
}

pub(super) fn read(conn: &Connection, id: Uuid) -> Result<Option<Watch>, PersistenceError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT state FROM service_tag_watches WHERE service_id=?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    raw.map(|raw| {
        serde_json::from_str(&raw).map_err(|error| PersistenceError::CorruptRecord {
            id: id.to_string(),
            reason: error.to_string(),
        })
    })
    .transpose()
}

pub(super) fn write(conn: &Connection, id: Uuid, watch: &Watch) -> Result<(), PersistenceError> {
    let mut bound = watch.clone();
    if bound.authority.is_none() {
        let current = super::iac_auto_deploy::authority(conn, id)?;
        if current.has_no_history() {
            bound.authority = Some(current);
        }
    }
    let raw = serde_json::to_string(&bound).map_err(|error| PersistenceError::CorruptRecord {
        id: id.to_string(),
        reason: error.to_string(),
    })?;
    conn.execute("INSERT INTO service_tag_watches VALUES (?1,?2) ON CONFLICT(service_id) DO UPDATE SET state=excluded.state",
        params![id.to_string(), raw])?;
    Ok(())
}

/// Stopped-service edits and new declarations must expose the same policy to runtime controls.
pub(super) fn configuration(
    conn: &Connection,
    service: &firecrab_api_types::MicroServiceResponse,
) -> Result<(), PersistenceError> {
    match &service.spec.auto_deploy {
        Some(policy) => {
            if read(conn, service.id)?
                .is_none_or(|watch| watch.status.policy.as_ref() != Some(policy))
            {
                let mut watch = Watch::new(policy.clone());
                watch.authority = Some(super::iac_auto_deploy::authority(conn, service.id)?);
                write(conn, service.id, &watch)?;
            }
        }
        None => {
            conn.execute(
                "DELETE FROM service_tag_watches WHERE service_id=?1",
                [service.id.to_string()],
            )?;
        }
    }
    Ok(())
}

/// Called inside the deployment transaction, including queued rollback admission.
pub(super) fn deployment(
    conn: &Connection,
    record: &MicroServiceDeployment,
) -> Result<(), PersistenceError> {
    let id = record.service_id;
    let mut watch = read(conn, id)?;
    let complete = record.update.as_ref().is_none_or(|u| u.completed);
    if record.phase == ServicePhase::Running && complete {
        match &record.spec.auto_deploy {
            None => {
                conn.execute(
                    "DELETE FROM service_tag_watches WHERE service_id=?1",
                    [id.to_string()],
                )?;
                return Ok(());
            }
            Some(policy)
                if watch
                    .as_ref()
                    .is_none_or(|w| w.status.policy.as_ref() != Some(policy)) =>
            {
                let mut configured = Watch::new(policy.clone());
                configured.authority = Some(super::iac_auto_deploy::authority(conn, id)?);
                watch = Some(configured);
            }
            _ => {}
        }
    }
    let Some(mut watch) = watch else {
        return Ok(());
    };
    if record.phase == ServicePhase::Queued
        && watch.rollback_operation != Some(record.id)
        && record
            .update
            .as_ref()
            .is_some_and(|u| u.rollback_of.is_some())
    {
        let blocked = watch.status.observed_digest.clone().or_else(|| {
            record
                .update
                .as_ref()
                .and_then(|u| u.previous.digest.clone())
        });
        watch.rollback_operation = Some(record.id);
        watch.pause(AutoDeployPause::Rollback, blocked.clone());
        watch.handled = blocked;
        watch.pending = None;
    } else if watch
        .pending
        .as_ref()
        .is_some_and(|p| p.operation == record.id)
        && !record.phase.is_busy()
        && complete
    {
        let pending = watch.pending.take().expect("matched pending operation");
        watch.handled = Some(pending.digest.clone());
        if record.phase != ServicePhase::Running {
            watch.pause(AutoDeployPause::Failed, Some(pending.digest));
            watch.status.error =
                Some("Automatic Deploy did not complete; inspect its recorded operation".into());
        }
    }
    write(conn, id, &watch)
}

impl Store {
    pub(crate) fn service_is_controller_owned(&self, id: Uuid) -> Result<bool, PersistenceError> {
        Ok(self.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM cluster_service_workers WHERE id=?1)",
            [id.to_string()],
            |r| r.get(0),
        )?)
    }

    pub(crate) fn tag_watch(&self, id: Uuid) -> Result<Option<Watch>, PersistenceError> {
        let conn = self.lock();
        let mut saved = read(&conn, id)?;
        if let Some(watch) = &mut saved
            && watch.authority.is_none()
        {
            let current = super::iac_auto_deploy::authority(&conn, id)?;
            if current.has_no_history() {
                watch.authority = Some(current);
            }
        }
        Ok(saved)
    }
    #[cfg(test)]
    pub(crate) fn save_tag_watch(&self, id: Uuid, watch: &Watch) -> Result<(), PersistenceError> {
        write(&self.lock(), id, watch)
    }

    /// Metadata controls change desired/current policy together, never successful release history.
    pub(crate) fn set_tag_policy(
        &self,
        service: &firecrab_api_types::MicroServiceResponse,
        deployment: Option<&MicroServiceDeployment>,
        watch: Option<&Watch>,
        expected: &super::iac_auto_deploy::Authority,
        expected_watch: Option<Uuid>,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        super::iac_auto_deploy::compare(&tx, service.id, expected)?;
        if read(&tx, service.id)?.map(|w| w.generation) != expected_watch {
            return Err(PersistenceError::StaleIacOwner);
        }
        if super::service_observation::previous(&tx, service.id)?.is_some() != deployment.is_some()
        {
            return Err(PersistenceError::StaleIacOwner);
        }
        super::iac_auto_deploy::validate_owner(
            &tx,
            &firecrab_stack::ManagedResource {
                kind: "service_auto_deploy".into(),
                id: service.id.to_string(),
            },
            false,
        )?;
        for kind in ["service", "service_auto_deploy"] {
            if !super::iac_owners::public_write_allowed(
                &tx,
                &firecrab_stack::ManagedResource {
                    kind: kind.into(),
                    id: service.id.to_string(),
                },
            )? {
                return Err(PersistenceError::IacOwnershipConflict);
            }
        }
        let mut current: firecrab_api_types::MicroServiceSpec =
            serde_json::from_str(&tx.query_row(
                "SELECT spec FROM micro_services WHERE id=?1",
                [service.id.to_string()],
                |row| row.get::<_, String>(0),
            )?)
            .map_err(|_| PersistenceError::IacOperationIdentity)?;
        current.auto_deploy = service.spec.auto_deploy.clone();
        if current != service.spec {
            return Err(PersistenceError::StaleIacOwner);
        }
        tx.execute(
            "UPDATE micro_services SET spec=?2 WHERE id=?1",
            params![
                service.id.to_string(),
                serde_json::to_string(&service.spec).map_err(|error| {
                    PersistenceError::CorruptRecord {
                        id: service.id.to_string(),
                        reason: error.to_string(),
                    }
                })?
            ],
        )?;
        if let Some(deployment) = deployment {
            let mut fresh = super::service_observation::previous(&tx, service.id)?
                .ok_or(PersistenceError::StaleIacOwner)?;
            fresh.spec.auto_deploy = service.spec.auto_deploy.clone();
            if fresh.id != deployment.id || fresh.spec != deployment.spec {
                return Err(PersistenceError::StaleIacOwner);
            }
            let deployment = &fresh;
            let previous = super::service_observation::previous(&tx, service.id)?;
            super::service_observation::observe(&tx, previous.as_ref(), deployment)?;
            let raw = serde_json::to_string(deployment).map_err(|error| {
                PersistenceError::CorruptRecord {
                    id: service.id.to_string(),
                    reason: error.to_string(),
                }
            })?;
            tx.execute(
                "UPDATE micro_service_deployments SET deployment=?2 WHERE service_id=?1",
                params![service.id.to_string(), raw],
            )?;
            tx.execute(
                "UPDATE micro_service_attempts SET deployment=?2 WHERE id=?1",
                params![deployment.id.to_string(), raw],
            )?;
        }
        if let Some(watch) = watch {
            write(&tx, service.id, watch)?;
        } else {
            tx.execute(
                "DELETE FROM service_tag_watches WHERE service_id=?1",
                [service.id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

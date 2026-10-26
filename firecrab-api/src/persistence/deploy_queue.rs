//! Durable admission order. A claimed row prevents duplicate workers from rejoining.

use firecrab_api_types::{MicroServiceDeployment, ServicePhase};
use rusqlite::{OptionalExtension, Transaction, params};
use uuid::Uuid;

use super::{PersistenceError, Store};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS service_deploy_queue (
 ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
 operation_id TEXT NOT NULL UNIQUE,
 service_id TEXT NOT NULL,
 release_id TEXT NOT NULL,
 priority INTEGER NOT NULL,
 admitted INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX IF NOT EXISTS service_deploy_queue_order
 ON service_deploy_queue(admitted, priority, ordinal);
";

/// An operation can disappear while its old worker is waiting for admission.
pub(crate) enum Claim {
    /// Another durable operation has priority.
    Waiting,
    /// Cancelled, replaced, completed, or already claimed by another worker.
    Obsolete,
    /// The exact owner was claimed before any deployment effects.
    Admitted(Box<MicroServiceDeployment>),
}

/// Repairs have their own operation identity while retaining the running release ID.
pub(crate) fn operation(record: &MicroServiceDeployment) -> Uuid {
    if record.update.as_ref().is_none_or(|update| update.completed)
        && let Some(repair) = record
            .recovery
            .as_ref()
            .and_then(|health| health.repair.as_ref())
    {
        return repair.id;
    }
    record.id
}

/// Existing side effects must finish before a new operation spends surge capacity.
fn priority(record: &MicroServiceDeployment) -> u8 {
    if record.phase != ServicePhase::Queued {
        return 0;
    }
    if !cancellable(record) {
        return 1;
    }
    if record
        .update
        .as_ref()
        .is_some_and(|update| update.rollback_of.is_some())
    {
        return 2;
    }
    3
}

/// A retried operation with owned resources is not a cancellable waiting request.
pub(crate) fn cancellable(record: &MicroServiceDeployment) -> bool {
    record.phase == ServicePhase::Queued
        && record.instances.is_empty()
        && !record.domain_route_owned
        && !record.internal_route_owned
        && record
            .recovery
            .as_ref()
            .is_none_or(|health| health.repair.is_none())
        && record.update.as_ref().is_none_or(|update| {
            !update.completed
                && !update.cancelled
                && !update.restoring
                && !update.routed_new
                && update.transition.is_none()
                && update.retiring.is_empty()
        })
}

/// Acceptance and its FIFO ordinal share the deployment snapshot transaction.
pub(super) fn save(
    tx: &Transaction<'_>,
    record: &MicroServiceDeployment,
) -> Result<(), PersistenceError> {
    if record.phase == ServicePhase::Queued {
        enqueue(tx, record, false)?;
    } else if !record.phase.is_busy() {
        tx.execute(
            "DELETE FROM service_deploy_queue WHERE service_id = ?1",
            [record.service_id.to_string()],
        )?;
    }
    Ok(())
}

fn enqueue(
    tx: &Transaction<'_>,
    record: &MicroServiceDeployment,
    restart: bool,
) -> Result<(), PersistenceError> {
    let id = operation(record).to_string();
    let service = record.service_id.to_string();
    let release = record.id.to_string();
    let owner: Option<(String, String)> = tx
        .query_row(
            "SELECT service_id, release_id FROM service_deploy_queue WHERE operation_id = ?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if owner.as_ref().is_some_and(|(old_service, old_release)| {
        old_service != &service || old_release != &release
    }) {
        return Err(PersistenceError::DeployQueueIdentity);
    }
    tx.execute("INSERT OR IGNORE INTO service_deploy_queue(operation_id, service_id, release_id, priority) VALUES (?1, ?2, ?3, ?4)",
        params![id, service, release, priority(record)])?;
    if restart {
        tx.execute(
            "UPDATE service_deploy_queue SET priority = ?2, admitted = 0 WHERE operation_id = ?1",
            params![id, priority(record)],
        )?;
    }
    Ok(())
}

fn decode(text: &str, id: Uuid) -> Result<MicroServiceDeployment, PersistenceError> {
    serde_json::from_str(text).map_err(|error| PersistenceError::CorruptRecord {
        id: id.to_string(),
        reason: error.to_string(),
    })
}

impl Store {
    /// Rebuild missing queue rows; restart also releases the former process's claims.
    pub fn enqueue_service_deployment(
        &self,
        record: &MicroServiceDeployment,
        restart: bool,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let text: Option<String> = tx
            .query_row(
                "SELECT deployment FROM micro_service_deployments WHERE service_id = ?1",
                [record.service_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(text) = text else {
            return Ok(());
        };
        let current = decode(&text, record.service_id)?;
        if current.id != record.id
            || operation(&current) != operation(record)
            || !current.phase.is_busy()
        {
            return Ok(());
        }
        enqueue(&tx, &current, restart)?;
        tx.commit()?;
        Ok(())
    }

    /// Return whether this operation still waits and whether it is first.
    pub fn service_deploy_turn(&self, id: Uuid) -> Result<(bool, bool), PersistenceError> {
        let conn = self.lock();
        let waiting: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM service_deploy_queue WHERE operation_id = ?1 AND admitted = 0)",
            [id.to_string()], |row| row.get(0))?;
        let head: Option<String> = conn.query_row("SELECT operation_id FROM service_deploy_queue WHERE admitted = 0 ORDER BY priority, ordinal LIMIT 1",
            [], |row| row.get(0)).optional()?;
        Ok((waiting, head.as_deref() == Some(id.to_string().as_str())))
    }

    /// Background maintenance yields to all accepted manual work.
    pub fn service_deploy_waiting(&self) -> Result<bool, PersistenceError> {
        self.lock()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM service_deploy_queue WHERE admitted = 0)",
                [],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Atomically select the first operation and make cancellation impossible before effects.
    pub fn claim_service_deployment(
        &self,
        service: Uuid,
        release: Uuid,
        id: Uuid,
    ) -> Result<Claim, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let owner: Option<(String, String)> = tx.query_row(
            "SELECT service_id, release_id FROM service_deploy_queue WHERE operation_id = ?1 AND admitted = 0",
            [id.to_string()], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
        if owner.as_ref() != Some(&(service.to_string(), release.to_string())) {
            return Ok(Claim::Obsolete);
        }
        let head: Option<String> = tx.query_row("SELECT operation_id FROM service_deploy_queue WHERE admitted = 0 ORDER BY priority, ordinal LIMIT 1",
            [], |row| row.get(0)).optional()?;
        if head.as_deref() != Some(id.to_string().as_str()) {
            return Ok(Claim::Waiting);
        }
        let text: Option<String> = tx
            .query_row(
                "SELECT deployment FROM micro_service_deployments WHERE service_id = ?1",
                [service.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let current = text
            .as_deref()
            .map(|text| decode(text, service))
            .transpose()?;
        let Some(mut current) = current.filter(|current| {
            current.id == release && operation(current) == id && current.phase.is_busy()
        }) else {
            tx.execute(
                "DELETE FROM service_deploy_queue WHERE operation_id = ?1",
                [id.to_string()],
            )?;
            tx.commit()?;
            return Ok(Claim::Obsolete);
        };
        if current.phase == ServicePhase::Queued {
            current.phase = ServicePhase::Importing;
            if current.log.len() >= 128 {
                current.log.remove(0);
            }
            current.log.push("Deploy admitted from queue".into());
        }
        let text =
            serde_json::to_string(&current).map_err(|error| PersistenceError::CorruptRecord {
                id: id.to_string(),
                reason: error.to_string(),
            })?;
        tx.execute(
            "UPDATE micro_service_deployments SET deployment = ?2 WHERE service_id = ?1",
            params![service.to_string(), text],
        )?;
        tx.execute(
            "UPDATE micro_service_attempts SET deployment = ?2 WHERE id = ?1",
            params![release.to_string(), text],
        )?;
        tx.execute(
            "UPDATE service_deploy_queue SET admitted = 1 WHERE operation_id = ?1",
            [id.to_string()],
        )?;
        tx.commit()?;
        Ok(Claim::Admitted(Box::new(current)))
    }
}

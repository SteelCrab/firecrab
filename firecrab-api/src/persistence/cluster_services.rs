//! Durable controller services and authenticated worker ownership receipts.
use super::{ClusterError, Store};
use firecrab_api_types::*;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS cluster_services (
 id TEXT PRIMARY KEY, data TEXT NOT NULL, next_attempt_ms INTEGER NOT NULL DEFAULT 0, claim TEXT
) STRICT;
CREATE TABLE IF NOT EXISTS cluster_service_operations (
 id TEXT PRIMARY KEY, service_id TEXT NOT NULL, idempotency_key TEXT NOT NULL UNIQUE, data TEXT NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS cluster_service_workers (id TEXT PRIMARY KEY, data TEXT NOT NULL) STRICT;
CREATE INDEX IF NOT EXISTS cluster_service_due ON cluster_services(next_attempt_ms,id);
CREATE INDEX IF NOT EXISTS cluster_service_operation_owner ON cluster_service_operations(service_id);
";

fn service(conn: &Connection, id: Uuid) -> Result<ClusterService, ClusterError> {
    let json: String = conn
        .query_row(
            "SELECT data FROM cluster_services WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(ClusterError::Missing)?;
    Ok(serde_json::from_str(&json)?)
}

pub(super) fn services(conn: &Connection) -> Result<Vec<ClusterService>, ClusterError> {
    let mut stmt = conn.prepare("SELECT data FROM cluster_services ORDER BY id")?;
    stmt.query_map([], |r| r.get::<_, String>(0))?
        .map(|r| Ok(serde_json::from_str(&r?)?))
        .collect()
}

fn worker(conn: &Connection, id: Uuid) -> Result<Option<ServiceWorkerReceipt>, ClusterError> {
    let json: Option<String> = conn
        .query_row(
            "SELECT data FROM cluster_service_workers WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    json.map(|json| serde_json::from_str(&json).map_err(Into::into))
        .transpose()
}

/// Keep the larger of desired and still-owned releases reserved until cleanup is acknowledged.
pub(super) fn reserved(conn: &Connection, host: Uuid) -> Result<HostCapacity, ClusterError> {
    let mut capacity = HostCapacity::default();
    for service in services(conn)? {
        for h in service.hosts.iter().filter(|h| h.host_id == host) {
            capacity.cpu = capacity.cpu.saturating_add(h.reservation.cpu);
            capacity.ram_mib = capacity.ram_mib.saturating_add(h.reservation.ram_mib);
        }
    }
    Ok(capacity)
}

impl Store {
    pub(crate) fn cluster_services(&self) -> Result<Vec<ClusterService>, ClusterError> {
        services(&self.lock())
    }
    pub(crate) fn cluster_service(&self, id: Uuid) -> Result<ClusterService, ClusterError> {
        service(&self.lock(), id)
    }
    pub(crate) fn cluster_service_operations(
        &self,
        id: Uuid,
    ) -> Result<Vec<ClusterServiceOperation>, ClusterError> {
        let conn = self.lock();
        service(&conn, id)?;
        let mut stmt = conn.prepare("SELECT data FROM cluster_service_operations WHERE service_id=?1 ORDER BY json_extract(data,'$.service.generation')")?;
        stmt.query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }

    /// Retry lookup, generation comparison, resource admission and intent commit are atomic.
    pub(crate) fn cluster_service_admit(
        &self,
        key: &str,
        id: Option<Uuid>,
        action: ClusterServiceAction,
        expected: Option<u64>,
        mut spec: Option<ClusterServiceSpec>,
        now: i64,
    ) -> Result<ClusterService, ClusterError> {
        if key.is_empty() || key.len() > 128 || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(ClusterError::Invalid(
                "Idempotency-Key must contain 1-128 visible ASCII bytes".into(),
            ));
        }
        if let Some(spec) = &mut spec {
            crate::cluster::services::validate_spec(spec)?;
        }
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior: Option<String> = tx
            .query_row(
                "SELECT data FROM cluster_service_operations WHERE idempotency_key=?1",
                [key],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(prior) = prior {
            let op: ClusterServiceOperation = serde_json::from_str(&prior)?;
            if op.action != action
                || op.expected_generation != expected
                || id.is_some_and(|id| id != op.service.id)
                || spec.as_ref().is_some_and(|spec| spec != &op.service.spec)
            {
                return Err(ClusterError::Conflict(
                    "idempotency key names another service request".into(),
                ));
            }
            return service(&tx, op.service.id);
        }
        let mut s = if let Some(id) = id {
            let s = service(&tx, id)?;
            if expected != Some(s.generation) {
                return Err(ClusterError::Conflict("stale service generation".into()));
            }
            s
        } else {
            if action != ClusterServiceAction::Create {
                return Err(ClusterError::Invalid(
                    "creation requires a service specification".into(),
                ));
            }
            ClusterService {
                id: Uuid::new_v4(),
                spec: spec
                    .clone()
                    .ok_or_else(|| ClusterError::Invalid("missing specification".into()))?,
                generation: 0,
                release_generation: 0,
                operation_id: Uuid::nil(),
                desired: ClusterServiceDesired::Running,
                observed: ClusterServiceObserved::Pending,
                pending: true,
                degraded: false,
                hosts: vec![],
            }
        };
        if action != ClusterServiceAction::Stop {
            let spec = spec.ok_or_else(|| ClusterError::Invalid("missing specification".into()))?;
            if !s.hosts.is_empty()
                && (s.spec.service.name != spec.service.name
                    || s.spec
                        .targets
                        .iter()
                        .map(|t| (t.host_id, &t.request))
                        .ne(spec.targets.iter().map(|t| (t.host_id, &t.request))))
            {
                return Err(ClusterError::Conflict(
                    "service name, host set, networks and host ports are immutable".into(),
                ));
            }
            let duplicate_name: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM cluster_services WHERE id<>?1 AND json_extract(data,'$.spec.service.name')=?2)",
                params![s.id.to_string(),spec.service.name], |r| r.get(0))?;
            if duplicate_name {
                return Err(ClusterError::Conflict(
                    "service name already belongs to another identity".into(),
                ));
            }
            let inventory = super::cluster::hosts(&tx, now)?;
            for target in &spec.targets {
                let h = inventory
                    .iter()
                    .find(|h| h.report.id == target.host_id)
                    .ok_or(ClusterError::Missing)?;
                if !h
                    .report
                    .networks
                    .iter()
                    .any(|n| Some(n.id) == target.request.micro_network_id)
                {
                    return Err(ClusterError::Invalid(
                        "network must belong to the selected host".into(),
                    ));
                }
                let old = s.hosts.iter().find(|h| h.host_id == target.host_id);
                let old_cpu = old.map_or(0, |h| h.reservation.cpu);
                let old_ram = old.map_or(0, |h| h.reservation.ram_mib);
                let new_cpu = u32::from(spec.service.cpu) * u32::from(target.replicas);
                let new_ram = u64::from(spec.service.ram) * u64::from(target.replicas);
                if !h.online
                    || h.draining
                    || u64::from(h.report.unmanaged.cpu)
                        + u64::from(h.reserved.cpu)
                        + u64::from(new_cpu.saturating_sub(old_cpu))
                        > u64::from(h.report.capacity.cpu)
                    || h.report
                        .unmanaged
                        .ram_mib
                        .saturating_add(h.reserved.ram_mib)
                        .saturating_add(new_ram.saturating_sub(old_ram))
                        > h.report.capacity.ram_mib
                {
                    return Err(ClusterError::Capacity);
                }
            }
            s.spec = spec;
            s.release_generation = s
                .release_generation
                .checked_add(1)
                .ok_or_else(|| ClusterError::Conflict("release generation exhausted".into()))?;
            s.desired = ClusterServiceDesired::Running;
        } else {
            s.desired = ClusterServiceDesired::Stopped;
        }
        s.generation = s
            .generation
            .checked_add(1)
            .filter(|g| *g <= i64::MAX as u64)
            .ok_or_else(|| ClusterError::Conflict("generation exhausted".into()))?;
        let previous_operation = s.operation_id;
        s.operation_id = Uuid::new_v4();
        if !previous_operation.is_nil() {
            tx.execute("UPDATE cluster_service_operations SET data=json_set(data,'$.supersededBy',?2) WHERE id=?1 AND json_extract(data,'$.service.pending')=1",
                params![previous_operation.to_string(),s.operation_id.to_string()])?;
        }
        s.pending = true;
        s.degraded = false;
        for target in &s.spec.targets {
            let mut local = s.spec.service.clone();
            local.replicas = target.replicas;
            let previous = s
                .hosts
                .iter()
                .find(|h| h.host_id == target.host_id)
                .cloned();
            let intent = ServiceWorkerIntent {
                generation: s.generation,
                release_generation: s.release_generation,
                desired: s.desired,
                deployment_id: if action == ClusterServiceAction::Stop {
                    previous
                        .as_ref()
                        .ok_or(ClusterError::Missing)?
                        .intent
                        .deployment_id
                } else {
                    Uuid::new_v4()
                },
                spec: local,
                request: target.request.clone(),
            };
            let host = ClusterServiceHost {
                host_id: target.host_id,
                role: None,
                reservation: HostCapacity {
                    cpu: previous
                        .as_ref()
                        .map_or(0, |h| h.reservation.cpu)
                        .max(u32::from(intent.spec.cpu) * u32::from(intent.spec.replicas)),
                    ram_mib: previous
                        .as_ref()
                        .map_or(0, |h| h.reservation.ram_mib)
                        .max(u64::from(intent.spec.ram) * u64::from(intent.spec.replicas)),
                },
                intent,
                observed_generation: None,
                deployment: previous.and_then(|h| h.deployment),
                error: None,
            };
            if let Some(old) = s.hosts.iter_mut().find(|h| h.host_id == target.host_id) {
                *old = host;
            } else {
                s.hosts.push(host);
            }
        }
        let op = ClusterServiceOperation {
            id: s.operation_id,
            idempotency_key: key.into(),
            action,
            expected_generation: expected,
            superseded_by: None,
            service: s.clone(),
        };
        tx.execute("INSERT INTO cluster_services(id,data) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET data=excluded.data,next_attempt_ms=0,claim=NULL", params![s.id.to_string(), serde_json::to_string(&s)?])?;
        tx.execute("INSERT INTO cluster_service_operations(id,service_id,idempotency_key,data) VALUES(?1,?2,?3,?4)", params![op.id.to_string(),s.id.to_string(),key,serde_json::to_string(&op)?])?;
        tx.commit()?;
        Ok(s)
    }

    pub(crate) fn cluster_service_due(
        &self,
        now: i64,
        limit: usize,
    ) -> Result<Vec<Uuid>, ClusterError> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT id FROM cluster_services WHERE next_attempt_ms<=?1 ORDER BY next_attempt_ms,id LIMIT ?2")?;
        stmt.query_map(params![now, limit.min(64) as i64], |r| {
            r.get::<_, String>(0)
        })?
        .map(|r| Uuid::parse_str(&r?).map_err(|_| ClusterError::Task))
        .collect()
    }

    pub(crate) fn cluster_service_claim(
        &self,
        id: Uuid,
        now: i64,
        force: bool,
    ) -> Result<Option<(Uuid, ClusterService)>, ClusterError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let s = service(&tx, id)?;
        let (next, claim): (i64, Option<String>) = tx.query_row(
            "SELECT next_attempt_ms,claim FROM cluster_services WHERE id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if now < next && (!force || claim.is_some()) {
            return Ok(None);
        }
        let token = Uuid::new_v4();
        tx.execute(
            "UPDATE cluster_services SET claim=?2,next_attempt_ms=?3 WHERE id=?1",
            params![id.to_string(), token.to_string(), now + 30_000],
        )?;
        tx.commit()?;
        Ok(Some((token, s)))
    }

    /// Compare-and-swap prevents an old response from overwriting a newer release or Stop.
    pub(crate) fn cluster_service_finish(
        &self,
        s: &ClusterService,
        token: Uuid,
        now: i64,
    ) -> Result<(), ClusterError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute("UPDATE cluster_services SET data=?3,claim=NULL,next_attempt_ms=?4 WHERE id=?1 AND claim=?2 AND json_extract(data,'$.generation')=?5",
            params![s.id.to_string(),token.to_string(),serde_json::to_string(s)?,now+10_000,s.generation as i64])?;
        if changed != 0 {
            let json: String = tx.query_row(
                "SELECT data FROM cluster_service_operations WHERE id=?1",
                [s.operation_id.to_string()],
                |r| r.get(0),
            )?;
            let mut op: ClusterServiceOperation = serde_json::from_str(&json)?;
            op.service = s.clone();
            tx.execute(
                "UPDATE cluster_service_operations SET data=?2 WHERE id=?1",
                params![op.id.to_string(), serde_json::to_string(&op)?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// IDs whose CPU/RAM is reserved by the service journal instead of unmanaged usage.
    pub(crate) fn cluster_service_vm_ids(
        &self,
    ) -> Result<std::collections::BTreeSet<Uuid>, ClusterError> {
        let mut ids = std::collections::BTreeSet::new();
        for receipt in self.cluster_service_workers()? {
            if let Some(d) = self.service_deployment(receipt.id)? {
                ids.extend(d.instances.iter().map(|i| i.vm_id));
            }
        }
        Ok(ids)
    }

    pub(crate) fn cluster_service_worker(
        &self,
        id: Uuid,
    ) -> Result<Option<ServiceWorkerReceipt>, ClusterError> {
        worker(&self.lock(), id)
    }
    pub(crate) fn cluster_service_workers(
        &self,
    ) -> Result<Vec<ServiceWorkerReceipt>, ClusterError> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT data FROM cluster_service_workers ORDER BY id")?;
        stmt.query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }

    /// Persist ownership before invoking any host-local service API.
    pub(crate) fn cluster_service_worker_admit(
        &self,
        id: Uuid,
        owner: Uuid,
        intent: ServiceWorkerIntent,
    ) -> Result<ServiceWorkerReceipt, ClusterError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old = worker(&tx, id)?;
        let foreign_deployment: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM micro_service_attempts WHERE id=?1 AND service_id<>?2)
                OR EXISTS(SELECT 1 FROM cluster_service_workers,json_each(data,'$.deploymentIds') WHERE cluster_service_workers.id<>?2 AND json_each.value=?1)",
            params![intent.deployment_id.to_string(),id.to_string()], |r| r.get(0))?;
        if foreign_deployment {
            return Err(ClusterError::Conflict(
                "deployment ID belongs to another service".into(),
            ));
        }
        if let Some(old) = &old {
            if old.controller_id != owner
                || old.intent.spec.name != intent.spec.name
                || intent.generation < old.intent.generation
                || (intent.generation == old.intent.generation && intent != old.intent)
                || intent.release_generation < old.intent.release_generation
            {
                return Err(ClusterError::Conflict(
                    "stale service intent or foreign owner".into(),
                ));
            }
            if intent == old.intent {
                return Ok(old.clone());
            }
        } else if tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM micro_services WHERE id=?1)",
            [id.to_string()],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(ClusterError::Conflict(
                "local service already has another owner".into(),
            ));
        }
        let mut receipt = ServiceWorkerReceipt {
            id,
            controller_id: owner,
            intent,
            deployment_ids: old
                .as_ref()
                .map(|r| r.deployment_ids.clone())
                .unwrap_or_default(),
            deployment: old.and_then(|r| r.deployment),
            error: None,
        };
        if !receipt
            .deployment_ids
            .contains(&receipt.intent.deployment_id)
        {
            receipt.deployment_ids.push(receipt.intent.deployment_id);
        }
        tx.execute("INSERT INTO cluster_service_workers(id,data) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET data=excluded.data",params![id.to_string(),serde_json::to_string(&receipt)?])?;
        tx.commit()?;
        Ok(receipt)
    }

    pub(crate) fn cluster_service_worker_finish(
        &self,
        receipt: &ServiceWorkerReceipt,
    ) -> Result<(), ClusterError> {
        if self.lock().execute("UPDATE cluster_service_workers SET data=?2 WHERE id=?1 AND json_extract(data,'$.intent.generation')=?3 AND json_extract(data,'$.controllerId')=?4",
            params![receipt.id.to_string(),serde_json::to_string(receipt)?,receipt.intent.generation as i64,receipt.controller_id.to_string()])? == 0 {
            return Err(ClusterError::Conflict("worker service generation changed".into()));
        }
        Ok(())
    }
}

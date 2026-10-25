//! Host inventory and placement/worker journals. Admission uses IMMEDIATE transactions.
use super::Store;
use firecrab_api_types::*;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::collections::BTreeMap;
use uuid::Uuid;

pub(crate) const HEARTBEAT_TTL_MS: i64 = 30_000;
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS cluster_hosts (
 id TEXT PRIMARY KEY, report TEXT NOT NULL, last_seen_ms INTEGER NOT NULL,
 draining INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE TABLE IF NOT EXISTS cluster_placements (
 id TEXT PRIMARY KEY, idempotency_key TEXT NOT NULL UNIQUE,
 data TEXT NOT NULL, next_attempt_ms INTEGER NOT NULL DEFAULT 0, claim TEXT
) STRICT;
CREATE TABLE IF NOT EXISTS cluster_worker_receipts (
 id TEXT PRIMARY KEY, data TEXT NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS cluster_placement_due ON cluster_placements(next_attempt_ms,id);
CREATE TABLE IF NOT EXISTS cluster_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
";

#[derive(Debug, thiserror::Error)]
pub(crate) enum ClusterError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error("resource does not exist")]
    Missing,
    #[error("no healthy, non-draining host satisfies image, labels, resources and capacity")]
    Capacity,
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Persistence(#[from] super::PersistenceError),
    #[error("cluster background task failed")]
    Task,
}

fn placements(conn: &Connection) -> Result<Vec<VmPlacement>, ClusterError> {
    let mut stmt = conn.prepare("SELECT data FROM cluster_placements ORDER BY id")?;
    stmt.query_map([], |row| row.get::<_, String>(0))?
        .map(|v| Ok(serde_json::from_str(&v?)?))
        .collect()
}

fn hosts(conn: &Connection, now: i64) -> Result<Vec<ClusterHost>, ClusterError> {
    let placements = placements(conn)?;
    let mut stmt =
        conn.prepare("SELECT report,last_seen_ms,draining FROM cluster_hosts ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, bool>(2)?,
        ))
    })?;
    let mut result = Vec::new();
    for row in rows {
        let (json, last_seen_ms, draining) = row?;
        let report: HostReport = serde_json::from_str(&json)?;
        let mut reserved = HostCapacity::default();
        let mut disks = BTreeMap::<String, u64>::new();
        for p in placements
            .iter()
            .filter(|p| p.vm.host_id == report.id && p.phase.reserves_capacity())
        {
            reserved.cpu = reserved.cpu.saturating_add(u32::from(p.spec.cpu));
            reserved.ram_mib = reserved.ram_mib.saturating_add(u64::from(p.spec.ram));
            *disks
                .entry(p.target.storage.resource_id.clone())
                .or_default() += u64::from(p.spec.disk_gb);
        }
        result.push(ClusterHost {
            report,
            last_seen_ms,
            draining,
            online: now.saturating_sub(last_seen_ms) <= HEARTBEAT_TTL_MS,
            reserved,
            reserved_disk_gib: disks,
        });
    }
    Ok(result)
}

fn placement(conn: &Connection, id: Uuid) -> Result<VmPlacement, ClusterError> {
    let json: String = conn
        .query_row(
            "SELECT data FROM cluster_placements WHERE id=?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(ClusterError::Missing)?;
    Ok(serde_json::from_str(&json)?)
}

fn worker(conn: &Connection, id: Uuid) -> Result<Option<WorkerReceipt>, ClusterError> {
    let json: Option<String> = conn
        .query_row(
            "SELECT data FROM cluster_worker_receipts WHERE id=?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    json.map(|v| serde_json::from_str(&v).map_err(Into::into))
        .transpose()
}

impl Store {
    /// Fetch due owners rather than repeatedly polling the first IDs and starving later VMs.
    pub(crate) fn cluster_due(&self, now: i64, limit: usize) -> Result<Vec<Uuid>, ClusterError> {
        let conn = self.lock();
        let mut statement = conn.prepare("SELECT id FROM cluster_placements WHERE next_attempt_ms <= ?1
            AND json_extract(data,'$.phase') IN ('pending','uncertain','confirmed') ORDER BY next_attempt_ms,id LIMIT ?2")?;
        statement
            .query_map(params![now, limit.min(64) as i64], |row| {
                row.get::<_, String>(0)
            })?
            .map(|id| {
                Uuid::parse_str(&id?)
                    .map_err(|_| ClusterError::Invalid("corrupt placement UUID".into()))
            })
            .collect()
    }

    /// Reconciliation needs the recorded endpoint, not a recomputed capacity snapshot.
    pub(crate) fn cluster_endpoint(&self, id: Uuid) -> Result<String, ClusterError> {
        self.lock()
            .query_row(
                "SELECT json_extract(report,'$.endpoint') FROM cluster_hosts WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(ClusterError::Missing)
    }
    /// Re-registration updates only inventory and receipt time, never operator drain state.
    pub(crate) fn cluster_register(
        &self,
        report: &HostReport,
        now: i64,
    ) -> Result<(), ClusterError> {
        self.lock().execute("INSERT INTO cluster_hosts(id,report,last_seen_ms) VALUES(?1,?2,?3)
            ON CONFLICT(id) DO UPDATE SET report=excluded.report,last_seen_ms=excluded.last_seen_ms",
            params![report.id.to_string(),serde_json::to_string(report)?,now])?;
        Ok(())
    }

    pub(crate) fn cluster_hosts(&self, now: i64) -> Result<Vec<ClusterHost>, ClusterError> {
        hosts(&self.lock(), now)
    }
    pub(crate) fn cluster_placements(&self) -> Result<Vec<VmPlacement>, ClusterError> {
        placements(&self.lock())
    }
    pub(crate) fn cluster_placement(&self, id: Uuid) -> Result<VmPlacement, ClusterError> {
        placement(&self.lock(), id)
    }

    /// Drain and admission serialize against the same database transaction.
    pub(crate) fn cluster_drain(&self, id: Uuid, draining: bool) -> Result<(), ClusterError> {
        if self.lock().execute(
            "UPDATE cluster_hosts SET draining=?2 WHERE id=?1",
            params![id.to_string(), draining],
        )? == 0
        {
            return Err(ClusterError::Missing);
        }
        Ok(())
    }

    /// A single transaction chooses one owner and commits its reservation and retry key.
    pub(crate) fn cluster_admit(
        &self,
        key: &str,
        mut spec: PlacementSpec,
        now: i64,
    ) -> Result<VmPlacement, ClusterError> {
        crate::cluster::validate_spec(&mut spec)?;
        if key.is_empty() || key.len() > 128 || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(ClusterError::Invalid(
                "Idempotency-Key must contain 1-128 visible ASCII bytes".into(),
            ));
        }
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old: Option<String> = tx
            .query_row(
                "SELECT data FROM cluster_placements WHERE idempotency_key=?1",
                [key],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(old) = old {
            let old: VmPlacement = serde_json::from_str(&old)?;
            if old.spec != spec {
                return Err(ClusterError::Conflict(
                    "idempotency key already names a different request".into(),
                ));
            }
            return Ok(old);
        }
        let inventory = hosts(&tx, now)?;
        let mut candidates = Vec::new();
        for target in &spec.targets {
            let host = inventory
                .iter()
                .find(|h| h.report.id == target.network.host_id)
                .ok_or(ClusterError::Missing)?;
            let report = &host.report;
            if target.storage.host_id != report.id
                || !report
                    .networks
                    .iter()
                    .any(|n| n.id.to_string() == target.network.resource_id)
            {
                return Err(ClusterError::Invalid(
                    "network and storage must belong to their candidate host".into(),
                ));
            }
            let storage = report
                .storages
                .iter()
                .find(|s| s.id == target.storage.resource_id)
                .ok_or_else(|| ClusterError::Invalid("unknown host-local storage".into()))?;
            let used_cpu = u64::from(report.unmanaged.cpu) + u64::from(host.reserved.cpu);
            let used_ram = report
                .unmanaged
                .ram_mib
                .saturating_add(host.reserved.ram_mib);
            let used_disk = storage
                .unmanaged_gib
                .saturating_add(*host.reserved_disk_gib.get(&storage.id).unwrap_or(&0));
            if !host.online
                || host.draining
                || !report.images.contains(&spec.image)
                || !spec
                    .required_labels
                    .iter()
                    .all(|(k, v)| report.labels.get(k) == Some(v))
                || used_cpu + u64::from(spec.cpu) > u64::from(report.capacity.cpu)
                || used_ram.saturating_add(u64::from(spec.ram)) > report.capacity.ram_mib
                || used_disk.saturating_add(u64::from(spec.disk_gb)) > storage.capacity_gib
            {
                continue;
            }
            // Least reserved memory, then CPU, with UUID as a stable tie breaker.
            let score = (
                used_ram * 1_000_000 / report.capacity.ram_mib.max(1),
                used_cpu * 1_000_000 / u64::from(report.capacity.cpu.max(1)),
                report.id,
            );
            candidates.push((score, target.clone()));
        }
        candidates.sort_by_key(|(score, _)| *score);
        let target = candidates
            .into_iter()
            .next()
            .ok_or(ClusterError::Capacity)?
            .1;
        let id = Uuid::new_v4();
        let p = VmPlacement {
            id,
            idempotency_key: key.into(),
            vm: HostResourceRef {
                host_id: target.network.host_id,
                resource_id: id.to_string(),
            },
            target,
            spec,
            generation: 0,
            action: PlacementAction::Create,
            phase: PlacementPhase::Pending,
            observed: None,
            error: None,
            created_at_ms: now,
        };
        tx.execute(
            "INSERT INTO cluster_placements(id,idempotency_key,data) VALUES(?1,?2,?3)",
            params![id.to_string(), key, serde_json::to_string(&p)?],
        )?;
        tx.commit()?;
        Ok(p)
    }

    /// The immutable owner survives every later lifecycle operation.
    pub(crate) fn cluster_action(
        &self,
        id: Uuid,
        action: PlacementAction,
    ) -> Result<VmPlacement, ClusterError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut p = placement(&tx, id)?;
        let satisfied = match action {
            PlacementAction::Start => p
                .observed
                .as_ref()
                .is_some_and(|v| matches!(v.state, VmState::Starting | VmState::Running)),
            PlacementAction::Stop => p
                .observed
                .as_ref()
                .is_some_and(|v| matches!(v.state, VmState::Created | VmState::Stopped)),
            PlacementAction::Delete => p.phase == PlacementPhase::Deleted,
            PlacementAction::Create => true,
        };
        if p.action == action
            && (p.phase != PlacementPhase::Confirmed || (p.error.is_none() && satisfied))
        {
            return Ok(p);
        }
        if p.phase == PlacementPhase::Deleted || action == PlacementAction::Create {
            return Err(ClusterError::Conflict(
                "placement is deleted or action is invalid".into(),
            ));
        }
        if action != PlacementAction::Delete && p.phase != PlacementPhase::Confirmed {
            return Err(ClusterError::Conflict(
                "reconcile the unresolved operation before starting another".into(),
            ));
        }
        p.generation = p
            .generation
            .checked_add(1)
            .filter(|v| *v <= i64::MAX as u64)
            .ok_or_else(|| ClusterError::Conflict("generation exhausted".into()))?;
        p.action = action;
        p.phase = PlacementPhase::Pending;
        p.error = None;
        tx.execute(
            "UPDATE cluster_placements SET data=?2,next_attempt_ms=0,claim=NULL WHERE id=?1",
            params![id.to_string(), serde_json::to_string(&p)?],
        )?;
        tx.commit()?;
        Ok(p)
    }

    /// A claim fences late responses; uncertain ownership remains reserved during retries.
    pub(crate) fn cluster_claim(
        &self,
        id: Uuid,
        now: i64,
        force: bool,
    ) -> Result<Option<(Uuid, VmPlacement)>, ClusterError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut p = placement(&tx, id)?;
        if !p.phase.reserves_capacity() {
            return Ok(None);
        }
        let (next, claim): (i64, Option<String>) = tx.query_row(
            "SELECT next_attempt_ms,claim FROM cluster_placements WHERE id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if next > now && (!force || claim.is_some()) {
            return Ok(None);
        }
        let token = Uuid::new_v4();
        if p.phase == PlacementPhase::Pending {
            p.phase = PlacementPhase::Uncertain;
        }
        tx.execute(
            "UPDATE cluster_placements SET data=?2,next_attempt_ms=?3,claim=?4 WHERE id=?1",
            params![
                id.to_string(),
                serde_json::to_string(&p)?,
                now + 30_000,
                token.to_string()
            ],
        )?;
        tx.commit()?;
        Ok(Some((token, p)))
    }

    pub(crate) fn cluster_finish(
        &self,
        p: &VmPlacement,
        token: Uuid,
        now: i64,
    ) -> Result<(), ClusterError> {
        self.lock().execute("UPDATE cluster_placements SET data=?3,next_attempt_ms=?4,claim=NULL WHERE id=?1 AND claim=?2",
            params![p.id.to_string(),token.to_string(),serde_json::to_string(p)?,now+10_000])?;
        Ok(())
    }

    pub(crate) fn cluster_worker(&self, id: Uuid) -> Result<Option<WorkerReceipt>, ClusterError> {
        worker(&self.lock(), id)
    }
    pub(crate) fn cluster_workers(&self) -> Result<Vec<WorkerReceipt>, ClusterError> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT data FROM cluster_worker_receipts ORDER BY id")?;
        stmt.query_map([], |r| r.get::<_, String>(0))?
            .map(|v| Ok(serde_json::from_str(&v?)?))
            .collect()
    }

    /// Called under the per-placement worker lock, before touching local VM resources.
    pub(crate) fn cluster_worker_admit(
        &self,
        id: Uuid,
        controller_id: Uuid,
        intent: WorkerIntent,
    ) -> Result<WorkerReceipt, ClusterError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(old) = worker(&tx, id)? {
            if old.controller_id != controller_id
                || old.intent.spec != intent.spec
                || old.intent.target != intent.target
            {
                return Err(ClusterError::Conflict(
                    "worker placement owner and configuration are immutable".into(),
                ));
            }
            if intent.generation < old.intent.generation
                || (intent.generation == old.intent.generation
                    && intent.action != old.intent.action)
            {
                return Err(ClusterError::Conflict(
                    "stale or conflicting operation generation".into(),
                ));
            }
            if intent.generation == old.intent.generation {
                return Ok(old);
            }
            if old.phase == PlacementPhase::Deleted || intent.action == PlacementAction::Create {
                return Err(ClusterError::Conflict(
                    "cannot resurrect a deleted worker placement".into(),
                ));
            }
        } else if tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM vms WHERE id=?1)",
            [id.to_string()],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(ClusterError::Conflict(
                "VM ID is already owned outside this placement".into(),
            ));
        }
        let receipt = WorkerReceipt {
            id,
            controller_id,
            intent,
            phase: PlacementPhase::Pending,
            observed: None,
            error: None,
        };
        tx.execute("INSERT INTO cluster_worker_receipts(id,data) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET data=excluded.data",
            params![id.to_string(),serde_json::to_string(&receipt)?])?;
        tx.commit()?;
        Ok(receipt)
    }

    pub(crate) fn cluster_worker_finish(&self, r: &WorkerReceipt) -> Result<(), ClusterError> {
        let generation = i64::try_from(r.intent.generation)
            .map_err(|_| ClusterError::Invalid("worker generation exceeds journal limit".into()))?;
        if self.lock().execute(
            "UPDATE cluster_worker_receipts SET data=?2 WHERE id=?1
                AND json_extract(data,'$.intent.generation')=?3 AND json_extract(data,'$.controllerId')=?4",
            params![r.id.to_string(), serde_json::to_string(r)?,generation,r.controller_id.to_string()],
        )? == 0 { return Err(ClusterError::Conflict("worker generation changed before acknowledgement".into())); }
        Ok(())
    }

    /// Enrollment captures a stable disk quota; VM disk writes must not shrink it twice.
    pub(crate) fn cluster_set_disk_budget(&self, id: &str, limit: u64) -> Result<(), ClusterError> {
        self.lock().execute("INSERT INTO cluster_settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![format!("disk:{id}"),limit.to_string()])?;
        Ok(())
    }

    /// Preserve the initial or explicitly configured budget across heartbeat observations.
    pub(crate) fn cluster_disk_budget(
        &self,
        id: &str,
        available: u64,
    ) -> Result<u64, ClusterError> {
        let conn = self.lock();
        let key = format!("disk:{id}");
        conn.execute(
            "INSERT OR IGNORE INTO cluster_settings(key,value) VALUES(?1,?2)",
            params![key, available.to_string()],
        )?;
        let value: String = conn.query_row(
            "SELECT value FROM cluster_settings WHERE key=?1",
            [key],
            |r| r.get(0),
        )?;
        value
            .parse()
            .map_err(|_| ClusterError::Invalid("invalid stored disk budget".into()))
    }
}

//! Warm pool rows (issue #291): pools, their member VMs, and leases, the
//! lease rows doubling as the idempotency record for `acquire`.
//!
//! Every state change that matters for "never hand one VM to two callers"
//! happens inside one `IMMEDIATE` transaction, and a partial unique index on
//! active leases backs it at the schema level.

use firecrab_api_types::{EgressPolicy, PoolLeaseState, PoolMemberState};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use thiserror::Error;
use uuid::Uuid;

use super::Store;

const CREATE_POOLS_SQL: &str = "CREATE TABLE IF NOT EXISTS vm_pools (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    template TEXT NOT NULL,
    template_version TEXT NOT NULL,
    cpu INTEGER NOT NULL,
    ram INTEGER NOT NULL,
    disk_gb INTEGER NOT NULL,
    egress_policy TEXT NOT NULL,
    micro_network_id TEXT NOT NULL,
    storage_root TEXT NOT NULL,
    min_ready INTEGER NOT NULL,
    max_size INTEGER NOT NULL,
    lease_ttl_seconds INTEGER NOT NULL,
    deleting INTEGER NOT NULL DEFAULT 0,
    created_at_ms INTEGER NOT NULL
)";

const CREATE_MEMBERS_SQL: &str = "CREATE TABLE IF NOT EXISTS vm_pool_members (
    vm_id TEXT PRIMARY KEY,
    pool_id TEXT NOT NULL,
    state TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
)";

const CREATE_LEASES_SQL: &str = "CREATE TABLE IF NOT EXISTS vm_pool_leases (
    id TEXT PRIMARY KEY,
    pool_id TEXT NOT NULL,
    vm_id TEXT NOT NULL,
    state TEXT NOT NULL,
    idempotency_key TEXT,
    acquired_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    ended_at_ms INTEGER
)";

/// A retried acquire finds its lease by key, per pool.
const CREATE_LEASE_KEY_INDEX_SQL: &str = "CREATE UNIQUE INDEX IF NOT EXISTS \
    vm_pool_leases_idempotency ON vm_pool_leases(pool_id, idempotency_key) \
    WHERE idempotency_key IS NOT NULL";

/// One VM can be under at most one active lease, whatever the code above does.
const CREATE_ACTIVE_LEASE_INDEX_SQL: &str = "CREATE UNIQUE INDEX IF NOT EXISTS \
    vm_pool_leases_active_vm ON vm_pool_leases(vm_id) WHERE state = 'active'";

const POOL_COLUMNS: &str = "id, name, template, template_version, cpu, ram, disk_gb, \
    egress_policy, micro_network_id, storage_root, min_ready, max_size, lease_ttl_seconds, \
    deleting, created_at_ms";

const LEASE_COLUMNS: &str =
    "id, pool_id, vm_id, state, idempotency_key, acquired_at_ms, expires_at_ms, ended_at_ms";

/// Creates the pool tables. Called from [`Store::open`].
pub(super) fn create_tables(conn: &Connection) -> rusqlite::Result<()> {
    for sql in [
        CREATE_POOLS_SQL,
        CREATE_MEMBERS_SQL,
        CREATE_LEASES_SQL,
        CREATE_LEASE_KEY_INDEX_SQL,
        CREATE_ACTIVE_LEASE_INDEX_SQL,
    ] {
        conn.execute(sql, [])?;
    }
    Ok(())
}

/// Failures specific to pool rows.
#[derive(Debug, Error)]
pub(crate) enum PoolStoreError {
    #[error("a pool named {0:?} already exists")]
    DuplicateName(String),
    #[error("pool {0} does not exist")]
    MissingPool(Uuid),
    #[error("pool {0} is being deleted")]
    Deleting(Uuid),
    #[error("pool {0} has no ready member")]
    Exhausted(Uuid),
    #[error("pool {id} still has {count} active lease(s)")]
    InUse { id: Uuid, count: u32 },
    #[error("pool record {id} is invalid: {reason}")]
    Corrupt { id: String, reason: String },
    #[error("pool database operation failed: {0}")]
    Database(#[from] rusqlite::Error),
}

/// A pool's durable configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PoolRow {
    pub id: Uuid,
    pub name: String,
    pub template: String,
    pub template_version: String,
    pub cpu: u8,
    pub ram: u32,
    pub disk_gb: u16,
    pub egress_policy: EgressPolicy,
    pub micro_network_id: Uuid,
    pub storage_root: String,
    pub min_ready: u16,
    pub max_size: u16,
    pub lease_ttl_seconds: u32,
    pub deleting: bool,
    pub created_at_ms: u64,
}

/// One VM owned by a pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MemberRow {
    pub vm_id: Uuid,
    pub pool_id: Uuid,
    pub state: PoolMemberState,
    pub created_at_ms: u64,
}

/// One lease, active or ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LeaseRow {
    pub id: Uuid,
    pub pool_id: Uuid,
    pub vm_id: Uuid,
    pub state: PoolLeaseState,
    pub idempotency_key: Option<String>,
    pub acquired_at_ms: u64,
    pub expires_at_ms: u64,
    pub ended_at_ms: Option<u64>,
}

/// Result of [`Store::acquire_pool_lease`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Acquired {
    pub lease: LeaseRow,
    /// `true` when the idempotency key matched an earlier acquire.
    pub replayed: bool,
}

impl Store {
    pub(crate) fn insert_pool(&self, pool: &PoolRow) -> Result<(), PoolStoreError> {
        let result = self.lock().execute(
            &format!(
                "INSERT INTO vm_pools ({POOL_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)"
            ),
            params![
                pool.id.to_string(),
                pool.name,
                pool.template,
                pool.template_version,
                pool.cpu,
                pool.ram,
                pool.disk_gb,
                pool.egress_policy.id(),
                pool.micro_network_id.to_string(),
                pool.storage_root,
                pool.min_ready,
                pool.max_size,
                pool.lease_ttl_seconds,
                pool.deleting,
                pool.created_at_ms as i64,
            ],
        );
        match result {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(PoolStoreError::DuplicateName(pool.name.clone()))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn list_pools(&self) -> Result<Vec<PoolRow>, PoolStoreError> {
        let conn = self.lock();
        let mut statement = conn.prepare(&format!(
            "SELECT {POOL_COLUMNS} FROM vm_pools ORDER BY name"
        ))?;
        let rows = statement.query_map([], pool_row)?;
        rows.map(|row| row?).collect()
    }

    pub(crate) fn pool(&self, id: Uuid) -> Result<Option<PoolRow>, PoolStoreError> {
        let conn = self.lock();
        select_pool(&conn, id)
    }

    /// Replaces the pool's sizing and TTL. `false` when the pool is gone.
    pub(crate) fn update_pool_limits(
        &self,
        id: Uuid,
        min_ready: u16,
        max_size: u16,
        lease_ttl_seconds: u32,
    ) -> Result<bool, PoolStoreError> {
        let changed = self.lock().execute(
            "UPDATE vm_pools SET min_ready = ?2, max_size = ?3, lease_ttl_seconds = ?4 \
             WHERE id = ?1",
            params![id.to_string(), min_ready, max_size, lease_ttl_seconds],
        )?;
        Ok(changed > 0)
    }

    /// Flags the pool for deletion, refused while any lease is active so a
    /// caller never loses a VM it holds.
    pub(crate) fn mark_pool_deleting(&self, id: Uuid) -> Result<PoolRow, PoolStoreError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut pool = select_pool(&tx, id)?.ok_or(PoolStoreError::MissingPool(id))?;
        let active: u32 = tx.query_row(
            "SELECT COUNT(*) FROM vm_pool_leases WHERE pool_id = ?1 AND state = 'active'",
            params![id.to_string()],
            |row| row.get(0),
        )?;
        if active > 0 {
            return Err(PoolStoreError::InUse { id, count: active });
        }
        tx.execute(
            "UPDATE vm_pools SET deleting = 1 WHERE id = ?1",
            params![id.to_string()],
        )?;
        tx.commit()?;
        pool.deleting = true;
        Ok(pool)
    }

    /// Removes a pool and its lease history. Only called once every member
    /// VM is gone.
    pub(crate) fn delete_pool(&self, id: Uuid) -> Result<(), PoolStoreError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM vm_pool_leases WHERE pool_id = ?1",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM vm_pools WHERE id = ?1",
            params![id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn pool_members(&self, pool_id: Uuid) -> Result<Vec<MemberRow>, PoolStoreError> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT vm_id, pool_id, state, created_at_ms FROM vm_pool_members \
             WHERE pool_id = ?1 ORDER BY created_at_ms, vm_id",
        )?;
        let rows = statement.query_map(params![pool_id.to_string()], member_row)?;
        rows.map(|row| row?).collect()
    }

    pub(crate) fn insert_pool_member(&self, member: &MemberRow) -> Result<(), PoolStoreError> {
        self.lock().execute(
            "INSERT INTO vm_pool_members (vm_id, pool_id, state, created_at_ms) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                member.vm_id.to_string(),
                member.pool_id.to_string(),
                encode_member_state(member.state),
                member.created_at_ms as i64,
            ],
        )?;
        Ok(())
    }

    /// Moves a member to `to` only from one of `from`. `false` when the
    /// member is gone or was in another state.
    pub(crate) fn set_pool_member_state(
        &self,
        vm_id: Uuid,
        from: &[PoolMemberState],
        to: PoolMemberState,
    ) -> Result<bool, PoolStoreError> {
        let conn = self.lock();
        let current: Option<String> = conn
            .query_row(
                "SELECT state FROM vm_pool_members WHERE vm_id = ?1",
                params![vm_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(current) = current else {
            return Ok(false);
        };
        if !from.contains(&decode_member_state(&vm_id.to_string(), &current)?) {
            return Ok(false);
        }
        conn.execute(
            "UPDATE vm_pool_members SET state = ?2 WHERE vm_id = ?1",
            params![vm_id.to_string(), encode_member_state(to)],
        )?;
        Ok(true)
    }

    pub(crate) fn delete_pool_member(&self, vm_id: Uuid) -> Result<(), PoolStoreError> {
        self.lock().execute(
            "DELETE FROM vm_pool_members WHERE vm_id = ?1",
            params![vm_id.to_string()],
        )?;
        Ok(())
    }

    /// Leases the oldest ready member for which `leasable` holds, or returns
    /// the lease an earlier call with the same `idempotency_key` created.
    /// Picking, marking, and recording happen in one transaction, so two
    /// concurrent calls can never lease the same VM.
    pub(crate) fn acquire_pool_lease(
        &self,
        pool_id: Uuid,
        idempotency_key: Option<&str>,
        now_ms: u64,
        leasable: impl Fn(Uuid) -> bool,
    ) -> Result<Acquired, PoolStoreError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(key) = idempotency_key {
            let existing = tx
                .query_row(
                    &format!(
                        "SELECT {LEASE_COLUMNS} FROM vm_pool_leases \
                         WHERE pool_id = ?1 AND idempotency_key = ?2"
                    ),
                    params![pool_id.to_string(), key],
                    lease_row,
                )
                .optional()?;
            if let Some(lease) = existing {
                return Ok(Acquired {
                    lease: lease?,
                    replayed: true,
                });
            }
        }
        let pool = select_pool(&tx, pool_id)?.ok_or(PoolStoreError::MissingPool(pool_id))?;
        if pool.deleting {
            return Err(PoolStoreError::Deleting(pool_id));
        }
        let ready: Vec<String> = {
            let mut statement = tx.prepare(
                "SELECT vm_id FROM vm_pool_members WHERE pool_id = ?1 AND state = 'ready' \
                 ORDER BY created_at_ms, vm_id",
            )?;
            let rows = statement.query_map(params![pool_id.to_string()], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let vm_id = ready
            .iter()
            .map(|id| decode_id(id, id))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .find(|&id| leasable(id))
            .ok_or(PoolStoreError::Exhausted(pool_id))?;

        let lease = LeaseRow {
            id: Uuid::new_v4(),
            pool_id,
            vm_id,
            state: PoolLeaseState::Active,
            idempotency_key: idempotency_key.map(str::to_owned),
            acquired_at_ms: now_ms,
            expires_at_ms: now_ms + u64::from(pool.lease_ttl_seconds) * 1000,
            ended_at_ms: None,
        };
        tx.execute(
            "UPDATE vm_pool_members SET state = 'leased' WHERE vm_id = ?1",
            params![vm_id.to_string()],
        )?;
        tx.execute(
            &format!(
                "INSERT INTO vm_pool_leases ({LEASE_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)"
            ),
            params![
                lease.id.to_string(),
                pool_id.to_string(),
                vm_id.to_string(),
                encode_lease_state(lease.state),
                lease.idempotency_key,
                lease.acquired_at_ms as i64,
                lease.expires_at_ms as i64,
            ],
        )?;
        tx.commit()?;
        Ok(Acquired {
            lease,
            replayed: false,
        })
    }

    pub(crate) fn pool_lease(&self, id: Uuid) -> Result<Option<LeaseRow>, PoolStoreError> {
        let conn = self.lock();
        select_lease(&conn, id)
    }

    /// Every lease of the pool, newest first.
    pub(crate) fn pool_leases(&self, pool_id: Uuid) -> Result<Vec<LeaseRow>, PoolStoreError> {
        let conn = self.lock();
        let mut statement = conn.prepare(&format!(
            "SELECT {LEASE_COLUMNS} FROM vm_pool_leases WHERE pool_id = ?1 \
             ORDER BY acquired_at_ms DESC, id"
        ))?;
        let rows = statement.query_map(params![pool_id.to_string()], lease_row)?;
        rows.map(|row| row?).collect()
    }

    /// Active leases whose TTL has run out at `now_ms`.
    pub(crate) fn expired_pool_leases(&self, now_ms: u64) -> Result<Vec<LeaseRow>, PoolStoreError> {
        let conn = self.lock();
        let mut statement = conn.prepare(&format!(
            "SELECT {LEASE_COLUMNS} FROM vm_pool_leases \
             WHERE state = 'active' AND expires_at_ms <= ?1"
        ))?;
        let rows = statement.query_map(params![now_ms as i64], lease_row)?;
        rows.map(|row| row?).collect()
    }

    /// The active lease on `vm_id`, if any.
    pub(crate) fn active_pool_lease_for_vm(
        &self,
        vm_id: Uuid,
    ) -> Result<Option<LeaseRow>, PoolStoreError> {
        let conn = self.lock();
        conn.query_row(
            &format!(
                "SELECT {LEASE_COLUMNS} FROM vm_pool_leases \
                 WHERE vm_id = ?1 AND state = 'active'"
            ),
            params![vm_id.to_string()],
            lease_row,
        )
        .optional()?
        .transpose()
    }

    /// Ends an active lease as `state` and sends its member to draining in
    /// one transaction. An already-ended lease is returned unchanged, so a
    /// retried release is harmless. `None` when the lease does not exist.
    pub(crate) fn end_pool_lease(
        &self,
        id: Uuid,
        state: PoolLeaseState,
        now_ms: u64,
    ) -> Result<Option<LeaseRow>, PoolStoreError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(mut lease) = select_lease(&tx, id)? else {
            return Ok(None);
        };
        if lease.state != PoolLeaseState::Active {
            return Ok(Some(lease));
        }
        tx.execute(
            "UPDATE vm_pool_leases SET state = ?2, ended_at_ms = ?3 WHERE id = ?1",
            params![id.to_string(), encode_lease_state(state), now_ms as i64],
        )?;
        tx.execute(
            "UPDATE vm_pool_members SET state = 'draining' WHERE vm_id = ?1",
            params![lease.vm_id.to_string()],
        )?;
        tx.commit()?;
        lease.state = state;
        lease.ended_at_ms = Some(now_ms);
        Ok(Some(lease))
    }

    /// Forgets leases that ended before `before_ms`, which also ends their
    /// idempotency window.
    pub(crate) fn prune_ended_pool_leases(&self, before_ms: u64) -> Result<usize, PoolStoreError> {
        Ok(self.lock().execute(
            "DELETE FROM vm_pool_leases WHERE state != 'active' AND ended_at_ms < ?1",
            params![before_ms as i64],
        )?)
    }
}

fn select_pool(conn: &Connection, id: Uuid) -> Result<Option<PoolRow>, PoolStoreError> {
    conn.query_row(
        &format!("SELECT {POOL_COLUMNS} FROM vm_pools WHERE id = ?1"),
        params![id.to_string()],
        pool_row,
    )
    .optional()?
    .transpose()
}

fn select_lease(conn: &Connection, id: Uuid) -> Result<Option<LeaseRow>, PoolStoreError> {
    conn.query_row(
        &format!("SELECT {LEASE_COLUMNS} FROM vm_pool_leases WHERE id = ?1"),
        params![id.to_string()],
        lease_row,
    )
    .optional()?
    .transpose()
}

/// Decodes one `vm_pools` row. The outer `Result` is SQLite's, the inner one
/// a value this schema does not allow.
fn pool_row(row: &Row<'_>) -> rusqlite::Result<Result<PoolRow, PoolStoreError>> {
    let id: String = row.get(0)?;
    let egress: String = row.get(7)?;
    let network: String = row.get(8)?;
    Ok((|| {
        Ok(PoolRow {
            id: decode_id(&id, &id)?,
            name: row.get(1)?,
            template: row.get(2)?,
            template_version: row.get(3)?,
            cpu: row.get(4)?,
            ram: row.get(5)?,
            disk_gb: row.get(6)?,
            egress_policy: egress.parse().map_err(|_| PoolStoreError::Corrupt {
                id: id.clone(),
                reason: format!("unknown egress policy {egress:?}"),
            })?,
            micro_network_id: decode_id(&id, &network)?,
            storage_root: row.get(9)?,
            min_ready: row.get(10)?,
            max_size: row.get(11)?,
            lease_ttl_seconds: row.get(12)?,
            deleting: row.get(13)?,
            created_at_ms: row.get::<_, i64>(14)? as u64,
        })
    })())
}

fn member_row(row: &Row<'_>) -> rusqlite::Result<Result<MemberRow, PoolStoreError>> {
    let vm_id: String = row.get(0)?;
    let pool_id: String = row.get(1)?;
    let state: String = row.get(2)?;
    let created_at_ms = row.get::<_, i64>(3)? as u64;
    Ok((|| {
        Ok(MemberRow {
            vm_id: decode_id(&vm_id, &vm_id)?,
            pool_id: decode_id(&vm_id, &pool_id)?,
            state: decode_member_state(&vm_id, &state)?,
            created_at_ms,
        })
    })())
}

fn lease_row(row: &Row<'_>) -> rusqlite::Result<Result<LeaseRow, PoolStoreError>> {
    let id: String = row.get(0)?;
    let pool_id: String = row.get(1)?;
    let vm_id: String = row.get(2)?;
    let state: String = row.get(3)?;
    let idempotency_key: Option<String> = row.get(4)?;
    let acquired_at_ms = row.get::<_, i64>(5)? as u64;
    let expires_at_ms = row.get::<_, i64>(6)? as u64;
    let ended_at_ms = row.get::<_, Option<i64>>(7)?.map(|ms| ms as u64);
    Ok((|| {
        Ok(LeaseRow {
            id: decode_id(&id, &id)?,
            pool_id: decode_id(&id, &pool_id)?,
            vm_id: decode_id(&id, &vm_id)?,
            state: decode_lease_state(&id, &state)?,
            idempotency_key,
            acquired_at_ms,
            expires_at_ms,
            ended_at_ms,
        })
    })())
}

fn decode_id(row_id: &str, value: &str) -> Result<Uuid, PoolStoreError> {
    Uuid::parse_str(value).map_err(|_| PoolStoreError::Corrupt {
        id: row_id.to_owned(),
        reason: format!("{value:?} is not a UUID"),
    })
}

fn encode_member_state(state: PoolMemberState) -> &'static str {
    match state {
        PoolMemberState::Provisioning => "provisioning",
        PoolMemberState::Ready => "ready",
        PoolMemberState::Leased => "leased",
        PoolMemberState::Draining => "draining",
    }
}

fn decode_member_state(row_id: &str, value: &str) -> Result<PoolMemberState, PoolStoreError> {
    match value {
        "provisioning" => Ok(PoolMemberState::Provisioning),
        "ready" => Ok(PoolMemberState::Ready),
        "leased" => Ok(PoolMemberState::Leased),
        "draining" => Ok(PoolMemberState::Draining),
        other => Err(PoolStoreError::Corrupt {
            id: row_id.to_owned(),
            reason: format!("unknown member state {other:?}"),
        }),
    }
}

fn encode_lease_state(state: PoolLeaseState) -> &'static str {
    match state {
        PoolLeaseState::Active => "active",
        PoolLeaseState::Released => "released",
        PoolLeaseState::Expired => "expired",
    }
}

fn decode_lease_state(row_id: &str, value: &str) -> Result<PoolLeaseState, PoolStoreError> {
    match value {
        "active" => Ok(PoolLeaseState::Active),
        "released" => Ok(PoolLeaseState::Released),
        "expired" => Ok(PoolLeaseState::Expired),
        other => Err(PoolStoreError::Corrupt {
            id: row_id.to_owned(),
            reason: format!("unknown lease state {other:?}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Arc, Barrier};

    use tempfile::tempdir;

    use super::*;

    fn store(dir: &std::path::Path) -> Store {
        Store::open(&dir.join("firecrab.db")).unwrap()
    }

    fn pool(name: &str) -> PoolRow {
        PoolRow {
            id: Uuid::new_v4(),
            name: name.to_owned(),
            template: "alpine-3.24.1".to_owned(),
            template_version: "alpine-3.24.1-v5".to_owned(),
            cpu: 1,
            ram: 512,
            disk_gb: 2,
            egress_policy: EgressPolicy::Isolated,
            micro_network_id: Uuid::from_u128(1),
            storage_root: "default".to_owned(),
            min_ready: 2,
            max_size: 4,
            lease_ttl_seconds: 600,
            deleting: false,
            created_at_ms: 1,
        }
    }

    fn member(store: &Store, pool_id: Uuid, state: PoolMemberState, created_at_ms: u64) -> Uuid {
        let vm_id = Uuid::new_v4();
        store
            .insert_pool_member(&MemberRow {
                vm_id,
                pool_id,
                state,
                created_at_ms,
            })
            .unwrap();
        vm_id
    }

    fn state_of(store: &Store, pool_id: Uuid, vm_id: Uuid) -> PoolMemberState {
        store
            .pool_members(pool_id)
            .unwrap()
            .into_iter()
            .find(|member| member.vm_id == vm_id)
            .unwrap()
            .state
    }

    #[test]
    fn pools_round_trip_and_survive_reopen() {
        let directory = tempdir().unwrap();
        let row = pool("ci");
        store(directory.path()).insert_pool(&row).unwrap();

        let reopened = store(directory.path());

        assert_eq!(reopened.pool(row.id).unwrap(), Some(row.clone()));
        assert_eq!(reopened.list_pools().unwrap(), vec![row]);
    }

    #[test]
    fn a_duplicate_pool_name_is_rejected() {
        let directory = tempdir().unwrap();
        let store = store(directory.path());
        store.insert_pool(&pool("ci")).unwrap();

        let error = store.insert_pool(&pool("ci")).unwrap_err();

        assert!(matches!(error, PoolStoreError::DuplicateName(name) if name == "ci"));
    }

    #[test]
    fn acquire_leases_the_oldest_ready_member_until_its_ttl() {
        let directory = tempdir().unwrap();
        let store = store(directory.path());
        let row = pool("ci");
        store.insert_pool(&row).unwrap();
        member(&store, row.id, PoolMemberState::Provisioning, 1);
        let newer = member(&store, row.id, PoolMemberState::Ready, 3);
        let older = member(&store, row.id, PoolMemberState::Ready, 2);

        let acquired = store
            .acquire_pool_lease(row.id, None, 1_000, |_| true)
            .unwrap();

        assert!(!acquired.replayed);
        assert_eq!(acquired.lease.vm_id, older);
        assert_eq!(acquired.lease.state, PoolLeaseState::Active);
        assert_eq!(acquired.lease.expires_at_ms, 1_000 + 600 * 1000);
        assert_eq!(state_of(&store, row.id, older), PoolMemberState::Leased);
        assert_eq!(state_of(&store, row.id, newer), PoolMemberState::Ready);
    }

    #[test]
    fn acquire_skips_ready_members_that_are_not_leasable() {
        let directory = tempdir().unwrap();
        let store = store(directory.path());
        let row = pool("ci");
        store.insert_pool(&row).unwrap();
        let dead = member(&store, row.id, PoolMemberState::Ready, 1);
        let live = member(&store, row.id, PoolMemberState::Ready, 2);

        let acquired = store
            .acquire_pool_lease(row.id, None, 0, |vm_id| vm_id != dead)
            .unwrap();

        assert_eq!(acquired.lease.vm_id, live);
        assert_eq!(state_of(&store, row.id, dead), PoolMemberState::Ready);
    }

    #[test]
    fn acquire_without_a_ready_member_is_exhausted() {
        let directory = tempdir().unwrap();
        let store = store(directory.path());
        let row = pool("ci");
        store.insert_pool(&row).unwrap();
        member(&store, row.id, PoolMemberState::Provisioning, 1);

        let error = store
            .acquire_pool_lease(row.id, None, 0, |_| true)
            .unwrap_err();

        assert!(matches!(error, PoolStoreError::Exhausted(id) if id == row.id));
    }

    #[test]
    fn a_retried_acquire_returns_the_original_lease() {
        let directory = tempdir().unwrap();
        let store = store(directory.path());
        let row = pool("ci");
        store.insert_pool(&row).unwrap();
        let first_vm = member(&store, row.id, PoolMemberState::Ready, 1);
        let second_vm = member(&store, row.id, PoolMemberState::Ready, 2);

        let first = store
            .acquire_pool_lease(row.id, Some("job-7"), 0, |_| true)
            .unwrap();
        let retry = store
            .acquire_pool_lease(row.id, Some("job-7"), 5, |_| true)
            .unwrap();

        assert!(retry.replayed);
        assert_eq!(retry.lease, first.lease);
        assert_eq!(first.lease.vm_id, first_vm);
        assert_eq!(state_of(&store, row.id, second_vm), PoolMemberState::Ready);
    }

    #[test]
    fn concurrent_acquires_never_return_the_same_vm() {
        let directory = tempdir().unwrap();
        let store = store(directory.path());
        let row = pool("ci");
        store.insert_pool(&row).unwrap();
        for created in 0..4 {
            member(&store, row.id, PoolMemberState::Ready, created);
        }
        let barrier = Arc::new(Barrier::new(8));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let store = store.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store.acquire_pool_lease(row.id, None, 0, |_| true)
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let leased: Vec<Uuid> = results
            .iter()
            .filter_map(|result| result.as_ref().ok().map(|acquired| acquired.lease.vm_id))
            .collect();
        assert_eq!(leased.len(), 4, "every ready member is leased once");
        assert_eq!(leased.iter().collect::<HashSet<_>>().len(), 4);
        assert!(
            results
                .iter()
                .filter(|result| result.is_err())
                .all(|result| matches!(result, Err(PoolStoreError::Exhausted(_))))
        );
    }

    #[test]
    fn acquire_refuses_a_deleting_or_missing_pool() {
        let directory = tempdir().unwrap();
        let store = store(directory.path());
        let row = pool("ci");
        store.insert_pool(&row).unwrap();
        member(&store, row.id, PoolMemberState::Ready, 1);
        store.mark_pool_deleting(row.id).unwrap();

        let deleting = store
            .acquire_pool_lease(row.id, None, 0, |_| true)
            .unwrap_err();
        let missing = store
            .acquire_pool_lease(Uuid::new_v4(), None, 0, |_| true)
            .unwrap_err();

        assert!(matches!(deleting, PoolStoreError::Deleting(_)));
        assert!(matches!(missing, PoolStoreError::MissingPool(_)));
    }

    #[test]
    fn ending_a_lease_drains_its_member_and_is_idempotent() {
        let directory = tempdir().unwrap();
        let store = store(directory.path());
        let row = pool("ci");
        store.insert_pool(&row).unwrap();
        let vm_id = member(&store, row.id, PoolMemberState::Ready, 1);
        let lease = store
            .acquire_pool_lease(row.id, None, 0, |_| true)
            .unwrap()
            .lease;

        let released = store
            .end_pool_lease(lease.id, PoolLeaseState::Released, 50)
            .unwrap()
            .unwrap();
        let again = store
            .end_pool_lease(lease.id, PoolLeaseState::Expired, 90)
            .unwrap()
            .unwrap();

        assert_eq!(released.state, PoolLeaseState::Released);
        assert_eq!(released.ended_at_ms, Some(50));
        assert_eq!(again, released, "a second end changes nothing");
        assert_eq!(state_of(&store, row.id, vm_id), PoolMemberState::Draining);
        assert_eq!(store.active_pool_lease_for_vm(vm_id).unwrap(), None);
    }

    #[test]
    fn expired_leases_are_the_active_ones_past_their_ttl() {
        let directory = tempdir().unwrap();
        let store = store(directory.path());
        let row = pool("ci");
        store.insert_pool(&row).unwrap();
        member(&store, row.id, PoolMemberState::Ready, 1);
        let lease = store
            .acquire_pool_lease(row.id, None, 0, |_| true)
            .unwrap()
            .lease;

        assert!(
            store
                .expired_pool_leases(lease.expires_at_ms - 1)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.expired_pool_leases(lease.expires_at_ms).unwrap(),
            vec![lease]
        );
    }

    #[test]
    fn a_pool_with_an_active_lease_cannot_be_deleted() {
        let directory = tempdir().unwrap();
        let store = store(directory.path());
        let row = pool("ci");
        store.insert_pool(&row).unwrap();
        member(&store, row.id, PoolMemberState::Ready, 1);
        store.acquire_pool_lease(row.id, None, 0, |_| true).unwrap();

        let error = store.mark_pool_deleting(row.id).unwrap_err();

        assert!(matches!(error, PoolStoreError::InUse { count: 1, .. }));
        assert!(!store.pool(row.id).unwrap().unwrap().deleting);
    }

    #[test]
    fn pruning_forgets_only_ended_leases_older_than_the_cutoff() {
        let directory = tempdir().unwrap();
        let store = store(directory.path());
        let row = pool("ci");
        store.insert_pool(&row).unwrap();
        member(&store, row.id, PoolMemberState::Ready, 1);
        member(&store, row.id, PoolMemberState::Ready, 2);
        let ended = store
            .acquire_pool_lease(row.id, Some("a"), 0, |_| true)
            .unwrap()
            .lease;
        let active = store
            .acquire_pool_lease(row.id, Some("b"), 0, |_| true)
            .unwrap()
            .lease;
        store
            .end_pool_lease(ended.id, PoolLeaseState::Released, 10)
            .unwrap();

        let pruned = store.prune_ended_pool_leases(11).unwrap();

        assert_eq!(pruned, 1);
        assert_eq!(store.pool_leases(row.id).unwrap(), vec![active]);
    }
}

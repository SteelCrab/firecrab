//! IP address and MAC allocation management (IPAM): hands out unique
//! IPv4/MAC leases from the fixed 172.30.0.0/24 subnet, backed by SQLite so
//! allocation is atomic under concurrent VM creation.

use std::collections::HashSet;
use std::net::Ipv4Addr;

use rusqlite::{OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::model::{Lease, MacAddr};

/// Schema for the `network_leases` table.
pub const CREATE_LEASES_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS network_leases (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    vm_id TEXT NOT NULL,
    ipv4 TEXT NOT NULL,
    mac TEXT NOT NULL,
    allocated_at TEXT NOT NULL,
    released_at TEXT
) STRICT";

/// Partial indexes: uniqueness only applies to still-active leases, so
/// released rows stay behind as history without blocking reuse.
pub const CREATE_LEASES_INDEXES_SQL: [&str; 3] = [
    "CREATE UNIQUE INDEX IF NOT EXISTS network_leases_active_vm \
     ON network_leases(vm_id) WHERE released_at IS NULL",
    "CREATE UNIQUE INDEX IF NOT EXISTS network_leases_active_ipv4 \
     ON network_leases(ipv4) WHERE released_at IS NULL",
    "CREATE UNIQUE INDEX IF NOT EXISTS network_leases_active_mac \
     ON network_leases(mac) WHERE released_at IS NULL",
];

/// Subnet network address; mirrors the fixed `fcbr0` config in
/// `firecrab-net-helper/src/bridge.rs`.
const NETWORK: Ipv4Addr = Ipv4Addr::new(172, 30, 0, 0);
/// Subnet gateway address, reserved from allocation.
const GATEWAY: Ipv4Addr = Ipv4Addr::new(172, 30, 0, 1);
/// Subnet broadcast address, reserved from allocation.
const BROADCAST: Ipv4Addr = Ipv4Addr::new(172, 30, 0, 255);
/// How many salted MAC candidates to try before giving up.
const MAX_MAC_ATTEMPTS: u32 = 8;

/// Failure modes for allocating or releasing a network lease.
#[derive(Debug, Error)]
pub enum IpamError {
    /// A SQLite query/statement failed.
    #[error("network lease operation failed")]
    Database(#[from] rusqlite::Error),
    /// Every host address in the subnet is already leased.
    #[error("no free IPv4 address left in 172.30.0.0/24")]
    PoolExhausted,
    /// No unclaimed MAC was found within [`MAX_MAC_ATTEMPTS`] salted tries.
    #[error("could not find a free MAC address after {MAX_MAC_ATTEMPTS} attempts")]
    MacPoolExhausted,
    /// The VM already holds an unreleased lease.
    #[error("vm {vm_id} already has an active network lease")]
    AlreadyLeased {
        /// The VM that already has an active lease.
        vm_id: Uuid,
    },
    /// The VM has no active lease to release.
    #[error("vm {vm_id} has no active network lease to release")]
    NotLeased {
        /// The VM with no active lease.
        vm_id: Uuid,
    },
    /// A lease row's stored ipv4/mac text didn't parse — the schema only
    /// ever accepts values this module itself wrote, so this means the
    /// database was altered out from under it.
    #[error("vm {vm_id}'s stored lease is corrupt: {reason}")]
    CorruptLease {
        /// The VM whose lease row is corrupt.
        vm_id: Uuid,
        /// Human-readable reason.
        reason: String,
    },
}

/// Allocate an IPv4 + MAC for `vm_id`. Must run inside a `BEGIN IMMEDIATE`
/// transaction (see `Store::allocate_lease`) so concurrent callers serialize
/// on the same write lock instead of racing on the free-address scan.
pub fn allocate(tx: &Transaction<'_>, vm_id: Uuid) -> Result<Lease, IpamError> {
    if has_active_lease(tx, vm_id)? {
        return Err(IpamError::AlreadyLeased { vm_id });
    }

    let taken_ips = active_ipv4s(tx)?;
    let ipv4 = (2_u8..255)
        .map(|last| Ipv4Addr::new(172, 30, 0, last))
        .find(|candidate| !taken_ips.contains(candidate))
        .ok_or(IpamError::PoolExhausted)?;

    let taken_macs = active_macs(tx)?;
    let mac = (0..MAX_MAC_ATTEMPTS)
        .map(|salt| derive_mac(vm_id, salt))
        .find(|candidate| !taken_macs.contains(candidate))
        .ok_or(IpamError::MacPoolExhausted)?;

    tx.execute(
        "INSERT INTO network_leases (vm_id, ipv4, mac, allocated_at) \
         VALUES (?1, ?2, ?3, datetime('now'))",
        params![vm_id.to_string(), ipv4.to_string(), mac.to_string()],
    )?;
    bump_lease_revision(tx)?;

    Ok(Lease { vm_id, ipv4, mac })
}

/// Release `vm_id`'s active lease. The row is kept with `released_at` set
/// rather than deleted, so the address/MAC free up for reuse while history
/// survives. Callers must only invoke this once VM cleanup (policy, TAP,
/// artifacts) has fully succeeded.
pub fn release(tx: &Transaction<'_>, vm_id: Uuid) -> Result<(), IpamError> {
    let changed = tx.execute(
        "UPDATE network_leases SET released_at = datetime('now') \
         WHERE vm_id = ?1 AND released_at IS NULL",
        params![vm_id.to_string()],
    )?;
    if changed == 0 {
        return Err(IpamError::NotLeased { vm_id });
    }
    bump_lease_revision(tx)?;
    Ok(())
}

/// Looks up `vm_id`'s current active lease, if it has one. Unlike
/// [`allocate`]/[`release`], this is a plain read with no need for a
/// `BEGIN IMMEDIATE` transaction.
pub fn active_lease(conn: &rusqlite::Connection, vm_id: Uuid) -> Result<Option<Lease>, IpamError> {
    conn.query_row(
        "SELECT ipv4, mac FROM network_leases WHERE vm_id = ?1 AND released_at IS NULL",
        params![vm_id.to_string()],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )
    .optional()?
    .map(|(ipv4, mac)| parse_lease(vm_id, ipv4, mac))
    .transpose()
}

/// Every currently-active lease, for handing the DHCP helper a full
/// snapshot to render its host-reservation file from (see
/// [`current_revision`], sent alongside so a stale snapshot is never
/// applied out of order).
pub fn active_leases(conn: &rusqlite::Connection) -> Result<Vec<Lease>, IpamError> {
    let mut statement =
        conn.prepare("SELECT vm_id, ipv4, mac FROM network_leases WHERE released_at IS NULL")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    rows.map(|row| {
        let (vm_id, ipv4, mac) = row?;
        let vm_id = Uuid::parse_str(&vm_id).map_err(|_| IpamError::CorruptLease {
            vm_id: Uuid::nil(),
            reason: format!("stored vm_id {vm_id:?} is not a UUID"),
        })?;
        parse_lease(vm_id, ipv4, mac)
    })
    .collect()
}

fn parse_lease(vm_id: Uuid, ipv4: String, mac: String) -> Result<Lease, IpamError> {
    let ipv4 = ipv4.parse().map_err(|_| IpamError::CorruptLease {
        vm_id,
        reason: format!("stored ipv4 {ipv4:?} does not parse"),
    })?;
    let mac = mac.parse().map_err(|_| IpamError::CorruptLease {
        vm_id,
        reason: format!("stored mac {mac:?} does not parse"),
    })?;
    Ok(Lease { vm_id, ipv4, mac })
}

/// Current lease generation, bumped by every [`allocate`]/[`release`] (see
/// [`bump_lease_revision`]). Read alone (no transaction) is fine: a caller
/// racing a concurrent bump just sees the pre- or post-bump value, either of
/// which is a valid revision to tag a snapshot with.
pub fn current_revision(conn: &rusqlite::Connection) -> Result<u64, IpamError> {
    Ok(conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))? as u64)
}

/// Bumps the lease generation counter, reusing SQLite's built-in
/// `user_version` pragma rather than a dedicated table/column. Must run
/// inside the same `BEGIN IMMEDIATE` transaction as the lease change so the
/// two commit atomically together — otherwise a crash between them could
/// leave the revision under- or over-counted relative to what's actually
/// stored.
fn bump_lease_revision(tx: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    let current: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    tx.pragma_update(None, "user_version", current + 1)
}

/// Whether `vm_id` currently holds an unreleased lease.
fn has_active_lease(tx: &Transaction<'_>, vm_id: Uuid) -> Result<bool, rusqlite::Error> {
    tx.query_row(
        "SELECT 1 FROM network_leases WHERE vm_id = ?1 AND released_at IS NULL",
        params![vm_id.to_string()],
        |_| Ok(()),
    )
    .optional()
    .map(|row| row.is_some())
}

/// Every IPv4 address currently unavailable for allocation: reserved
/// network/gateway/broadcast plus every still-leased address.
fn active_ipv4s(tx: &Transaction<'_>) -> Result<HashSet<Ipv4Addr>, rusqlite::Error> {
    let mut statement = tx.prepare("SELECT ipv4 FROM network_leases WHERE released_at IS NULL")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut set = HashSet::from([NETWORK, GATEWAY, BROADCAST]);
    for row in rows {
        if let Ok(addr) = row?.parse() {
            set.insert(addr);
        }
    }
    Ok(set)
}

/// Every MAC address currently claimed by a still-leased VM.
fn active_macs(tx: &Transaction<'_>) -> Result<HashSet<MacAddr>, rusqlite::Error> {
    let mut statement = tx.prepare("SELECT mac FROM network_leases WHERE released_at IS NULL")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut set = HashSet::new();
    for row in rows {
        if let Ok(mac) = row?.parse() {
            set.insert(mac);
        }
    }
    Ok(set)
}

/// Deterministically derives a candidate MAC from `vm_id` and `salt`, so
/// retrying with an incremented salt tries a different address without
/// needing to track previously-tried candidates.
fn derive_mac(vm_id: Uuid, salt: u32) -> MacAddr {
    let mut hasher = Sha256::new();
    hasher.update(vm_id.as_bytes());
    hasher.update(salt.to_be_bytes());
    let digest = hasher.finalize();
    // 02:FC prefix marks locally-administered, Firecrab-owned MACs.
    MacAddr([0x02, 0xFC, digest[0], digest[1], digest[2], digest[3]])
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, TransactionBehavior};

    use super::*;

    fn open() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(CREATE_LEASES_TABLE_SQL, []).unwrap();
        for sql in CREATE_LEASES_INDEXES_SQL {
            conn.execute(sql, []).unwrap();
        }
        conn
    }

    fn begin(conn: &mut Connection) -> Transaction<'_> {
        conn.transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap()
    }

    #[test]
    fn lease_revision_bumps_on_both_allocate_and_release() {
        let mut conn = open();
        assert_eq!(current_revision(&conn).unwrap(), 0);

        let vm_id = Uuid::new_v4();
        let tx = begin(&mut conn);
        allocate(&tx, vm_id).unwrap();
        tx.commit().unwrap();
        assert_eq!(current_revision(&conn).unwrap(), 1);

        let tx = begin(&mut conn);
        release(&tx, vm_id).unwrap();
        tx.commit().unwrap();
        assert_eq!(current_revision(&conn).unwrap(), 2);
    }

    #[test]
    fn active_leases_lists_only_unreleased_rows() {
        let mut conn = open();
        let kept = Uuid::new_v4();
        let released = Uuid::new_v4();

        let tx = begin(&mut conn);
        allocate(&tx, kept).unwrap();
        allocate(&tx, released).unwrap();
        tx.commit().unwrap();

        let tx = begin(&mut conn);
        release(&tx, released).unwrap();
        tx.commit().unwrap();

        let leases = active_leases(&conn).unwrap();
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].vm_id, kept);
    }

    #[test]
    fn allocates_distinct_addresses_across_many_vms() {
        let mut conn = open();
        let mut seen_ips = HashSet::new();
        let mut seen_macs = HashSet::new();

        for _ in 0..50 {
            let tx = begin(&mut conn);
            let lease = allocate(&tx, Uuid::new_v4()).unwrap();
            tx.commit().unwrap();
            assert!(seen_ips.insert(lease.ipv4), "duplicate ip {}", lease.ipv4);
            assert!(seen_macs.insert(lease.mac), "duplicate mac {}", lease.mac);
        }
    }

    #[test]
    fn reserved_addresses_are_never_handed_out() {
        let mut conn = open();
        for _ in 0..253 {
            let tx = begin(&mut conn);
            let lease = allocate(&tx, Uuid::new_v4()).unwrap();
            tx.commit().unwrap();
            assert_ne!(lease.ipv4, NETWORK);
            assert_ne!(lease.ipv4, GATEWAY);
            assert_ne!(lease.ipv4, BROADCAST);
        }
    }

    #[test]
    fn active_lease_reports_a_corrupt_stored_ipv4() {
        let mut conn = open();
        let vm_id = Uuid::new_v4();
        let tx = begin(&mut conn);
        allocate(&tx, vm_id).unwrap();
        tx.commit().unwrap();

        conn.execute(
            "UPDATE network_leases SET ipv4 = 'not-an-ip' WHERE vm_id = ?1",
            params![vm_id.to_string()],
        )
        .unwrap();

        assert!(matches!(
            active_lease(&conn, vm_id),
            Err(IpamError::CorruptLease { .. })
        ));
    }

    #[test]
    fn pool_exhaustion_is_reported_once_all_253_hosts_are_leased() {
        let mut conn = open();
        for _ in 0..253 {
            let tx = begin(&mut conn);
            allocate(&tx, Uuid::new_v4()).unwrap();
            tx.commit().unwrap();
        }

        let tx = begin(&mut conn);
        assert!(matches!(
            allocate(&tx, Uuid::new_v4()),
            Err(IpamError::PoolExhausted)
        ));
    }

    #[test]
    fn same_vm_cannot_hold_two_active_leases() {
        let mut conn = open();
        let vm_id = Uuid::new_v4();
        let tx = begin(&mut conn);
        allocate(&tx, vm_id).unwrap();
        tx.commit().unwrap();

        let tx = begin(&mut conn);
        assert!(matches!(
            allocate(&tx, vm_id),
            Err(IpamError::AlreadyLeased { vm_id: leased }) if leased == vm_id
        ));
    }

    #[test]
    fn release_then_reallocate_reuses_the_freed_address() {
        let mut conn = open();
        let first_vm = Uuid::new_v4();
        let tx = begin(&mut conn);
        let first_lease = allocate(&tx, first_vm).unwrap();
        tx.commit().unwrap();

        let tx = begin(&mut conn);
        release(&tx, first_vm).unwrap();
        tx.commit().unwrap();

        // History row survives, released.
        let history_count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM network_leases WHERE vm_id = ?1",
                params![first_vm.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(history_count, 1);

        let second_vm = Uuid::new_v4();
        let tx = begin(&mut conn);
        let second_lease = allocate(&tx, second_vm).unwrap();
        tx.commit().unwrap();
        assert_eq!(second_lease.ipv4, first_lease.ipv4);
    }

    #[test]
    fn releasing_a_vm_without_a_lease_fails() {
        let mut conn = open();
        let tx = begin(&mut conn);
        assert!(matches!(
            release(&tx, Uuid::new_v4()),
            Err(IpamError::NotLeased { .. })
        ));
    }

    #[test]
    fn mac_collisions_bump_the_salt() {
        let mut conn = open();
        let vm_id = Uuid::new_v4();

        // Occupy the salt=0 MAC under a different, already-active vm so the
        // real allocation must skip it.
        let blocker = Uuid::new_v4();
        let tx = begin(&mut conn);
        tx.execute(
            "INSERT INTO network_leases (vm_id, ipv4, mac, allocated_at) \
             VALUES (?1, ?2, ?3, datetime('now'))",
            params![
                blocker.to_string(),
                Ipv4Addr::new(172, 30, 0, 2).to_string(),
                derive_mac(vm_id, 0).to_string(),
            ],
        )
        .unwrap();
        tx.commit().unwrap();

        let tx = begin(&mut conn);
        let lease = allocate(&tx, vm_id).unwrap();
        tx.commit().unwrap();
        assert_eq!(lease.mac, derive_mac(vm_id, 1));
    }

    #[test]
    fn mac_pool_exhaustion_rolls_back_without_leaving_a_partial_row() {
        let mut conn = open();
        let vm_id = Uuid::new_v4();

        // Pre-occupy every salt-derived MAC for this vm_id under distinct
        // blockers, so allocation cannot find a free one and must abort.
        let tx = begin(&mut conn);
        for (index, salt) in (0..MAX_MAC_ATTEMPTS).enumerate() {
            tx.execute(
                "INSERT INTO network_leases (vm_id, ipv4, mac, allocated_at) \
                 VALUES (?1, ?2, ?3, datetime('now'))",
                params![
                    Uuid::new_v4().to_string(),
                    Ipv4Addr::new(172, 30, 0, 2 + index as u8).to_string(),
                    derive_mac(vm_id, salt).to_string(),
                ],
            )
            .unwrap();
        }
        tx.commit().unwrap();

        let tx = begin(&mut conn);
        let result = allocate(&tx, vm_id);
        assert!(matches!(result, Err(IpamError::MacPoolExhausted)));
        drop(tx); // no commit: rolls back

        let leaked: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM network_leases WHERE vm_id = ?1",
                params![vm_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            leaked, 0,
            "failed allocation must not leave a lease row behind"
        );
    }
}

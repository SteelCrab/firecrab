use std::collections::HashSet;
use std::net::Ipv4Addr;

use rusqlite::{OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::model::{Lease, MacAddr};

pub const CREATE_LEASES_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS network_leases (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    vm_id TEXT NOT NULL,
    ipv4 TEXT NOT NULL,
    mac TEXT NOT NULL,
    allocated_at TEXT NOT NULL,
    released_at TEXT
) STRICT";

// Partial indexes: uniqueness only applies to still-active leases, so
// released rows stay behind as history without blocking reuse.
pub const CREATE_LEASES_INDEXES_SQL: [&str; 3] = [
    "CREATE UNIQUE INDEX IF NOT EXISTS network_leases_active_vm \
     ON network_leases(vm_id) WHERE released_at IS NULL",
    "CREATE UNIQUE INDEX IF NOT EXISTS network_leases_active_ipv4 \
     ON network_leases(ipv4) WHERE released_at IS NULL",
    "CREATE UNIQUE INDEX IF NOT EXISTS network_leases_active_mac \
     ON network_leases(mac) WHERE released_at IS NULL",
];

// Mirrors the fixed fcbr0 config in firecrab-net-helper/src/bridge.rs.
const NETWORK: Ipv4Addr = Ipv4Addr::new(172, 30, 0, 0);
const GATEWAY: Ipv4Addr = Ipv4Addr::new(172, 30, 0, 1);
const BROADCAST: Ipv4Addr = Ipv4Addr::new(172, 30, 0, 255);
const MAX_MAC_ATTEMPTS: u32 = 8;

#[derive(Debug, Error)]
pub enum IpamError {
    #[error("network lease operation failed")]
    Database(#[from] rusqlite::Error),
    #[error("no free IPv4 address left in 172.30.0.0/24")]
    PoolExhausted,
    #[error("could not find a free MAC address after {MAX_MAC_ATTEMPTS} attempts")]
    MacPoolExhausted,
    #[error("vm {vm_id} already has an active network lease")]
    AlreadyLeased { vm_id: Uuid },
    #[error("vm {vm_id} has no active network lease to release")]
    NotLeased { vm_id: Uuid },
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
    Ok(())
}

fn has_active_lease(tx: &Transaction<'_>, vm_id: Uuid) -> Result<bool, rusqlite::Error> {
    tx.query_row(
        "SELECT 1 FROM network_leases WHERE vm_id = ?1 AND released_at IS NULL",
        params![vm_id.to_string()],
        |_| Ok(()),
    )
    .optional()
    .map(|row| row.is_some())
}

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
    fn pool_exhaustion_is_reported_once_all_253_hosts_are_leased() {
        let mut conn = open();
        for _ in 0..253 {
            let tx = begin(&mut conn);
            allocate(&tx, Uuid::new_v4()).unwrap();
            tx.commit().unwrap();
        }

        let tx = begin(&mut conn);
        assert!(matches!(allocate(&tx, Uuid::new_v4()), Err(IpamError::PoolExhausted)));
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
        assert_eq!(leaked, 0, "failed allocation must not leave a lease row behind");
    }
}

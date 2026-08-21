//! IP address and MAC allocation management (IPAM): hands out unique
//! IPv4/MAC leases from a MicroNetwork's subnet
//! (`public-docs/networking.md`) — backed by SQLite so allocation
//! is atomic under concurrent VM creation.

use std::collections::HashSet;
use std::net::{Ipv4Addr, Ipv6Addr};

use firecrab_helper_protocol::network::{
    Ipv6AddressMode, MicroNetworkIpv6Spec, MicroNetworkSpec, slaac_address,
};
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
    released_at TEXT,
    micro_network_id TEXT,
    ipv6 TEXT
) STRICT";

/// Adds `micro_network_id` to a `network_leases` table created before
/// MicroNetworks existed. Pre-existing rows are backfilled by
/// [`crate::persistence::Store`] promotion to an explicit MicroNetwork.
pub const ADD_LEASE_MICRO_NETWORK_COLUMN_SQL: &str =
    "ALTER TABLE network_leases ADD COLUMN micro_network_id TEXT";

/// Adds `ipv6` to a `network_leases` table created before dual-stack
/// MicroNetworks existed. `NULL` is the IPv4-only lease every row held then,
/// and stays correct for a network that has no IPv6 prefix.
pub const ADD_LEASE_IPV6_COLUMN_SQL: &str = "ALTER TABLE network_leases ADD COLUMN ipv6 TEXT";

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

/// Legacy implicit-default CIDR used only when promoting old NULL
/// `micro_network_id` rows to an explicit MicroNetwork on upgrade.
pub(crate) const LEGACY_DEFAULT_NETWORK: Ipv4Addr = Ipv4Addr::new(172, 30, 0, 0);
/// Legacy gateway for the same promotion path.
pub(crate) const LEGACY_DEFAULT_GATEWAY: Ipv4Addr = Ipv4Addr::new(172, 30, 0, 1);
/// Legacy prefix for the same promotion path.
pub(crate) const LEGACY_DEFAULT_PREFIX: u8 = 24;
/// How many salted MAC candidates to try before giving up.
const MAX_MAC_ATTEMPTS: u32 = 8;

/// The subnet a lease is allocated from — always a real MicroNetwork.
/// Addresses are derived from `network`/`prefix` rather than stored, the
/// same way `firecrab-net-helper`'s bridge derives them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubnetSpec {
    /// The MicroNetwork this subnet belongs to.
    pub micro_network_id: Uuid,
    /// Network (base) address.
    pub network: Ipv4Addr,
    /// CIDR prefix length.
    pub prefix: u8,
    /// The network's IPv6 plan, when it is dual-stack. IPv4 stays mandatory
    /// — this is a second family, never a replacement
    /// (`public-docs/networking.md`).
    pub ipv6: Option<MicroNetworkIpv6Spec>,
}

impl SubnetSpec {
    /// First host address, reserved as the gateway.
    pub fn gateway(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.network) + 1)
    }

    /// Last address in the subnet, reserved as broadcast.
    pub fn broadcast(&self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.network) | !self.mask())
    }

    fn mask(&self) -> u32 {
        u32::MAX
            .checked_shl(32 - u32::from(self.prefix))
            .unwrap_or(0)
    }

    /// Whether `address` falls inside this subnet.
    fn contains(&self, address: Ipv4Addr) -> bool {
        u32::from(address) & self.mask() == u32::from(self.network) & self.mask()
    }

    /// Whether two subnets share any address. The helper refuses an
    /// overlapping bridge anyway; checking here first turns that into a
    /// field-level validation error instead of a rolled-back internal one.
    pub fn overlaps(&self, other: &Self) -> bool {
        self.contains(other.network) || other.contains(self.network)
    }

    /// Every assignable host address: everything between the gateway and the
    /// broadcast address, both of which are reserved.
    fn host_addresses(&self) -> impl Iterator<Item = Ipv4Addr> {
        (u32::from(self.gateway()) + 1..u32::from(self.broadcast())).map(Ipv4Addr::from)
    }

    /// How many addresses this subnet can hand out — everything except the
    /// network, gateway and broadcast addresses.
    pub fn usable_addresses(&self) -> u32 {
        u32::from(self.broadcast()).saturating_sub(u32::from(self.gateway()) + 1)
    }

    /// Parses a MicroNetwork's stored `<network>/<prefix>` CIDR. The one
    /// place subnet text is turned into numbers, so the API, the DHCP
    /// snapshot and the firewall ruleset can't drift apart on what a stored
    /// CIDR means.
    pub fn parse(micro_network_id: Uuid, cidr: &str) -> Option<Self> {
        let (network, prefix) = cidr.split_once('/')?;
        let network: Ipv4Addr = network.parse().ok()?;
        let prefix: u8 = prefix.parse().ok()?;
        (prefix <= 32).then_some(Self {
            micro_network_id,
            network,
            prefix,
            ipv6: None,
        })
    }

    /// The same subnet with an IPv6 plan attached (or cleared). Kept
    /// separate from [`Self::parse`] so every existing IPv4-only caller
    /// keeps its current behavior untouched.
    pub fn with_ipv6(self, ipv6: Option<MicroNetworkIpv6Spec>) -> Self {
        Self { ipv6, ..self }
    }

    /// Parses a MicroNetwork's stored IPv6 `<prefix>/<length>` into the plan
    /// the helper and the leases are derived from. The gateway is the
    /// prefix's first address, mirroring how [`Self::gateway`] derives the
    /// IPv4 one rather than storing it. The host bits of a non-canonical
    /// prefix such as `fd00::5/64` are masked off first, so the gateway is
    /// `fd00::1` and not `fd00::6`.
    pub fn parse_ipv6(cidr: &str, address_mode: Ipv6AddressMode) -> Option<MicroNetworkIpv6Spec> {
        let (network, prefix) = cidr.split_once('/')?;
        let network: Ipv6Addr = network.parse().ok()?;
        let prefix: u8 = prefix.parse().ok()?;
        if prefix > 128 {
            return None;
        }
        let spec = MicroNetworkIpv6Spec {
            gateway: network,
            prefix,
            address_mode,
        };
        Some(MicroNetworkIpv6Spec {
            gateway: Ipv6Addr::from(u128::from(spec.network_address()).checked_add(1)?),
            prefix,
            address_mode,
        })
    }

    /// The privileged helper's view of this subnet. `internet_enabled` and
    /// `uplink` are the network's stored posture rather than anything
    /// derivable from the CIDR, so they are passed in. `uplink` of `None`
    /// means the helper should use the host default-route iface.
    pub fn helper_spec(&self, internet_enabled: bool, uplink: Option<String>) -> MicroNetworkSpec {
        MicroNetworkSpec {
            micro_network_id: self.micro_network_id,
            gateway: self.gateway(),
            prefix: self.prefix,
            internet_enabled,
            uplink,
            ipv6: self.ipv6,
        }
    }

    /// The IPv6 address this subnet hands `(ipv4, mac)`, or `None` when it
    /// is IPv4-only. Under SLAAC that is the EUI-64 the guest builds for
    /// itself from its MAC; under DHCPv6 it is the prefix plus the same host
    /// offset the v4 address got, so one lease row describes both families
    /// and neither can drift from the other.
    fn ipv6_for(&self, ipv4: Ipv4Addr, mac: MacAddr) -> Option<Ipv6Addr> {
        let ipv6 = self.ipv6?;
        Some(match ipv6.address_mode {
            Ipv6AddressMode::Slaac => slaac_address(ipv6.network_address(), mac),
            Ipv6AddressMode::Dhcpv6 => {
                let offset = u128::from(u32::from(ipv4).saturating_sub(u32::from(self.network)));
                Ipv6Addr::from(u128::from(ipv6.network_address()) | offset)
            }
        })
    }

    /// The subnet a stored MicroNetwork row describes, both families at
    /// once — the one place a row is turned into the spec the helper, the
    /// DHCP snapshot and the lease allocator all work from.
    pub fn from_micro_network(network: &firecrab_api_types::MicroNetworkResponse) -> Option<Self> {
        let ipv6 = network.ipv6_cidr.as_deref().and_then(|cidr| {
            Self::parse_ipv6(cidr, protocol_address_mode(network.ipv6_address_mode))
        });
        Some(Self::parse(network.id, &network.subnet_cidr)?.with_ipv6(ipv6))
    }

    /// The historical `172.30.0.0/24` layout under an explicit MicroNetwork
    /// id — used by upgrade promotion and unit tests that need a full /24.
    pub fn legacy_default_subnet(micro_network_id: Uuid) -> Self {
        Self {
            micro_network_id,
            network: LEGACY_DEFAULT_NETWORK,
            prefix: LEGACY_DEFAULT_PREFIX,
            ipv6: None,
        }
    }
}

/// Failure modes for allocating or releasing a network lease.
#[derive(Debug, Error)]
pub enum IpamError {
    /// A SQLite query/statement failed.
    #[error("network lease operation failed")]
    Database(#[from] rusqlite::Error),
    /// Every host address in the subnet is already leased.
    #[error("no free IPv4 address left in {network}/{prefix}")]
    PoolExhausted {
        /// Network address of the exhausted subnet.
        network: Ipv4Addr,
        /// Its prefix length.
        prefix: u8,
    },
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
pub fn allocate(tx: &Transaction<'_>, vm_id: Uuid, subnet: SubnetSpec) -> Result<Lease, IpamError> {
    allocate_excluding(tx, vm_id, subnet, &HashSet::new())
}

/// Atomically replaces `vm_id`'s active lease with a fresh address outside
/// `unavailable_ipv4s`. This is the recovery path for host state that outlived
/// its API record: an orphan MicroVM may still own an IP in nftables, so the
/// new VM must move rather than overwrite that policy.
///
/// The previous row is retained as released history. Releasing and selecting
/// the replacement run in one transaction, so a full pool or any database
/// error leaves the original active lease intact.
pub fn rotate(
    tx: &Transaction<'_>,
    vm_id: Uuid,
    subnet: SubnetSpec,
    unavailable_ipv4s: &HashSet<Ipv4Addr>,
) -> Result<Lease, IpamError> {
    let current = active_lease(&*tx, vm_id)?.ok_or(IpamError::NotLeased { vm_id })?;
    let mut excluded = unavailable_ipv4s.clone();
    // A caller may only know about the most recent conflict, but never hand
    // the just-released address straight back to the VM.
    excluded.insert(current.ipv4);

    release_active(tx, vm_id)?;
    allocate_excluding(tx, vm_id, subnet, &excluded)
}

/// The implementation shared by a normal allocation and an orphan-conflict
/// recovery. `unavailable_ipv4s` contains host-owned addresses that are not
/// represented by our SQLite lease rows, so it is checked alongside the
/// durable active-lease set.
fn allocate_excluding(
    tx: &Transaction<'_>,
    vm_id: Uuid,
    subnet: SubnetSpec,
    unavailable_ipv4s: &HashSet<Ipv4Addr>,
) -> Result<Lease, IpamError> {
    if has_active_lease(tx, vm_id)? {
        return Err(IpamError::AlreadyLeased { vm_id });
    }

    // Scanned against every active lease, not just this subnet's: two
    // MicroNetworks can never overlap (the helper's bridge overlap check
    // refuses that), so a globally-unique address is also unique here, and
    // keeping the check global means a subnet whose CIDR was somehow reused
    // still cannot hand out a duplicate.
    let taken_ips = active_ipv4s(tx)?;
    let ipv4 = subnet
        .host_addresses()
        .find(|candidate| !taken_ips.contains(candidate) && !unavailable_ipv4s.contains(candidate))
        .ok_or(IpamError::PoolExhausted {
            network: subnet.network,
            prefix: subnet.prefix,
        })?;

    let taken_macs = active_macs(tx)?;
    let mac = (0..MAX_MAC_ATTEMPTS)
        .map(|salt| derive_mac(vm_id, salt))
        .find(|candidate| !taken_macs.contains(candidate))
        .ok_or(IpamError::MacPoolExhausted)?;

    // Derived from the pair just chosen rather than scanned for: both modes
    // are a pure function of an address that is already unique in this
    // subnet, so the v6 address cannot collide either.
    let ipv6 = subnet.ipv6_for(ipv4, mac);

    tx.execute(
        "INSERT INTO network_leases (vm_id, ipv4, ipv6, mac, allocated_at, micro_network_id) \
         VALUES (?1, ?2, ?3, ?4, datetime('now'), ?5)",
        params![
            vm_id.to_string(),
            ipv4.to_string(),
            ipv6.map(|ipv6| ipv6.to_string()),
            mac.to_string(),
            subnet.micro_network_id.to_string(),
        ],
    )?;
    bump_lease_revision(tx)?;

    Ok(Lease {
        vm_id,
        ipv4,
        ipv6,
        mac,
    })
}

/// Whether any still-active lease belongs to `micro_network_id` — what makes
/// deleting a MicroNetwork that still has VMs in it refusable.
pub fn has_active_leases_in(
    conn: &rusqlite::Connection,
    micro_network_id: Uuid,
) -> Result<bool, IpamError> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM network_leases \
             WHERE micro_network_id = ?1 AND released_at IS NULL",
            params![micro_network_id.to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Release `vm_id`'s active lease. The row is kept with `released_at` set
/// rather than deleted, so the address/MAC free up for reuse while history
/// survives. Callers must only invoke this once VM cleanup (policy, TAP,
/// artifacts) has fully succeeded.
pub fn release(tx: &Transaction<'_>, vm_id: Uuid) -> Result<(), IpamError> {
    release_active(tx, vm_id)?;
    bump_lease_revision(tx)?;
    Ok(())
}

/// Marks the active lease released without changing the revision. A rotation
/// calls this before its replacement allocation, which performs the single
/// revision bump for the all-or-nothing lease change.
fn release_active(tx: &Transaction<'_>, vm_id: Uuid) -> Result<(), IpamError> {
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

/// Looks up `vm_id`'s current active lease, if it has one. Unlike
/// [`allocate`]/[`release`], this is a plain read with no need for a
/// `BEGIN IMMEDIATE` transaction.
pub fn active_lease(conn: &rusqlite::Connection, vm_id: Uuid) -> Result<Option<Lease>, IpamError> {
    conn.query_row(
        "SELECT ipv4, ipv6, mac FROM network_leases WHERE vm_id = ?1 AND released_at IS NULL",
        params![vm_id.to_string()],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
    )
    .optional()?
    .map(|(ipv4, ipv6, mac)| parse_lease(vm_id, ipv4, ipv6, mac))
    .transpose()
}

/// Every currently-active lease, for handing the DHCP helper a full
/// snapshot to render its host-reservation file from (see
/// [`current_revision`], sent alongside so a stale snapshot is never
/// applied out of order).
pub fn active_leases(conn: &rusqlite::Connection) -> Result<Vec<Lease>, IpamError> {
    let mut statement = conn
        .prepare("SELECT vm_id, ipv4, ipv6, mac FROM network_leases WHERE released_at IS NULL")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    rows.map(|row| {
        let (vm_id, ipv4, ipv6, mac) = row?;
        let vm_id = Uuid::parse_str(&vm_id).map_err(|_| IpamError::CorruptLease {
            vm_id: Uuid::nil(),
            reason: format!("stored vm_id {vm_id:?} is not a UUID"),
        })?;
        parse_lease(vm_id, ipv4, ipv6, mac)
    })
    .collect()
}

fn parse_lease(
    vm_id: Uuid,
    ipv4: String,
    ipv6: Option<String>,
    mac: String,
) -> Result<Lease, IpamError> {
    let ipv4 = ipv4.parse().map_err(|_| IpamError::CorruptLease {
        vm_id,
        reason: format!("stored ipv4 {ipv4:?} does not parse"),
    })?;
    let ipv6 = ipv6
        .map(|ipv6| {
            ipv6.parse::<Ipv6Addr>()
                .map_err(|_| IpamError::CorruptLease {
                    vm_id,
                    reason: format!("stored ipv6 {ipv6:?} does not parse"),
                })
        })
        .transpose()?;
    let mac = mac.parse().map_err(|_| IpamError::CorruptLease {
        vm_id,
        reason: format!("stored mac {mac:?} does not parse"),
    })?;
    Ok(Lease {
        vm_id,
        ipv4,
        ipv6,
        mac,
    })
}

/// The protocol's address-mode enum for the API-facing one a stored row
/// carries. A row with no stored mode predates dual-stack (or holds no
/// prefix at all), and SLAAC is the default either way.
pub fn protocol_address_mode(mode: Option<firecrab_api_types::Ipv6AddressMode>) -> Ipv6AddressMode {
    match mode {
        Some(firecrab_api_types::Ipv6AddressMode::Dhcpv6) => Ipv6AddressMode::Dhcpv6,
        _ => Ipv6AddressMode::Slaac,
    }
}

/// Unique-local prefixes masquerade (NAT66); every other accepted prefix
/// is forwarded untranslated. Shared by the create response and the list
/// path so the two cannot drift.
pub fn ipv6_egress_mode(ipv6: &MicroNetworkIpv6Spec) -> firecrab_api_types::Ipv6EgressMode {
    if ipv6.is_unique_local() {
        firecrab_api_types::Ipv6EgressMode::Nat66
    } else {
        firecrab_api_types::Ipv6EgressMode::Direct
    }
}

/// A per-host Unique Local `/64` for a MicroNetwork that was created without
/// an explicit prefix (RFC 4193: `fd` + 40 bits of host-specific global id,
/// then a 16-bit subnet id). Deterministic in both inputs, so a network keeps
/// the same prefix across restarts and two networks on one host — or the same
/// network on two hosts — never share one. ULA scope is what makes this
/// egress through NAT66 (`public-docs/networking.md`).
pub fn auto_ula_prefix(host_id: &str, micro_network_id: Uuid) -> MicroNetworkIpv6Spec {
    let global_id = Sha256::digest(host_id.as_bytes());
    let subnet_id = Sha256::digest(micro_network_id.as_bytes());
    let mut octets = [0_u8; 16];
    octets[0] = 0xfd;
    octets[1..6].copy_from_slice(&global_id[..5]);
    octets[6..8].copy_from_slice(&subnet_id[..2]);
    let network = Ipv6Addr::from(octets);
    MicroNetworkIpv6Spec {
        gateway: Ipv6Addr::from(u128::from(network) + 1),
        prefix: 64,
        address_mode: Ipv6AddressMode::Slaac,
    }
}

/// This host's stable identity for [`auto_ula_prefix`]. `/etc/machine-id` is
/// the canonical one; a host without it falls back to its hostname, so two
/// Firecrab hosts on one L2 still get different prefixes.
pub fn host_id() -> String {
    std::fs::read_to_string("/etc/machine-id")
        .map(|id| id.trim().to_owned())
        .ok()
        .filter(|id| !id.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .map(|name| name.trim().to_owned())
                .ok()
        })
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| "firecrab".to_owned())
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

/// Every IPv4 address currently leased. The network/gateway/broadcast
/// addresses need no entry here — [`SubnetSpec::host_addresses`] never
/// offers them for the subnet being allocated from in the first place.
fn active_ipv4s(tx: &Transaction<'_>) -> Result<HashSet<Ipv4Addr>, rusqlite::Error> {
    let mut statement = tx.prepare("SELECT ipv4 FROM network_leases WHERE released_at IS NULL")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut set = HashSet::new();
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
    use core::assert_matches;

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
    fn helper_spec_threads_internet_and_uplink() {
        let subnet = SubnetSpec::legacy_default_subnet(Uuid::from_u128(1));
        let auto = subnet.helper_spec(true, None);
        assert!(auto.internet_enabled);
        assert_eq!(auto.uplink, None);
        assert_eq!(auto.gateway, subnet.gateway());
        assert_eq!(auto.prefix, 24);

        let chosen = subnet.helper_spec(false, Some("eth1".to_owned()));
        assert!(!chosen.internet_enabled);
        assert_eq!(chosen.uplink.as_deref(), Some("eth1"));
    }

    #[test]
    fn parse_ipv6_masks_the_prefix_before_deriving_the_gateway() {
        let spec = SubnetSpec::parse_ipv6("fd00::5/64", Ipv6AddressMode::Slaac).unwrap();
        assert_eq!(spec.gateway, "fd00::1".parse::<Ipv6Addr>().unwrap());
        assert_eq!(
            spec.network_address(),
            "fd00::".parse::<Ipv6Addr>().unwrap()
        );

        let canonical = SubnetSpec::parse_ipv6("2001:db8:1::/64", Ipv6AddressMode::Dhcpv6).unwrap();
        assert_eq!(
            canonical.gateway,
            "2001:db8:1::1".parse::<Ipv6Addr>().unwrap()
        );
        assert_eq!(canonical.address_mode, Ipv6AddressMode::Dhcpv6);

        assert!(
            SubnetSpec::parse_ipv6(
                "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff/128",
                Ipv6AddressMode::Slaac
            )
            .is_none()
        );
        assert!(SubnetSpec::parse_ipv6("fd00::/129", Ipv6AddressMode::Slaac).is_none());
    }

    #[test]
    fn ipv6_egress_mode_follows_unique_local_vs_global() {
        let ula = SubnetSpec::parse_ipv6("fd00:1::/64", Ipv6AddressMode::Slaac).unwrap();
        assert_eq!(
            ipv6_egress_mode(&ula),
            firecrab_api_types::Ipv6EgressMode::Nat66
        );
        let gua = SubnetSpec::parse_ipv6("2001:db8:1::/64", Ipv6AddressMode::Slaac).unwrap();
        assert_eq!(
            ipv6_egress_mode(&gua),
            firecrab_api_types::Ipv6EgressMode::Direct
        );
    }

    fn dual_stack_subnet(id: u128, prefix6: &str, mode: Ipv6AddressMode) -> SubnetSpec {
        SubnetSpec {
            ipv6: Some(MicroNetworkIpv6Spec {
                gateway: format!("{prefix6}1").parse().unwrap(),
                prefix: 64,
                address_mode: mode,
            }),
            ..SubnetSpec::legacy_default_subnet(Uuid::from_u128(id))
        }
    }

    #[test]
    fn a_slaac_lease_carries_the_eui64_of_the_mac_it_was_given() {
        let mut conn = open();
        let tx = begin(&mut conn);
        let lease = allocate(
            &tx,
            Uuid::from_u128(7),
            dual_stack_subnet(1, "fd00:1::", Ipv6AddressMode::Slaac),
        )
        .unwrap();
        tx.commit().unwrap();

        // The guest configures exactly this address itself from the RA,
        // which is what lets the firewall pin it up front.
        assert_eq!(
            lease.ipv6,
            Some(slaac_address("fd00:1::".parse().unwrap(), lease.mac))
        );
        // ...and it survives a reload from the row.
        assert_eq!(
            active_lease(&conn, Uuid::from_u128(7)).unwrap(),
            Some(lease)
        );
    }

    #[test]
    fn a_dhcpv6_lease_mirrors_the_host_offset_of_its_v4_address() {
        let mut conn = open();
        let tx = begin(&mut conn);
        let lease = allocate(
            &tx,
            Uuid::from_u128(7),
            dual_stack_subnet(1, "fd00:2::", Ipv6AddressMode::Dhcpv6),
        )
        .unwrap();
        tx.commit().unwrap();

        // 172.30.0.2 is the first address handed out of the legacy /24, so
        // its reservation is the second address of the prefix.
        assert_eq!(lease.ipv4, "172.30.0.2".parse::<Ipv4Addr>().unwrap());
        assert_eq!(lease.ipv6, Some("fd00:2::2".parse().unwrap()));
    }

    #[test]
    fn an_ipv4_only_subnet_leases_no_v6_address() {
        let mut conn = open();
        let tx = begin(&mut conn);
        let lease = allocate(
            &tx,
            Uuid::from_u128(7),
            SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)),
        )
        .unwrap();
        tx.commit().unwrap();
        assert_eq!(lease.ipv6, None);
    }

    #[test]
    fn rotating_a_dual_stack_lease_keeps_an_ipv6_address() {
        let mut conn = open();
        let subnet = dual_stack_subnet(1, "fd00:9::", Ipv6AddressMode::Slaac);
        let tx = begin(&mut conn);
        let first = allocate(&tx, Uuid::from_u128(7), subnet).unwrap();
        tx.commit().unwrap();
        assert!(first.ipv6.is_some());

        let tx = begin(&mut conn);
        let rotated = rotate(&tx, Uuid::from_u128(7), subnet, &HashSet::new()).unwrap();
        tx.commit().unwrap();
        assert!(rotated.ipv6.is_some());
        assert_ne!(rotated.ipv4, first.ipv4);
    }

    #[test]
    fn releasing_a_dual_stack_lease_frees_both_families_at_once() {
        let mut conn = open();
        let subnet = dual_stack_subnet(1, "fd00:3::", Ipv6AddressMode::Dhcpv6);
        let tx = begin(&mut conn);
        let first = allocate(&tx, Uuid::from_u128(7), subnet).unwrap();
        tx.commit().unwrap();

        let tx = begin(&mut conn);
        release(&tx, Uuid::from_u128(7)).unwrap();
        tx.commit().unwrap();
        assert!(active_leases(&conn).unwrap().is_empty());

        // The next VM gets the same pair back — one row, one lifetime.
        let tx = begin(&mut conn);
        let second = allocate(&tx, Uuid::from_u128(8), subnet).unwrap();
        tx.commit().unwrap();
        assert_eq!(second.ipv4, first.ipv4);
        assert_eq!(second.ipv6, first.ipv6);
    }

    #[test]
    fn the_auto_ula_prefix_is_stable_per_host_and_distinct_per_network() {
        let host = "3f8a1c2b4d5e6f708192a3b4c5d6e7f8";
        let first = auto_ula_prefix(host, Uuid::from_u128(1));
        let second = auto_ula_prefix(host, Uuid::from_u128(2));

        // RFC 4193 Unique Local space, so egress is NAT66 by scope alone.
        assert!(first.is_unique_local());
        assert_eq!(first.prefix, 64);
        assert_eq!(first.address_mode, Ipv6AddressMode::Slaac);
        // Same host, same network: the same prefix every time it is asked.
        assert_eq!(first, auto_ula_prefix(host, Uuid::from_u128(1)));
        // Different networks on one host never collide...
        assert_ne!(first.network_address(), second.network_address());
        // ...and neither do two hosts.
        assert_ne!(
            first.network_address(),
            auto_ula_prefix("00112233445566778899aabbccddeeff", Uuid::from_u128(1))
                .network_address()
        );
    }

    #[test]
    fn lease_revision_bumps_on_both_allocate_and_release() {
        let mut conn = open();
        assert_eq!(current_revision(&conn).unwrap(), 0);

        let vm_id = Uuid::new_v4();
        let tx = begin(&mut conn);
        allocate(
            &tx,
            vm_id,
            SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)),
        )
        .unwrap();
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
        allocate(
            &tx,
            kept,
            SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)),
        )
        .unwrap();
        allocate(
            &tx,
            released,
            SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)),
        )
        .unwrap();
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
            let lease = allocate(
                &tx,
                Uuid::new_v4(),
                SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)),
            )
            .unwrap();
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
            let lease = allocate(
                &tx,
                Uuid::new_v4(),
                SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)),
            )
            .unwrap();
            tx.commit().unwrap();
            assert_ne!(lease.ipv4, LEGACY_DEFAULT_NETWORK);
            assert_ne!(lease.ipv4, LEGACY_DEFAULT_GATEWAY);
            assert_ne!(
                lease.ipv4,
                SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)).broadcast()
            );
        }
    }

    #[test]
    fn active_lease_reports_a_corrupt_stored_ipv4() {
        let mut conn = open();
        let vm_id = Uuid::new_v4();
        let tx = begin(&mut conn);
        allocate(
            &tx,
            vm_id,
            SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)),
        )
        .unwrap();
        tx.commit().unwrap();

        conn.execute(
            "UPDATE network_leases SET ipv4 = 'not-an-ip' WHERE vm_id = ?1",
            params![vm_id.to_string()],
        )
        .unwrap();

        let result = active_lease(&conn, vm_id);
        assert_matches!(result, Err(IpamError::CorruptLease { .. }));
    }

    #[test]
    fn pool_exhaustion_is_reported_once_all_253_hosts_are_leased() {
        let mut conn = open();
        for _ in 0..253 {
            let tx = begin(&mut conn);
            allocate(
                &tx,
                Uuid::new_v4(),
                SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)),
            )
            .unwrap();
            tx.commit().unwrap();
        }

        let tx = begin(&mut conn);
        let result = allocate(
            &tx,
            Uuid::new_v4(),
            SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)),
        );
        assert_matches!(result, Err(IpamError::PoolExhausted { .. }));
    }

    #[test]
    fn same_vm_cannot_hold_two_active_leases() {
        let mut conn = open();
        let vm_id = Uuid::new_v4();
        let tx = begin(&mut conn);
        allocate(
            &tx,
            vm_id,
            SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)),
        )
        .unwrap();
        tx.commit().unwrap();

        let tx = begin(&mut conn);
        assert_matches!(allocate(&tx, vm_id, SubnetSpec::legacy_default_subnet(Uuid::from_u128(1))),
            Err(IpamError::AlreadyLeased { vm_id: leased }) if leased == vm_id);
    }

    #[test]
    fn release_then_reallocate_reuses_the_freed_address() {
        let mut conn = open();
        let first_vm = Uuid::new_v4();
        let tx = begin(&mut conn);
        let first_lease = allocate(
            &tx,
            first_vm,
            SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)),
        )
        .unwrap();
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
        let second_lease = allocate(
            &tx,
            second_vm,
            SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)),
        )
        .unwrap();
        tx.commit().unwrap();
        assert_eq!(second_lease.ipv4, first_lease.ipv4);
    }

    #[test]
    fn rotating_a_conflicted_lease_skips_the_host_owned_ip_and_keeps_history() {
        let mut conn = open();
        let vm_id = Uuid::new_v4();
        let subnet = SubnetSpec::legacy_default_subnet(Uuid::from_u128(1));

        let tx = begin(&mut conn);
        let original = allocate(&tx, vm_id, subnet).unwrap();
        tx.commit().unwrap();

        let tx = begin(&mut conn);
        let replacement = rotate(&tx, vm_id, subnet, &HashSet::from([original.ipv4])).unwrap();
        tx.commit().unwrap();

        assert_eq!(original.ipv4, Ipv4Addr::new(172, 30, 0, 2));
        assert_eq!(replacement.ipv4, Ipv4Addr::new(172, 30, 0, 3));
        assert_eq!(active_lease(&conn, vm_id).unwrap(), Some(replacement));
        assert_eq!(current_revision(&conn).unwrap(), 2);

        let history_count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM network_leases WHERE vm_id = ?1",
                params![vm_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(history_count, 2, "the displaced lease remains auditable");
    }

    #[test]
    fn failed_rotation_rolls_back_and_keeps_the_original_active_lease() {
        let mut conn = open();
        let vm_id = Uuid::new_v4();
        let subnet = SubnetSpec {
            micro_network_id: Uuid::from_u128(1),
            network: Ipv4Addr::new(172, 30, 0, 0),
            prefix: 29,
            ipv6: None,
        };

        let tx = begin(&mut conn);
        let original = allocate(&tx, vm_id, subnet).unwrap();
        tx.commit().unwrap();

        // /29 offers .2 through .6. Mark every candidate as still owned by
        // host state so no replacement is possible.
        let unavailable = (2..=6)
            .map(|last_octet| Ipv4Addr::new(172, 30, 0, last_octet))
            .collect();
        let tx = begin(&mut conn);
        let result = rotate(&tx, vm_id, subnet, &unavailable);
        assert_matches!(result, Err(IpamError::PoolExhausted { .. }));
        drop(tx); // no commit: release_active must roll back too.

        assert_eq!(active_lease(&conn, vm_id).unwrap(), Some(original));
        assert_eq!(current_revision(&conn).unwrap(), 1);
    }

    #[test]
    fn releasing_a_vm_without_a_lease_fails() {
        let mut conn = open();
        let tx = begin(&mut conn);
        let result = release(&tx, Uuid::new_v4());
        assert_matches!(result, Err(IpamError::NotLeased { .. }));
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
        let lease = allocate(
            &tx,
            vm_id,
            SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)),
        )
        .unwrap();
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
        let result = allocate(
            &tx,
            vm_id,
            SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)),
        );
        assert_matches!(result, Err(IpamError::MacPoolExhausted));
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

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::PROTOCOL_VERSION;

/// Prefix for every Firecrab-owned TAP interface name. TAP interface names
/// are bounded by IFNAMSIZ (16 bytes incl. NUL): `fct` + 12 hex of
/// sha256(vm_id) = 15 chars. The prefix is distinct from MicroNetwork bridge
/// names so rules and diagnostics can identify VM-facing interfaces.
pub const TAP_PREFIX: &str = "fct";

/// The deterministic TAP interface name for a VM. Both `firecrab-api` (to
/// reference it in the Firecracker config) and `firecrab-net-helper` (to
/// create/attach/delete the real device, and to name nftables objects)
/// derive the same name from the same `vm_id` — the API never gets to pass
/// the helper an arbitrary interface name.
pub fn tap_name(vm_id: Uuid) -> String {
    let digest = Sha256::digest(vm_id.as_bytes());
    let mut name = String::from(TAP_PREFIX);
    for byte in &digest[..6] {
        name.push_str(&format!("{byte:02x}"));
    }
    name
}

/// Prefix for a MicroNetwork's deterministic bridge interface name.
pub const MICRO_NETWORK_BRIDGE_PREFIX: &str = "mnb";

/// The deterministic bridge interface name for a MicroNetwork (mirrors
/// [`tap_name`]'s construction and its reasoning — the helper derives every
/// interface name itself from a trusted id, never from a name string the API
/// supplies).
pub fn micro_network_bridge_name(micro_network_id: Uuid) -> String {
    let digest = Sha256::digest(micro_network_id.as_bytes());
    let mut name = String::from(MICRO_NETWORK_BRIDGE_PREFIX);
    for byte in &digest[..6] {
        name.push_str(&format!("{byte:02x}"));
    }
    name
}

/// The deterministic guest hostname for a VM: `fc-` plus 12 hex digits of
/// `sha256(vm_id)` (same construction as [`tap_name`], so two different
/// `vm_id`s can't collide just because they happen to share high-order
/// bits — unlike truncating the UUID's own text form directly). Derived
/// the same way by both sides so the API never has to hand the helper an
/// arbitrary, user-influenced hostname string to embed in the DHCP
/// reservation file.
pub fn guest_hostname(vm_id: Uuid) -> String {
    let digest = Sha256::digest(vm_id.as_bytes());
    let mut hostname = String::from("fc-");
    for byte in &digest[..6] {
        hostname.push_str(&format!("{byte:02x}"));
    }
    hostname
}

/// MAC address in `aa:bb:cc:dd:ee:ff` form; serialized as that string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacAddr(pub [u8; 6]);

/// Returned by [`MacAddr`]'s `FromStr` impl for malformed input.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("MAC address must be six ':'-separated hex octets")]
pub struct MacAddrParseError;

impl fmt::Display for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{g:02x}")
    }
}

impl FromStr for MacAddr {
    type Err = MacAddrParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut octets = [0_u8; 6];
        let mut parts = text.split(':');
        for octet in &mut octets {
            let part = parts.next().ok_or(MacAddrParseError)?;
            if part.len() != 2 {
                return Err(MacAddrParseError);
            }
            *octet = u8::from_str_radix(part, 16).map_err(|_| MacAddrParseError)?;
        }
        if parts.next().is_some() {
            return Err(MacAddrParseError);
        }
        Ok(Self(octets))
    }
}

impl Serialize for MacAddr {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for MacAddr {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// The complete privileged surface. Interface names, CIDRs, or nftables text
/// are deliberately absent: the helper derives all of those from its own
/// root-owned configuration and the VM UUID.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum NetworkRequest {
    /// Idempotently ensure the shared bridge/subnet/gateway exist.
    EnsureBridge,
    /// Idempotently ensure a MicroNetwork's own bridge/subnet/gateway exist
    /// (`public-docs/networking.md`). The interface name is derived from
    /// `micro_network_id` (see [`micro_network_bridge_name`]), never taken
    /// as a string from the API — only the numeric gateway/prefix, which
    /// carry no shell/nftables injection surface, cross the boundary
    /// (mirrors `ApplyVmPolicy`'s `ipv4` field).
    EnsureMicroNetworkBridge {
        /// The MicroNetwork this bridge belongs to.
        micro_network_id: Uuid,
        /// The bridge's own address on its subnet (the MicroNetwork's
        /// implicit router, same role as the default network's gateway).
        gateway: Ipv4Addr,
        /// CIDR prefix length of the MicroNetwork's subnet.
        prefix: u8,
        /// The network's IPv6 plan, or `None` for an IPv4-only network
        /// (also what an older API, which has no v6 concept, sends).
        #[serde(default)]
        ipv6: Option<MicroNetworkIpv6Spec>,
    },
    /// Removes a MicroNetwork's bridge; a no-op if it's already gone.
    RemoveMicroNetworkBridge {
        /// The MicroNetwork whose bridge should be removed.
        micro_network_id: Uuid,
    },
    /// Idempotently (re)apply the owned nftables tables. `micro_networks`
    /// is the full current set (may be empty), so the helper can render one
    /// NAT/dispatch rule per network and default-deny traffic routed between
    /// them. There is no implicit default network outside this list.
    EnsureFirewall {
        /// Every MicroNetwork that currently exists.
        micro_networks: Vec<MicroNetworkSpec>,
        /// Complete desired policy snapshot for VMs whose host networking is
        /// active. Rebuilding the shared tables and these policies in one nft
        /// transaction removes orphaned entries without interrupting policies
        /// that still belong to a live VM. Defaults to empty for requests from
        /// an older API binary.
        #[serde(default)]
        vm_policies: Vec<VmPolicySpec>,
    },
    /// Create and attach a TAP device for a starting VM.
    CreateTap {
        /// The VM the TAP belongs to.
        vm_id: Uuid,
        /// The MicroNetwork whose bridge to attach to.
        micro_network_id: Uuid,
    },
    /// Remove a VM's TAP device.
    DeleteTap {
        /// The VM the TAP belongs to.
        vm_id: Uuid,
    },
    /// Apply per-VM firewall/anti-spoofing rules for its lease.
    ApplyVmPolicy {
        /// The VM the policy applies to.
        vm_id: Uuid,
        /// The VM's allocated IPv4 address.
        ipv4: Ipv4Addr,
        /// The VM's allocated IPv6 address, when its MicroNetwork is
        /// dual-stack. Absent means IPv4-only, as before this field existed.
        #[serde(default)]
        ipv6: Option<Ipv6Addr>,
        /// The VM's Firecracker guest MAC.
        mac: MacAddr,
        /// ID resolved against the helper's allowlist; never a raw CIDR.
        egress_policy: String,
        /// Whether host SSH access should be permitted for this VM.
        allow_host_ssh: bool,
        /// Inbound port forwarding rules (DNAT).
        #[serde(default)]
        port_forwards: Vec<PortForwardSpec>,
    },
    /// Remove a VM's firewall/anti-spoofing rules.
    RemoveVmPolicy {
        /// The VM whose policy should be removed.
        vm_id: Uuid,
    },
    /// Replace the DHCP host-reservation file with this full snapshot of
    /// every currently-active lease, then reload. `revision` must be
    /// monotonically increasing (see `Store::lease_revision`); a snapshot
    /// older than the last one the helper applied is ignored rather than
    /// clobbering a newer one that arrived out of order.
    SyncDhcpLeases {
        /// Lease generation this snapshot reflects.
        revision: u64,
        /// Every currently-active lease.
        leases: Vec<DhcpLeaseEntry>,
        /// Every MicroNetwork that currently exists, so dnsmasq can serve
        /// each one's bridge. Empty means no Firecrab DHCP interfaces.
        micro_networks: Vec<MicroNetworkSpec>,
    },
    /// Install an already-downloaded host bundle over this host's binaries and
    /// restart both services (`firecrab update --apply`).
    ///
    /// Nothing in this request is taken on trust (same reasoning as
    /// `validate_prefix` and the `egress_policy` allowlist lookup):
    /// * `sha256` is re-verified by the helper from its own open file
    ///   descriptor, so a file swapped after the caller hashed it cannot be
    ///   installed;
    /// * `layout` is compared against the layout the helper derives from its
    ///   *own* `PREFIX`/`FIRECRAB_LIBDIR`, and any difference is rejected — it
    ///   is a cross-check that the caller agrees about this host, never a
    ///   destination the helper will write to on request;
    /// * every entry the bundle unpacks must be a regular file or a directory,
    ///   so no symlink can redirect the helper's `chown`/`chmod`/`rename`.
    ApplySelfUpdate {
        /// Absolute path to the downloaded `firecrab-host-<arch>-<libc>.tar.gz`.
        tarball_path: PathBuf,
        /// Lowercase hex SHA-256 the caller read out of the release's `SHA256SUMS`.
        sha256: String,
        /// Where this host's install put its binaries and assets.
        layout: InstallLayout,
    },
}

/// One MicroNetwork's host-facing parameters. The helper derives the bridge
/// name from `micro_network_id` (see [`micro_network_bridge_name`]) and the
/// subnet from `gateway`/`prefix`, so those never arrive as spliceable text.
/// Optional [`Self::uplink`] is the one host interface name the API may send;
/// the helper re-validates it before any nftables use. Omitted means the
/// host's default-route iface (today's single-uplink behavior).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MicroNetworkSpec {
    /// The MicroNetwork this describes.
    pub micro_network_id: Uuid,
    /// Its gateway address (the bridge's own address on its subnet).
    pub gateway: Ipv4Addr,
    /// Its subnet's CIDR prefix length.
    pub prefix: u8,
    /// Whether this network's VMs may reach anything outside Firecrab
    /// (AWS's "is an internet gateway attached to this VPC"). `false`
    /// withholds both the masquerade rule and the forward permission, so
    /// nothing leaves the network at L3 — DHCP/DNS from its own gateway are
    /// unaffected, being host-local rather than forwarded. Defaults to `true`
    /// when absent so an older API, which has no such concept, keeps the
    /// behavior every network had before this field existed.
    #[serde(default = "internet_enabled_default")]
    pub internet_enabled: bool,
    /// Host NIC this network should egress through. `None` (absent on the
    /// wire, or explicit null) keeps auto-detect. Not a CIDR and never a
    /// Firecrab-owned `fct*`/`mnb*` name; the helper is the trust boundary.
    #[serde(default)]
    pub uplink: Option<String>,
    /// This network's IPv6 plan, alongside — never instead of — its IPv4
    /// subnet. `None` (absent on the wire, which is all an older API can
    /// send) keeps the network IPv4-only.
    #[serde(default)]
    pub ipv6: Option<MicroNetworkIpv6Spec>,
}

/// Serde default for [`MicroNetworkSpec::internet_enabled`].
fn internet_enabled_default() -> bool {
    true
}

impl MicroNetworkSpec {
    /// Network (base) address of this MicroNetwork's subnet.
    pub fn network_address(&self) -> Ipv4Addr {
        let mask = if self.prefix == 0 {
            0
        } else {
            u32::MAX << (32 - u32::from(self.prefix.min(32)))
        };
        Ipv4Addr::from(u32::from(self.gateway) & mask)
    }

    /// The subnet in `<network>/<prefix>` CIDR notation, built from typed
    /// values rather than any caller-supplied text.
    pub fn subnet_cidr(&self) -> String {
        format!("{}/{}", self.network_address(), self.prefix)
    }

    /// The deterministic name of this MicroNetwork's bridge interface.
    pub fn bridge_name(&self) -> String {
        micro_network_bridge_name(self.micro_network_id)
    }

    /// Whether `ip` sits in this network's subnet. Used to pick the NAT
    /// uplink (and DNAT `iifname`) for a VM from its leased address.
    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        let prefix = self.prefix.min(32);
        let mask = if prefix == 0 {
            0
        } else {
            u32::MAX << (32 - u32::from(prefix))
        };
        (u32::from(ip) & mask) == u32::from(self.network_address())
    }
}

/// How guests in a dual-stack MicroNetwork get their IPv6 address.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Ipv6AddressMode {
    /// Router advertisements only: the guest builds its own address as the
    /// EUI-64 of its MAC (see [`slaac_address`]), which is exactly the
    /// address the API stored for it.
    #[default]
    Slaac,
    /// Stateful DHCPv6: the address comes from a per-VM reservation, the
    /// same way the IPv4 address does.
    Dhcpv6,
}

impl fmt::Display for Ipv6AddressMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Slaac => write!(f, "slaac"),
            Self::Dhcpv6 => write!(f, "dhcpv6"),
        }
    }
}

/// A MicroNetwork's IPv6 plan. Like its IPv4 counterpart in
/// [`MicroNetworkSpec`], the prefix is derived from `gateway`/`prefix`
/// rather than sent as text, so nothing spliceable crosses the boundary.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct MicroNetworkIpv6Spec {
    /// The bridge's own IPv6 address, which guests use as their router.
    pub gateway: Ipv6Addr,
    /// CIDR prefix length of the network's IPv6 prefix.
    pub prefix: u8,
    /// How guests obtain an address inside that prefix.
    #[serde(default)]
    pub address_mode: Ipv6AddressMode,
}

impl MicroNetworkIpv6Spec {
    /// Prefix (base) address of this network's IPv6 block.
    pub fn network_address(&self) -> Ipv6Addr {
        Ipv6Addr::from(u128::from(self.gateway) & self.mask())
    }

    /// The block in `<prefix>/<length>` CIDR notation, built from typed
    /// values rather than any caller-supplied text.
    pub fn subnet_cidr(&self) -> String {
        format!("{}/{}", self.network_address(), self.prefix)
    }

    /// Whether `address` sits inside this network's prefix.
    pub fn contains(&self, address: Ipv6Addr) -> bool {
        (u128::from(address) & self.mask()) == u128::from(self.network_address())
    }

    /// Whether two IPv6 plans share any address, the v6 counterpart of
    /// [`crate::network::MicroNetworkSpec`]'s IPv4 overlap check.
    pub fn overlaps(&self, other: &Self) -> bool {
        self.contains(other.network_address()) || other.contains(self.network_address())
    }

    /// Whether this prefix is a Unique Local Address block (`fc00::/7`).
    /// ULA space is not routable off-host, so it egresses through NAT66;
    /// a global prefix is forwarded untranslated instead. The egress mode
    /// follows from the prefix's scope — there is no separate toggle.
    pub fn is_unique_local(&self) -> bool {
        (self.network_address().octets()[0] & 0xfe) == 0xfc
    }

    /// Whether this prefix can back a MicroNetwork: Unique Local (`fc00::/7`)
    /// or global unicast (`2000::/3`). Other reserved ranges — link-local,
    /// multicast, deprecated site-local, discard-only, NAT64 — cannot.
    pub fn is_routable_scope(&self) -> bool {
        self.is_unique_local() || Self::is_global_unicast(self.network_address())
    }

    fn is_global_unicast(addr: Ipv6Addr) -> bool {
        addr.segments()[0] & 0xe000 == 0x2000
    }

    fn mask(&self) -> u128 {
        u128::MAX
            .checked_shl(128 - u32::from(self.prefix.min(128)))
            .unwrap_or(0)
    }
}

/// The address a SLAAC guest configures for itself: `prefix`'s network bits
/// followed by the EUI-64 interface identifier of `mac` (the MAC split
/// around `ff:fe` with its U/L bit flipped). The API stores this so the
/// firewall can pin it the way it pins the IPv4 lease, instead of waiting to
/// learn whatever address the guest happens to pick.
pub fn slaac_address(prefix: Ipv6Addr, mac: MacAddr) -> Ipv6Addr {
    let [a, b, c, d, e, f] = mac.0;
    let interface_id = u64::from_be_bytes([a ^ 0x02, b, c, 0xff, 0xfe, d, e, f]);
    let network = u128::from(prefix) & (u128::MAX << 64);
    Ipv6Addr::from(network | u128::from(interface_id))
}

/// One inbound port forwarding rule specification for [`NetworkRequest::ApplyVmPolicy`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct PortForwardSpec {
    /// Host port (1–65535).
    pub host_port: u16,
    /// Guest port inside the VM (1–65535).
    pub guest_port: u16,
    /// Protocol ("tcp" or "udp").
    pub protocol: String,
}

/// One VM policy in the full snapshot carried by
/// [`NetworkRequest::EnsureFirewall`]. This deliberately has the same typed
/// fields as [`NetworkRequest::ApplyVmPolicy`]; the helper still resolves the
/// egress id against its own allowlist before rendering any host rules.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VmPolicySpec {
    /// The VM this policy applies to.
    pub vm_id: Uuid,
    /// The VM's allocated IPv4 address.
    pub ipv4: Ipv4Addr,
    /// The VM's allocated IPv6 address, when its MicroNetwork is dual-stack.
    #[serde(default)]
    pub ipv6: Option<Ipv6Addr>,
    /// The VM's Firecracker guest MAC.
    pub mac: MacAddr,
    /// ID resolved against the helper's allowlist; never a raw CIDR.
    pub egress_policy: String,
    /// Whether host SSH access should be permitted for this VM.
    pub allow_host_ssh: bool,
    /// Inbound port forwarding rules (DNAT).
    #[serde(default)]
    pub port_forwards: Vec<PortForwardSpec>,
}

/// One VM's reservation for [`NetworkRequest::SyncDhcpLeases`]. No hostname
/// field: the helper derives it itself via [`guest_hostname`], the same way
/// it derives TAP names, rather than trusting a string the API supplies.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct DhcpLeaseEntry {
    /// The VM this reservation belongs to.
    pub vm_id: Uuid,
    /// Its allocated IPv4 address.
    pub ipv4: Ipv4Addr,
    /// Its allocated IPv6 address, when its MicroNetwork is dual-stack.
    /// Only DHCPv6 networks need it in a reservation; a SLAAC guest derives
    /// the same address itself from its MAC (see [`slaac_address`]).
    #[serde(default)]
    pub ipv6: Option<Ipv6Addr>,
    /// Its Firecracker guest MAC.
    pub mac: MacAddr,
}

/// The install layout a self-update writes into: `install.sh`'s `$PREFIX/bin`,
/// `$LIBDIR` (`$PREFIX/lib/firecrab`) and `$SHAREDIR` (`$PREFIX/share/firecrab`).
///
/// The (unprivileged) CLI resolves these exactly the way `firecrab info` does
/// and sends them here, but the helper does **not** write where they point: it
/// re-derives the same three paths from its own `PREFIX`/`FIRECRAB_LIBDIR`
/// (exported by `packaging/systemd/firecrab-net-helper.service`) and rejects
/// the request unless the two agree byte-for-byte. So this field is a
/// "do we both mean the same host?" cross-check, not a destination — a caller
/// that made it past the socket's uid allowlist still cannot point a
/// root-owned binary swap at a directory of its choosing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallLayout {
    /// Receives the `firecrab` CLI (`$PREFIX/bin`).
    pub bindir: PathBuf,
    /// Receives `firecrab-api` and `firecrab-net-helper` (`$LIBDIR`).
    pub libdir: PathBuf,
    /// Receives the dashboard assets under `dashboard/` (`$SHAREDIR`).
    pub sharedir: PathBuf,
}

/// A [`NetworkRequest`] tagged with protocol version and a correlation id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkRequestEnvelope {
    /// Sender's [`crate::PROTOCOL_VERSION`].
    pub version: u16,
    /// Correlates this request with its response.
    pub request_id: Uuid,
    /// The actual request payload.
    pub request: NetworkRequest,
}

impl NetworkRequestEnvelope {
    /// Wraps `request` with the current protocol version.
    pub fn new(request_id: Uuid, request: NetworkRequest) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request_id,
            request,
        }
    }
}

/// Reasons a [`NetworkRequest`] can fail, sent back over the wire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum HelperFailure {
    /// Request envelope's version doesn't match the helper's.
    #[error("helper only speaks protocol version {supported}")]
    UnsupportedVersion {
        /// The version the helper actually supports.
        supported: u16,
    },
    /// The requested operation exists but has no handler yet.
    #[error("operation is not implemented yet")]
    UnsupportedOperation,
    /// Request failed validation before touching any host state.
    #[error("request rejected: {detail}")]
    InvalidRequest {
        /// Human-readable rejection reason.
        detail: String,
    },
    /// Request was valid but applying it failed.
    #[error("helper internal failure: {detail}")]
    Internal {
        /// Human-readable failure detail.
        detail: String,
    },
    /// The bundle on disk didn't match the checksum the request carried, so
    /// nothing was extracted and no binary was replaced.
    #[error("update bundle checksum mismatch: expected {expected}, got {actual}")]
    UpdateChecksumMismatch {
        /// SHA-256 the request carried.
        expected: String,
        /// SHA-256 the helper computed from the file it opened.
        actual: String,
    },
    /// Extraction or the binary swap failed after validation passed.
    #[error("update apply failed: {detail}")]
    UpdateApplyFailed {
        /// Flattened error chain (same shape as `Internal`'s `detail`).
        detail: String,
        /// Whether every replaced binary was restored to its pre-update copy.
        /// `false` means the install is in a mixed state and needs `install.sh`.
        restored: bool,
    },
}

/// Response to a [`NetworkRequestEnvelope`], echoing its correlation id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkResponseEnvelope {
    /// Responder's [`crate::PROTOCOL_VERSION`].
    pub version: u16,
    /// Matches the request's `request_id`.
    pub request_id: Uuid,
    /// Outcome of processing the request.
    pub result: Result<(), HelperFailure>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_addr_round_trips_through_text_and_json() {
        let mac: MacAddr = "02:fc:0a:1b:2c:3d".parse().expect("parse mac");
        assert_eq!(mac.to_string(), "02:fc:0a:1b:2c:3d");

        let json = serde_json::to_string(&mac).expect("serialize");
        assert_eq!(json, "\"02:fc:0a:1b:2c:3d\"");
        assert_eq!(
            serde_json::from_str::<MacAddr>(&json).expect("deserialize"),
            mac
        );
    }

    #[test]
    fn tap_name_is_deterministic_and_within_ifnamsiz() {
        let vm = Uuid::from_u128(0x1234);
        assert_eq!(tap_name(vm), tap_name(vm));
        assert!(tap_name(vm).len() <= 15, "{}", tap_name(vm));
        assert!(tap_name(vm).starts_with(TAP_PREFIX));
        assert_ne!(tap_name(vm), tap_name(Uuid::from_u128(0x1235)));
    }

    #[test]
    fn micro_network_bridge_name_is_deterministic_and_within_ifnamsiz() {
        let network = Uuid::from_u128(0x1234);
        assert_eq!(
            micro_network_bridge_name(network),
            micro_network_bridge_name(network)
        );
        assert!(
            micro_network_bridge_name(network).len() <= 15,
            "{}",
            micro_network_bridge_name(network)
        );
        assert!(micro_network_bridge_name(network).starts_with(MICRO_NETWORK_BRIDGE_PREFIX));
        assert_ne!(
            micro_network_bridge_name(network),
            micro_network_bridge_name(Uuid::from_u128(0x1235))
        );
    }

    #[test]
    fn guest_hostname_is_deterministic_and_distinct_per_vm() {
        let vm = Uuid::from_u128(0x1234);
        assert_eq!(guest_hostname(vm), guest_hostname(vm));
        assert!(guest_hostname(vm).starts_with("fc-"));
        assert_ne!(guest_hostname(vm), guest_hostname(Uuid::from_u128(0x1235)));
    }

    #[test]
    fn malformed_mac_addrs_are_rejected() {
        for text in [
            "",
            "02:fc",
            "02:fc:0a:1b:2c:3d:4e",
            "02:fc:0a:1b:2c:zz",
            "2:fc:0a:1b:2c:3d",
        ] {
            assert_eq!(text.parse::<MacAddr>(), Err(MacAddrParseError), "{text}");
        }
    }

    #[test]
    fn requests_serialize_with_snake_case_operation_tags() {
        let json = serde_json::to_value(NetworkRequest::CreateTap {
            vm_id: Uuid::nil(),
            micro_network_id: Uuid::nil(),
        })
        .unwrap();
        assert_eq!(json["operation"], "create_tap");

        let envelope = NetworkRequestEnvelope::new(Uuid::nil(), NetworkRequest::EnsureBridge);
        assert_eq!(envelope.version, PROTOCOL_VERSION);
    }

    #[test]
    fn older_ensure_firewall_requests_default_to_an_empty_policy_snapshot() {
        let request: NetworkRequest =
            serde_json::from_str(r#"{"operation":"ensure_firewall","micro_networks":[]}"#)
                .expect("deserialize an older API request");
        assert_eq!(
            request,
            NetworkRequest::EnsureFirewall {
                micro_networks: Vec::new(),
                vm_policies: Vec::new(),
            }
        );
    }

    #[test]
    fn sync_dhcp_leases_serializes_with_its_operation_tag() {
        let request = NetworkRequest::SyncDhcpLeases {
            revision: 3,
            leases: vec![DhcpLeaseEntry {
                vm_id: Uuid::nil(),
                ipv4: "172.30.0.5".parse().unwrap(),
                ipv6: None,
                mac: "02:fc:00:00:00:05".parse().unwrap(),
            }],
            micro_networks: vec![MicroNetworkSpec {
                micro_network_id: Uuid::from_u128(0x1234),
                gateway: "172.31.0.1".parse().unwrap(),
                prefix: 24,
                internet_enabled: true,
                uplink: None,
                ipv6: None,
            }],
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["operation"], "sync_dhcp_leases");
        assert_eq!(json["revision"], 3);
        assert_eq!(
            serde_json::from_value::<NetworkRequest>(json).unwrap(),
            request
        );
    }

    #[test]
    fn micro_network_spec_derives_its_subnet_from_gateway_and_prefix() {
        let spec = MicroNetworkSpec {
            micro_network_id: Uuid::from_u128(0x1234),
            gateway: "172.31.5.1".parse().unwrap(),
            prefix: 24,
            internet_enabled: true,
            uplink: None,
            ipv6: None,
        };
        assert_eq!(
            spec.network_address(),
            "172.31.5.0".parse::<Ipv4Addr>().unwrap()
        );
        assert_eq!(spec.subnet_cidr(), "172.31.5.0/24");
        assert_eq!(
            spec.bridge_name(),
            micro_network_bridge_name(spec.micro_network_id)
        );
        assert!(spec.contains("172.31.5.42".parse().unwrap()));
        assert!(!spec.contains("172.32.5.42".parse().unwrap()));

        // A /16 masks off the third octet too.
        let wide = MicroNetworkSpec {
            prefix: 16,
            ..spec.clone()
        };
        assert_eq!(wide.subnet_cidr(), "172.31.0.0/16");
        assert!(wide.contains("172.31.99.1".parse().unwrap()));
        assert!(!wide.contains("172.32.0.1".parse().unwrap()));

        // A /0 mask is 0, so every address is in the subnet.
        let everywhere = MicroNetworkSpec { prefix: 0, ..spec };
        assert!(everywhere.contains("0.0.0.0".parse().unwrap()));
        assert!(everywhere.contains("255.255.255.255".parse().unwrap()));
    }

    #[test]
    fn a_spec_without_internet_enabled_keeps_the_pre_toggle_behavior() {
        // An API built before the toggle existed sends no such field, and
        // every network it knows about is on the internet.
        let spec: MicroNetworkSpec = serde_json::from_value(serde_json::json!({
            "micro_network_id": Uuid::nil(),
            "gateway": "172.31.0.1",
            "prefix": 24,
        }))
        .expect("a spec without the field must still deserialize");
        assert!(spec.internet_enabled);
    }

    #[test]
    fn a_spec_without_uplink_stays_on_auto() {
        // An older API has no per-network uplink and must keep today's
        // single detect_uplink() default. Do not bump PROTOCOL_VERSION.
        let spec: MicroNetworkSpec = serde_json::from_value(serde_json::json!({
            "micro_network_id": Uuid::nil(),
            "gateway": "172.31.0.1",
            "prefix": 24,
        }))
        .expect("a spec without uplink must still deserialize");
        assert_eq!(spec.uplink, None);
    }

    #[test]
    fn ensure_micro_network_bridge_serializes_with_its_operation_tag() {
        let request = NetworkRequest::EnsureMicroNetworkBridge {
            micro_network_id: Uuid::nil(),
            gateway: "172.31.0.1".parse().unwrap(),
            prefix: 24,
            ipv6: None,
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["operation"], "ensure_micro_network_bridge");
        assert_eq!(json["prefix"], 24);
        assert_eq!(
            serde_json::from_value::<NetworkRequest>(json).unwrap(),
            request
        );
    }

    #[test]
    fn response_result_round_trips() {
        let failure = NetworkResponseEnvelope {
            version: PROTOCOL_VERSION,
            request_id: Uuid::nil(),
            result: Err(HelperFailure::UnsupportedVersion { supported: 1 }),
        };

        let json = serde_json::to_string(&failure).expect("serialize");
        assert_eq!(
            serde_json::from_str::<NetworkResponseEnvelope>(&json).expect("deserialize"),
            failure
        );
    }

    #[test]
    fn apply_self_update_serializes_with_its_operation_tag() {
        let request = NetworkRequest::ApplySelfUpdate {
            tarball_path: PathBuf::from(
                "/var/lib/firecrab/updates/abc/firecrab-host-x86_64-gnu.tar.gz",
            ),
            sha256: "a".repeat(64),
            layout: InstallLayout {
                bindir: PathBuf::from("/usr/local/bin"),
                libdir: PathBuf::from("/usr/local/lib/firecrab"),
                sharedir: PathBuf::from("/usr/local/share/firecrab"),
            },
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["operation"], "apply_self_update");
        assert_eq!(json["layout"]["libdir"], "/usr/local/lib/firecrab");
        assert_eq!(
            serde_json::from_value::<NetworkRequest>(json).unwrap(),
            request
        );
    }

    #[test]
    fn install_layout_round_trips() {
        let layout = InstallLayout {
            bindir: PathBuf::from("/opt/fc/bin"),
            libdir: PathBuf::from("/opt/fc/lib/firecrab"),
            sharedir: PathBuf::from("/opt/fc/share/firecrab"),
        };
        let json = serde_json::to_string(&layout).expect("serialize");
        assert_eq!(
            serde_json::from_str::<InstallLayout>(&json).expect("deserialize"),
            layout
        );
    }

    #[test]
    fn update_failures_round_trip() {
        for failure in [
            HelperFailure::UpdateChecksumMismatch {
                expected: "b".repeat(64),
                actual: "c".repeat(64),
            },
            HelperFailure::UpdateApplyFailed {
                detail: "failed to apply the bundle: No space left on device".to_owned(),
                restored: true,
            },
        ] {
            let envelope = NetworkResponseEnvelope {
                version: PROTOCOL_VERSION,
                request_id: Uuid::nil(),
                result: Err(failure.clone()),
            };
            let json = serde_json::to_value(&envelope).expect("serialize");
            let code = json["result"]["Err"]["code"].as_str().expect("a code tag");
            assert!(
                code == "update_checksum_mismatch" || code == "update_apply_failed",
                "unexpected wire code {code}"
            );
            assert_eq!(
                serde_json::from_value::<NetworkResponseEnvelope>(json).expect("deserialize"),
                envelope
            );
        }
    }

    #[test]
    fn a_spec_without_ipv6_stays_ipv4_only() {
        // An API built before dual-stack existed sends no v6 fields; the
        // helper must keep rendering exactly today's IPv4-only host state.
        let spec: MicroNetworkSpec = serde_json::from_value(serde_json::json!({
            "micro_network_id": Uuid::nil(),
            "gateway": "172.31.0.1",
            "prefix": 24,
        }))
        .expect("a spec without ipv6 must still deserialize");
        assert_eq!(spec.ipv6, None);
    }

    #[test]
    fn micro_network_ipv6_spec_derives_its_prefix_from_gateway_and_length() {
        let spec = MicroNetworkIpv6Spec {
            gateway: "fd00:1234:5678:9abc::1".parse().unwrap(),
            prefix: 64,
            address_mode: Ipv6AddressMode::Slaac,
        };
        assert_eq!(
            spec.network_address(),
            "fd00:1234:5678:9abc::".parse::<Ipv6Addr>().unwrap()
        );
        assert_eq!(spec.subnet_cidr(), "fd00:1234:5678:9abc::/64");
        assert!(spec.contains("fd00:1234:5678:9abc::42".parse().unwrap()));
        assert!(!spec.contains("fd00:1234:5678:9abd::42".parse().unwrap()));
    }

    #[test]
    fn egress_mode_follows_the_prefix_scope() {
        // ULA (fc00::/7) is not routable off-host, so it needs NAT66; a GUA
        // is publicly routable and must be forwarded untranslated.
        let ula = MicroNetworkIpv6Spec {
            gateway: "fd00::1".parse().unwrap(),
            prefix: 64,
            address_mode: Ipv6AddressMode::Slaac,
        };
        assert!(ula.is_unique_local());

        let gua = MicroNetworkIpv6Spec {
            gateway: "2001:db8:1::1".parse().unwrap(),
            prefix: 64,
            address_mode: Ipv6AddressMode::Slaac,
        };
        assert!(!gua.is_unique_local());
        assert!(ula.is_routable_scope());
        assert!(gua.is_routable_scope());

        for gateway in ["fec0::1", "100::1", "64:ff9b::1", "fe80::1", "::1", "::"] {
            let reserved = MicroNetworkIpv6Spec {
                gateway: gateway.parse().unwrap(),
                prefix: 64,
                address_mode: Ipv6AddressMode::Slaac,
            };
            assert!(
                !reserved.is_routable_scope(),
                "{gateway} is not unique-local or global unicast"
            );
        }
    }

    #[test]
    fn slaac_address_is_the_eui64_of_the_guest_mac() {
        // EUI-64: split the MAC around ff:fe and flip the U/L bit, so the
        // address the API stores is the one the guest configures itself.
        let mac: MacAddr = "02:fc:0a:1b:2c:3d".parse().unwrap();
        let prefix: Ipv6Addr = "fd00:1234:5678:9abc::".parse().unwrap();
        assert_eq!(
            slaac_address(prefix, mac),
            "fd00:1234:5678:9abc:fc:aff:fe1b:2c3d"
                .parse::<Ipv6Addr>()
                .unwrap()
        );
    }

    #[test]
    fn ipv6_address_modes_use_their_wire_names() {
        assert_eq!(
            serde_json::to_value(Ipv6AddressMode::Slaac).unwrap(),
            serde_json::json!("slaac")
        );
        assert_eq!(
            serde_json::to_value(Ipv6AddressMode::Dhcpv6).unwrap(),
            serde_json::json!("dhcpv6")
        );
        assert_eq!(Ipv6AddressMode::default(), Ipv6AddressMode::Slaac);
    }

    #[test]
    fn vm_policies_and_leases_carry_an_optional_ipv6() {
        let policy = VmPolicySpec {
            vm_id: Uuid::nil(),
            ipv4: "172.31.0.5".parse().unwrap(),
            ipv6: Some("fd00::5".parse().unwrap()),
            mac: "02:fc:00:00:00:05".parse().unwrap(),
            egress_policy: "internet".to_owned(),
            allow_host_ssh: false,
            port_forwards: Vec::new(),
        };
        let json = serde_json::to_value(&policy).unwrap();
        assert_eq!(json["ipv6"], "fd00::5");
        assert_eq!(
            serde_json::from_value::<VmPolicySpec>(json).unwrap(),
            policy
        );

        // An older API omits the field entirely.
        let legacy: VmPolicySpec = serde_json::from_value(serde_json::json!({
            "vm_id": Uuid::nil(),
            "ipv4": "172.31.0.5",
            "mac": "02:fc:00:00:00:05",
            "egress_policy": "internet",
            "allow_host_ssh": false,
        }))
        .expect("a policy without ipv6 must still deserialize");
        assert_eq!(legacy.ipv6, None);

        let lease: DhcpLeaseEntry = serde_json::from_value(serde_json::json!({
            "vm_id": Uuid::nil(),
            "ipv4": "172.31.0.5",
            "mac": "02:fc:00:00:00:05",
        }))
        .expect("a lease without ipv6 must still deserialize");
        assert_eq!(lease.ipv6, None);
    }

    #[test]
    fn ensure_micro_network_bridge_carries_the_ipv6_gateway() {
        let request = NetworkRequest::EnsureMicroNetworkBridge {
            micro_network_id: Uuid::nil(),
            gateway: "172.31.0.1".parse().unwrap(),
            prefix: 24,
            ipv6: Some(MicroNetworkIpv6Spec {
                gateway: "fd00::1".parse().unwrap(),
                prefix: 64,
                address_mode: Ipv6AddressMode::Dhcpv6,
            }),
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["ipv6"]["gateway"], "fd00::1");
        assert_eq!(json["ipv6"]["address_mode"], "dhcpv6");
        assert_eq!(
            serde_json::from_value::<NetworkRequest>(json).unwrap(),
            request
        );
    }

    #[test]
    fn protocol_version_is_unchanged_by_the_self_update_request() {
        // Adding a request variant is additive: existing fields are
        // untouched, so an older peer keeps parsing every request it already
        // knew. Bumping this would make a freshly swapped API talk to a
        // not-yet-restarted helper and get UnsupportedVersion for the few
        // seconds between the swap and the restart. Do not bump.
        assert_eq!(PROTOCOL_VERSION, 2);
    }
}

use std::fmt;
use std::net::Ipv4Addr;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::PROTOCOL_VERSION;

/// Prefix for every Firecrab-owned TAP interface name. TAP interface names
/// are bounded by IFNAMSIZ (16 bytes incl. NUL): `fct` + 12 hex of
/// sha256(vm_id) = 15 chars. The prefix is distinct from the bridge name
/// (`fcbr0`) so an east-west wildcard (`fct*`) never matches the bridge
/// itself.
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
}

/// One MicroNetwork's host-facing parameters. Deliberately carries no
/// interface name or CIDR text: the helper derives the bridge name from
/// `micro_network_id` (see [`micro_network_bridge_name`]) and the subnet from
/// `gateway`/`prefix`, so nothing the API sends is ever spliced into an
/// `ip link` or nftables argument as-is.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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

/// One VM's reservation for [`NetworkRequest::SyncDhcpLeases`]. No hostname
/// field: the helper derives it itself via [`guest_hostname`], the same way
/// it derives TAP names, rather than trusting a string the API supplies.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct DhcpLeaseEntry {
    /// The VM this reservation belongs to.
    pub vm_id: Uuid,
    /// Its allocated IPv4 address.
    pub ipv4: Ipv4Addr,
    /// Its Firecracker guest MAC.
    pub mac: MacAddr,
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
    fn sync_dhcp_leases_serializes_with_its_operation_tag() {
        let request = NetworkRequest::SyncDhcpLeases {
            revision: 3,
            leases: vec![DhcpLeaseEntry {
                vm_id: Uuid::nil(),
                ipv4: "172.30.0.5".parse().unwrap(),
                mac: "02:fc:00:00:00:05".parse().unwrap(),
            }],
            micro_networks: vec![MicroNetworkSpec {
                micro_network_id: Uuid::from_u128(0x1234),
                gateway: "172.31.0.1".parse().unwrap(),
                prefix: 24,
                internet_enabled: true,
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

        // A /16 masks off the third octet too.
        let wide = MicroNetworkSpec { prefix: 16, ..spec };
        assert_eq!(wide.subnet_cidr(), "172.31.0.0/16");
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
    fn ensure_micro_network_bridge_serializes_with_its_operation_tag() {
        let request = NetworkRequest::EnsureMicroNetworkBridge {
            micro_network_id: Uuid::nil(),
            gateway: "172.31.0.1".parse().unwrap(),
            prefix: 24,
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
}

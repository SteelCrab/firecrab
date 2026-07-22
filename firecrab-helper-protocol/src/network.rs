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
    /// Idempotently (re)apply the owned nftables tables.
    EnsureFirewall,
    /// Create and attach a TAP device for a starting VM.
    CreateTap {
        /// The VM the TAP belongs to.
        vm_id: Uuid,
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
    },
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
        let json = serde_json::to_value(NetworkRequest::CreateTap { vm_id: Uuid::nil() }).unwrap();
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

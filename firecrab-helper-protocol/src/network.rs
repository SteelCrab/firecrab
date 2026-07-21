use std::fmt;
use std::net::Ipv4Addr;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::PROTOCOL_VERSION;

/// MAC address in `aa:bb:cc:dd:ee:ff` form; serialized as that string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacAddr(pub [u8; 6]);

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
    EnsureBridge,
    EnsureFirewall,
    CreateTap {
        vm_id: Uuid,
    },
    DeleteTap {
        vm_id: Uuid,
    },
    ApplyVmPolicy {
        vm_id: Uuid,
        ipv4: Ipv4Addr,
        mac: MacAddr,
        /// ID resolved against the helper's allowlist; never a raw CIDR.
        egress_policy: String,
        allow_host_ssh: bool,
    },
    RemoveVmPolicy {
        vm_id: Uuid,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkRequestEnvelope {
    pub version: u16,
    pub request_id: Uuid,
    pub request: NetworkRequest,
}

impl NetworkRequestEnvelope {
    pub fn new(request_id: Uuid, request: NetworkRequest) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request_id,
            request,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum HelperFailure {
    #[error("helper only speaks protocol version {supported}")]
    UnsupportedVersion { supported: u16 },
    #[error("operation is not implemented yet")]
    UnsupportedOperation,
    #[error("request rejected: {detail}")]
    InvalidRequest { detail: String },
    #[error("helper internal failure: {detail}")]
    Internal { detail: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkResponseEnvelope {
    pub version: u16,
    pub request_id: Uuid,
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
        assert_eq!(serde_json::from_str::<MacAddr>(&json).expect("deserialize"), mac);
    }

    #[test]
    fn malformed_mac_addrs_are_rejected() {
        for text in ["", "02:fc", "02:fc:0a:1b:2c:3d:4e", "02:fc:0a:1b:2c:zz", "2:fc:0a:1b:2c:3d"] {
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

//! DNS posture is separate from routed Internet access and defaults to legacy behavior.
use serde::{Deserialize, Serialize};

/// Resolver behavior for a MicroNetwork; private networks never forward external queries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MicroNetworkDnsMode {
    /// Preserve the existing gateway resolver and its host-configured upstream servers.
    #[default]
    LegacyForward,
    /// Use a separate resolver/cache with local records and no upstream servers.
    /// Local names are shared within the private group, not a tenant secrecy boundary.
    PrivateOnly,
}

impl MicroNetworkDnsMode {
    /// Stable database/wire spelling, independent of Rust variant names.
    pub fn id(self) -> &'static str {
        match self {
            Self::LegacyForward => "legacy_forward",
            Self::PrivateOnly => "private_only",
        }
    }

    /// Decode persisted posture strictly; corruption must not re-enable forwarding.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "legacy_forward" => Some(Self::LegacyForward),
            "private_only" => Some(Self::PrivateOnly),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_preserve_forwarding_but_invalid_postures_are_rejected() {
        assert_eq!(
            MicroNetworkDnsMode::default(),
            MicroNetworkDnsMode::LegacyForward
        );
        for mode in [
            MicroNetworkDnsMode::LegacyForward,
            MicroNetworkDnsMode::PrivateOnly,
        ] {
            assert_eq!(MicroNetworkDnsMode::parse(mode.id()), Some(mode));
            assert_eq!(serde_json::to_value(mode).unwrap(), mode.id());
        }
        assert_eq!(MicroNetworkDnsMode::parse("private"), None);
        assert!(serde_json::from_str::<MicroNetworkDnsMode>("\"private\"").is_err());
    }
}

//! Egress policy selection for VM network isolation.
//!
//! The API never hands the privileged helper a raw CIDR. It selects one of a
//! fixed set of named egress policy IDs; the helper resolves that ID against
//! its own root-owned configuration into concrete nftables rules. This module
//! is the API-side allowlist and default.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The egress policies the API may request for a VM. New policies are added
/// here and mirrored in the helper (`firecrab-net-helper/src/firewall.rs`);
/// the helper is the trust boundary and re-validates every ID it receives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressPolicy {
    /// Outbound to non-reserved destinations (the internet) is permitted.
    #[default]
    Internet,
    /// No outbound egress; only gateway-local services (DHCP/DNS) reach it.
    Isolated,
}

impl EgressPolicy {
    /// The wire ID carried in `NetworkRequest::ApplyVmPolicy.egress_policy`.
    pub fn id(self) -> &'static str {
        match self {
            EgressPolicy::Internet => "internet",
            EgressPolicy::Isolated => "isolated",
        }
    }
}

impl fmt::Display for EgressPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

/// Reject an unknown ID rather than silently defaulting, so a client typo
/// surfaces as a validation error instead of an unexpected network posture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownEgressPolicy(pub String);

impl FromStr for EgressPolicy {
    type Err = UnknownEgressPolicy;

    fn from_str(id: &str) -> Result<Self, Self::Err> {
        match id {
            "internet" => Ok(EgressPolicy::Internet),
            "isolated" => Ok(EgressPolicy::Isolated),
            other => Err(UnknownEgressPolicy(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_round_trips_through_from_str() {
        for policy in [EgressPolicy::Internet, EgressPolicy::Isolated] {
            assert_eq!(policy.id().parse(), Ok(policy));
        }
    }

    #[test]
    fn unknown_ids_are_rejected_not_defaulted() {
        assert_eq!(
            "wide-open".parse::<EgressPolicy>(),
            Err(UnknownEgressPolicy("wide-open".to_owned()))
        );
        // A CIDR must never be accepted as a policy ID.
        assert!("0.0.0.0/0".parse::<EgressPolicy>().is_err());
    }

    #[test]
    fn default_is_internet() {
        assert_eq!(EgressPolicy::default(), EgressPolicy::Internet);
    }

    #[test]
    fn serializes_as_its_snake_case_id() {
        let json = serde_json::to_string(&EgressPolicy::Isolated).unwrap();
        assert_eq!(json, "\"isolated\"");
    }
}

//! Structured security-group inputs. No user-provided nft syntax crosses here.

use std::net::Ipv4Addr;

use firecrab_api_types::{
    MAX_SECURITY_GROUP_RULES, PortProtocol, SecurityGroupDirection, SecurityGroupRule,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A validated IPv4 peer and destination-port range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityGroupRuleSpec {
    /// Connection side on which this allow rule is evaluated.
    pub direction: SecurityGroupDirection,
    /// TCP or UDP transport protocol to match.
    pub protocol: PortProtocol,
    /// First allowed destination port, inclusive and nonzero.
    pub port_from: u16,
    /// Last allowed destination port, inclusive and not below `port_from`.
    pub port_to: u16,
    /// Canonical IPv4 network address with all host bits cleared.
    pub peer_address: Ipv4Addr,
    /// IPv4 network prefix length from 0 through 32.
    pub peer_prefix: u8,
}

impl SecurityGroupRuleSpec {
    /// Parse and canonicalize a REST rule before sending it to the helper.
    pub fn from_api(rule: &SecurityGroupRule) -> Result<Self, String> {
        let (address, prefix) = rule
            .peer_cidr
            .split_once('/')
            .ok_or("peerCidr must be an IPv4 CIDR")?;
        let address: Ipv4Addr = address
            .parse()
            .map_err(|_| "peerCidr must be an IPv4 CIDR")?;
        let prefix: u8 = prefix.parse().map_err(|_| "peerCidr prefix must be 0–32")?;
        let mask = mask(prefix)?;
        let result = Self {
            direction: rule.direction,
            protocol: rule.protocol,
            port_from: rule.port_from,
            port_to: rule.port_to,
            peer_address: Ipv4Addr::from(u32::from(address) & mask),
            peer_prefix: prefix,
        };
        result.validate()?;
        Ok(result)
    }

    /// Revalidate even a typed request at the privileged boundary.
    pub fn validate(&self) -> Result<(), String> {
        let mask = mask(self.peer_prefix)?;
        if u32::from(self.peer_address) & mask != u32::from(self.peer_address) {
            return Err("peer address must be a canonical IPv4 network".into());
        }
        if self.port_from == 0 || self.port_from > self.port_to {
            return Err("port range must satisfy 1 <= portFrom <= portTo <= 65535".into());
        }
        Ok(())
    }
}

fn mask(prefix: u8) -> Result<u32, String> {
    if prefix > 32 {
        return Err("peerCidr prefix must be 0–32".into());
    }
    Ok(u32::MAX.checked_shl(u32::from(32 - prefix)).unwrap_or(0))
}

/// `None` means legacy behavior; `Some` with no rules means default-deny.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityGroupPolicySpec {
    /// Non-nil identifier of the selected security group.
    pub id: Uuid,
    /// Non-nil identifier of the MicroNetwork to which the group belongs.
    pub micro_network_id: Uuid,
    /// Complete allow list; an empty list retains default-deny behavior.
    pub rules: Vec<SecurityGroupRuleSpec>,
}

impl SecurityGroupPolicySpec {
    /// Reject nil identifiers, oversized allow lists, and invalid rules
    /// before the privileged helper renders or installs the policy.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_nil() || self.micro_network_id.is_nil() {
            return Err("security group and network ids must not be nil".into());
        }
        if self.rules.len() > MAX_SECURITY_GROUP_RULES {
            return Err(format!(
                "at most {MAX_SECURITY_GROUP_RULES} rules are allowed"
            ));
        }
        self.rules
            .iter()
            .try_for_each(SecurityGroupRuleSpec::validate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_reject_invalid_ports_and_ipv6_and_canonicalize_ipv4() {
        let mut rule = SecurityGroupRule {
            direction: SecurityGroupDirection::Ingress,
            protocol: PortProtocol::Tcp,
            port_from: 5432,
            port_to: 5432,
            peer_cidr: "172.30.0.42/24".into(),
        };
        assert_eq!(
            SecurityGroupRuleSpec::from_api(&rule).unwrap().peer_address,
            Ipv4Addr::new(172, 30, 0, 0)
        );
        rule.peer_cidr = "::/0".into();
        assert!(SecurityGroupRuleSpec::from_api(&rule).is_err());
        rule.peer_cidr = "0.0.0.0/0".into();
        rule.port_from = 0;
        assert!(SecurityGroupRuleSpec::from_api(&rule).is_err());
        rule.port_from = 6000;
        assert!(SecurityGroupRuleSpec::from_api(&rule).is_err());
    }

    #[test]
    fn policies_reject_nil_identifiers_and_excessive_rule_lists() {
        let rule = SecurityGroupRuleSpec {
            direction: SecurityGroupDirection::Egress,
            protocol: PortProtocol::Udp,
            port_from: 1,
            port_to: u16::MAX,
            peer_address: Ipv4Addr::UNSPECIFIED,
            peer_prefix: 0,
        };
        let mut policy = SecurityGroupPolicySpec {
            id: Uuid::from_u128(1),
            micro_network_id: Uuid::from_u128(2),
            rules: vec![rule; MAX_SECURITY_GROUP_RULES],
        };
        policy.validate().unwrap();
        policy.rules.push(policy.rules[0].clone());
        assert!(policy.validate().is_err());
        policy.rules.clear();
        policy.id = Uuid::nil();
        assert!(policy.validate().is_err());
        policy.id = Uuid::from_u128(1);
        policy.micro_network_id = Uuid::nil();
        assert!(policy.validate().is_err());
    }
}

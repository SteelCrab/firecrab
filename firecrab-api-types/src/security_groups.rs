//! Reusable, allow-only IPv4 firewall policies.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::PortProtocol;

/// Bound policy size at both the HTTP and privileged helper boundaries.
pub const MAX_SECURITY_GROUP_RULES: usize = 256;

/// The side of a VM connection on which a rule is evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecurityGroupDirection {
    /// Match new connections arriving at the VM from the peer CIDR.
    Ingress,
    /// Match new connections leaving the VM for the peer CIDR.
    Egress,
}

/// One allow rule. Ingress peers are sources; egress peers are destinations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SecurityGroupRule {
    /// Whether the peer CIDR names a connection source or destination.
    pub direction: SecurityGroupDirection,
    /// Transport protocol allowed by this rule: TCP or UDP.
    pub protocol: PortProtocol,
    /// First allowed destination port, inclusive; must be at least 1.
    pub port_from: u16,
    /// Last allowed destination port, inclusive; must not precede `port_from`.
    pub port_to: u16,
    /// IPv4 source CIDR for ingress, or destination CIDR for egress.
    pub peer_cidr: String,
}

/// Create a policy within one IPv4-only MicroNetwork.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateSecurityGroupRequest {
    /// Display name containing 1–64 bytes and no control characters.
    pub name: String,
    /// Existing IPv4-only MicroNetwork to which this policy is bound.
    pub micro_network_id: Uuid,
}

/// Rename a policy without changing its network or rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdateSecurityGroupRequest {
    /// Replacement display name; uses the same limits as group creation.
    pub name: String,
}

/// Atomically replace all rules. An empty list keeps default-deny active.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReplaceSecurityGroupRulesRequest {
    /// Complete allow list, bounded by [`MAX_SECURITY_GROUP_RULES`].
    /// An empty list denies new peer connections.
    pub rules: Vec<SecurityGroupRule>,
}

/// A stored policy and its complete rule list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecurityGroupResponse {
    /// Stable identifier used by VM and MicroService policy selections.
    pub id: Uuid,
    /// Stored display name.
    pub name: String,
    /// MicroNetwork that all VMs using this policy must join.
    pub micro_network_id: Uuid,
    /// Complete allow list with canonical IPv4 network CIDRs.
    pub rules: Vec<SecurityGroupRule>,
}

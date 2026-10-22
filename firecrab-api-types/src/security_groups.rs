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
    Ingress,
    Egress,
}

/// One allow rule. Ingress peers are sources; egress peers are destinations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SecurityGroupRule {
    pub direction: SecurityGroupDirection,
    pub protocol: PortProtocol,
    pub port_from: u16,
    pub port_to: u16,
    pub peer_cidr: String,
}

/// Create a policy within one IPv4-only MicroNetwork.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateSecurityGroupRequest {
    pub name: String,
    pub micro_network_id: Uuid,
}

/// Rename a policy without changing its network or rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdateSecurityGroupRequest {
    pub name: String,
}

/// Atomically replace all rules. An empty list keeps default-deny active.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReplaceSecurityGroupRulesRequest {
    pub rules: Vec<SecurityGroupRule>,
}

/// A stored policy and its complete rule list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecurityGroupResponse {
    pub id: Uuid,
    pub name: String,
    pub micro_network_id: Uuid,
    pub rules: Vec<SecurityGroupRule>,
}

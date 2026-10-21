//! Intent requests and stable identities for independent anonymous connectors.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    Config, Result, ServiceAction, UnitStatus, hostname, reserved_quick_host, validate_upstreams,
};

/// Maximum number of anonymous connectors, including the legacy singleton.
pub const MAX_QUICK_TUNNELS: usize = 16;
/// Stable dashboard identity for the pre-list anonymous connector.
pub const LEGACY_QUICK_TUNNEL_ID: Uuid = Uuid::from_u128(1);

/// Private Nginx virtual host reached only by this connector's Host override.
pub fn quick_tunnel_host(id: Uuid) -> String {
    format!("quick-{}.firecrab.internal", id.simple())
}

/// Independent systemd unit that owns one anonymous connector.
pub fn quick_tunnel_unit(id: Uuid) -> String {
    format!("firecrab-quick-tunnel-{}.service", id.simple())
}

/// App addresses or an existing Nginx host group to publish anonymously.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum QuickTunnelTarget {
    /// Distribute every incoming path between the selected app addresses.
    Apps {
        /// Distinct app IP:port addresses.
        upstreams: Vec<SocketAddr>,
    },
    /// Reuse every saved path rule belonging to one Nginx hostname.
    Routes {
        /// Existing literal hostname whose path rules remain locally managed.
        host: String,
    },
}

impl QuickTunnelTarget {
    fn validate(&self, listen: Option<SocketAddr>) -> Result<()> {
        match self {
            Self::Apps { upstreams } => validate_upstreams(upstreams, listen, "Quick Tunnel"),
            Self::Routes { host } if hostname(host) && !reserved_quick_host(host) => Ok(()),
            Self::Routes { .. } => {
                Err("select a valid existing Nginx hostname for Quick Tunnel".into())
            }
        }
    }
}

/// Persisted identity and routing selection for an anonymous connector.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuickTunnelEntry {
    /// Stable non-nil connector identifier.
    pub id: Uuid,
    /// User-facing connector name, at most 64 characters.
    pub name: String,
    /// Selected app addresses or existing Nginx hostname.
    pub target: QuickTunnelTarget,
}

impl QuickTunnelEntry {
    pub(super) fn validate(&self, config: &Config) -> Result<()> {
        validate_id(self.id)?;
        validate_name(&self.name)?;
        self.target.validate(Some(config.listen))?;
        if let QuickTunnelTarget::Routes { host } = &self.target
            && !config.routes.iter().any(|route| &route.host == host)
        {
            return Err(format!(
                "edit or delete Quick Tunnel '{}' before removing the last route for {host}",
                self.name
            )
            .into());
        }
        Ok(())
    }
}

/// Fresh state for one independent connector, including a normalized legacy row.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickTunnelSnapshot {
    /// Stable identifier used by edit, delete and service actions.
    pub id: Uuid,
    /// User-facing connector name.
    pub name: String,
    /// Persisted routing selection.
    pub target: QuickTunnelTarget,
    /// Current state of this connector's systemd unit.
    pub unit: UnitStatus,
    /// HTTPS URL from the unit's current invocation, absent while stopped or starting.
    pub public_url: Option<String>,
    /// Sanitized connector failure, absent while starting normally or connected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Dashboard save input; unrelated Pipe configuration is never submitted here.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuickTunnelSaveRequest {
    /// Existing connector identifier, or absent for a new connector.
    #[serde(default)]
    pub id: Option<Uuid>,
    /// User-facing name for this connector.
    pub name: String,
    /// App selection or existing Nginx host group.
    pub target: QuickTunnelTarget,
}

/// Bounded intent applied to the helper's latest saved Pipe configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    tag = "action",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum QuickTunnelRequest {
    /// Add or edit one connector without overwriting unrelated routes.
    Save {
        /// Existing connector identifier, or absent to create a random identifier.
        #[serde(default)]
        id: Option<Uuid>,
        /// User-facing connector name.
        name: String,
        /// Selected app addresses or saved Nginx host group.
        target: QuickTunnelTarget,
    },
    /// Stop and remove one connector while preserving other Pipe resources.
    Delete {
        /// Existing connector identifier.
        id: Uuid,
    },
    /// Change the runtime state of one configured connector.
    Control {
        /// Existing connector identifier.
        id: Uuid,
        /// Fixed lifecycle operation.
        service_action: ServiceAction,
    },
}

impl QuickTunnelRequest {
    /// Reject malformed intents before acquiring the privileged configuration lock.
    pub fn validate(&self) -> Result<()> {
        if serde_json::to_vec(self)?.len() > 48 * 1024 {
            return Err("Quick Tunnel request exceeds 48 KiB".into());
        }
        match self {
            Self::Save { id, name, target } => {
                if let Some(id) = id {
                    validate_id(*id)?;
                }
                validate_name(name)?;
                target.validate(None)
            }
            Self::Delete { id } | Self::Control { id, .. } => validate_id(*id),
        }
    }
}

fn validate_id(id: Uuid) -> Result<()> {
    if id.is_nil() {
        return Err("Quick Tunnel id must not be the nil UUID".into());
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    if name.trim().is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(
            "Quick Tunnel name must contain 1 to 64 characters without control characters".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "quick_tunnels_tests.rs"]
mod tests;

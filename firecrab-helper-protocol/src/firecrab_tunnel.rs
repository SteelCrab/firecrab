//! Stable Firecrab addresses and local target selections. Ownership secrets stay on the host.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Default production connection-service API, configured by the operator rather than dashboard users.
pub const ENDPOINT: &str = "https://tunnel.firecrab.dev";
/// Public suffix served by the Firecrab connection service.
pub const DOMAIN: &str = "firecrab.dev";
/// Maximum number of stable addresses on one managed host.
pub const MAX_ADDRESSES: usize = 16;

/// Reject reserved infrastructure names and require a single lowercase DNS label.
pub fn subdomain(value: &str) -> bool {
    (3..=48).contains(&value.len())
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        && ![
            "www",
            "api",
            "admin",
            "tunnel",
            "connect",
            "docs",
            "status",
            "mail",
            "smtp",
            "support",
            "dashboard",
            "cdn",
            "assets",
            "auth",
            "login",
            "account",
            "accounts",
        ]
        .contains(&value)
}

/// VM app addresses or a saved Nginx hostname to publish.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    /// Publish the selected VM apps through an automatically managed connector.
    Apps {
        /// Distinct app IP:port addresses.
        upstreams: Vec<std::net::SocketAddr>,
    },
    /// Read previously saved connector references without removing their connections.
    QuickTunnel {
        /// Stable connector identity.
        id: Uuid,
    },
    /// Publish all saved paths using an automatically managed connector.
    Nginx {
        /// Saved Nginx route hostname.
        host: String,
    },
}

/// Secret dashboard token. Debug output must never reveal its contents.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConnectionToken(pub String);

impl std::fmt::Debug for ConnectionToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ConnectionToken([REDACTED])")
    }
}

/// Accept only the opaque 64-character hexadecimal token format issued by the dashboard.
pub fn valid_token(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// One address change, applied to the helper's latest saved state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Connect a dashboard-issued address to a local resource.
    Connect {
        /// Ownership secret, stored only on the managed host.
        token: ConnectionToken,
        /// Selected local resource.
        target: Target,
    },
    /// Change an address already saved on this host.
    Save {
        /// Public DNS label.
        subdomain: String,
        /// Selected local resource.
        target: Target,
    },
    /// Disconnect an address and release its exclusively owned connector.
    Delete {
        /// Public DNS label.
        subdomain: String,
    },
}
impl Request {
    /// Validate bounded input before privileged work.
    pub fn validate(&self) -> Result<(), String> {
        if serde_json::to_vec(self).map_or(true, |bytes| bytes.len() > 4096) {
            return Err("Firecrab address request exceeds 4 KiB".into());
        }
        let name = match self {
            Self::Save { subdomain, .. } | Self::Delete { subdomain } => Some(subdomain),
            Self::Connect { token, .. } => {
                if !valid_token(&token.0) {
                    return Err("Enter a valid connection token from the Firecrab dashboard".into());
                }
                None
            }
        };
        if name.is_some_and(|name| !subdomain(name)) {
            return Err(
                "Use 3–48 lowercase letters, digits or hyphens; infrastructure names are reserved"
                    .into(),
            );
        }
        if let Self::Save { target, .. } | Self::Connect { target, .. } = self {
            match target {
                Target::Apps { upstreams } => {
                    super::pipe::validate_upstreams(upstreams, None, "Firecrab Tunnel")
                        .map_err(|error| error.to_string())?;
                }
                Target::QuickTunnel { .. } => {
                    return Err("Select VM apps or Nginx routes".into());
                }
                Target::Nginx { host } if !super::pipe::valid_route_hostname(host) => {
                    return Err("Select an existing Nginx hostname".into());
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Non-secret status returned through Pipe inspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Address {
    /// Reserved DNS label.
    pub subdomain: String,
    /// Stable public HTTPS address.
    pub public_url: String,
    /// User-selected local target.
    pub target: Target,
    /// Current source connector.
    pub connector_id: Uuid,
    /// Whether the last service update succeeded with a live target.
    pub connected: bool,
    /// Sanitized failure or pending status.
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_targets_reject_empty_invalid_and_duplicate_pools() {
        for values in [
            vec![],
            vec!["0.0.0.0:80"],
            vec!["172.30.36.2:0"],
            vec!["172.30.36.2:80", "172.30.36.2:80"],
        ] {
            let request = Request::Save {
                subdomain: "my-app".into(),
                target: Target::Apps {
                    upstreams: values
                        .into_iter()
                        .map(|value| value.parse().unwrap())
                        .collect(),
                },
            };
            assert!(request.validate().is_err());
        }
        let legacy = Target::QuickTunnel {
            id: Uuid::from_u128(2),
        };
        assert!(
            Request::Save {
                subdomain: "my-app".into(),
                target: legacy
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn connection_tokens_are_bounded_and_redacted_from_debug_output() {
        let token = "a".repeat(64);
        let request = Request::Connect {
            token: ConnectionToken(token.clone()),
            target: Target::Nginx {
                host: "app.test".into(),
            },
        };
        request.validate().unwrap();
        assert!(!format!("{request:?}").contains(&token));
        for invalid in [String::new(), "x".repeat(64), "a".repeat(65)] {
            assert!(
                Request::Connect {
                    token: ConnectionToken(invalid),
                    target: Target::Nginx {
                        host: "app.test".into()
                    }
                }
                .validate()
                .is_err()
            );
        }
    }

    #[test]
    fn names_and_target_intents_are_bounded_and_reject_registration_credentials() {
        for name in ["my-app", "abc", "api-service"] {
            assert!(subdomain(name));
        }
        for name in ["api", "www", "tunnel", "-app", "app-", "a.b", "UPPER", "ab"] {
            assert!(!subdomain(name));
        }
        let invalid = serde_json::json!({"action":"save","subdomain":"my-app","target":{"type":"nginx","host":"app.test"},"token":"secret"});
        assert!(serde_json::from_value::<Request>(invalid).is_err());
        for target in [
            Target::Apps {
                upstreams: vec!["172.30.36.2:80".parse().unwrap()],
            },
            Target::Nginx {
                host: "app.test".into(),
            },
        ] {
            Request::Save {
                subdomain: "my-app".into(),
                target,
            }
            .validate()
            .unwrap();
        }
        assert!(
            Request::Save {
                subdomain: "my-app".into(),
                target: Target::Nginx {
                    host: "bad;host".into()
                }
            }
            .validate()
            .is_err()
        );
    }
}

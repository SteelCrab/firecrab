//! Onboarding inputs. Account API tokens are transient and redacted in Debug.
use serde::{Deserialize, Serialize};

use crate::pipe::Config;

/// One-time account API token whose Debug representation never exposes its value.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApiToken(
    /// Raw submitted token, retained only for the current onboarding request.
    String,
);

impl std::fmt::Debug for ApiToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl ApiToken {
    /// Borrow the token without surrounding whitespace for HTTP authorization.
    pub fn value(&self) -> &str {
        self.0.trim()
    }

    /// Reject installation commands and values outside the API-token character set.
    pub fn validate(&self) -> Result<(), &'static str> {
        let value = self.value();
        if !(32..=1024).contains(&value.len())
            || !value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        {
            return Err("Enter a Cloudflare API token, not a tunnel token or installation command");
        }
        Ok(())
    }
}

/// Transient authorization used to list domains available for onboarding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessRequest {
    /// Scoped token provided by the user; never persisted in Pipe configuration.
    pub api_token: ApiToken,
}

/// Cloudflare account that owns a selectable DNS zone and the created tunnel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// Cloudflare's 32-character hexadecimal account identifier.
    pub id: String,
    /// Display name, optional in Cloudflare responses.
    #[serde(default)]
    pub name: String,
}

/// DNS domain returned by Cloudflare; only active, accessible zones are offered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Zone {
    /// Cloudflare's 32-character hexadecimal zone identifier.
    pub id: String,
    /// Domain name used to validate the requested public hostname.
    pub name: String,
    /// Activation state; onboarding requires the value `active`.
    pub status: String,
    /// Owning account in which the tunnel is provisioned.
    pub account: Account,
}

/// Check Cloudflare account, zone and DNS record identifier syntax.
pub fn identifier(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|c| c.is_ascii_hexdigit())
}

/// Inputs for creating a tunnel and DNS record, then applying local Pipe state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectRequest {
    /// One-time token authorized for the selected account and domain.
    pub api_token: ApiToken,
    /// Zone selected from the token's active domains.
    pub zone_id: String,
    /// Public hostname belonging to that zone and present in the route list.
    pub hostname: String,
    /// Desired local routes; an existing tunnel must be removed before connecting.
    pub config: Config,
}

impl ConnectRequest {
    /// Validate token, zone syntax, route membership and request size before setup.
    pub fn validate(&self) -> Result<(), String> {
        self.api_token.validate().map_err(str::to_owned)?;
        if !identifier(&self.zone_id) {
            return Err("Invalid Cloudflare domain identifier".into());
        }
        self.config.validate().map_err(|e| e.to_string())?;
        if self.config.tunnel.is_some() || self.config.quick_tunnel.is_some() {
            return Err("Disconnect the configured tunnel before creating a new connection".into());
        }
        if !self
            .config
            .routes
            .iter()
            .any(|route| route.host == self.hostname)
        {
            return Err("Add an app route for the public hostname before connecting".into());
        }
        if serde_json::to_vec(self).map_or(true, |bytes| bytes.len() > 48 * 1024) {
            return Err("Cloudflare request exceeds 48 KiB".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn named_onboarding_rejects_an_existing_quick_tunnel() {
        let request: ConnectRequest = serde_json::from_value(serde_json::json!({
            "apiToken": "scoped-api-token-01234567890123456789",
            "zoneId": "01234567890123456789012345678901",
            "hostname": "app.example.com",
            "config": {
                "routes": [{ "host": "app.example.com", "upstreams": ["172.30.1.2:80"] }],
                "quickTunnel": { "upstreams": ["172.30.1.2:80"] }
            }
        }))
        .unwrap();
        assert!(request.validate().unwrap_err().contains("Disconnect"));
    }

    #[test]
    fn named_onboarding_preserves_independent_quick_tunnels() {
        let request: ConnectRequest = serde_json::from_value(serde_json::json!({
            "apiToken": "scoped-api-token-01234567890123456789",
            "zoneId": "01234567890123456789012345678901",
            "hostname": "app.example.com",
            "config": {
                "routes": [{ "host": "app.example.com", "upstreams": ["172.30.1.2:80"] }],
                "quickTunnels": [{ "id":"00000000-0000-0000-0000-000000000002", "name":"App", "target":{"type":"apps","upstreams":["172.30.1.2:80"]} }]
            }
        })).unwrap();
        request.validate().unwrap();
    }

    #[test]
    fn api_tokens_are_redacted_and_commands_are_rejected() {
        let secret = "scoped-api-token-01234567890123456789";
        let request: AccessRequest =
            serde_json::from_value(serde_json::json!({"apiToken":secret})).unwrap();
        request.api_token.validate().unwrap();
        assert!(!format!("{request:?}").contains(secret));
        let invalid: AccessRequest = serde_json::from_value(
            serde_json::json!({"apiToken":"cloudflared tunnel run --token secret"}),
        )
        .unwrap();
        assert!(invalid.api_token.validate().is_err());
    }
}

#[cfg(test)]
mod stable_hostname_tests {
    #[test]
    fn named_hostname_requires_its_route_and_coexists_with_independent_quick_tunnels() {
        let mut config: crate::pipe::Config = serde_json::from_value(serde_json::json!({
            "routes":[{"host":"app.example.com","upstreams":["127.0.0.1:9000"]}],
            "tunnel":{"id":"00000000-0000-0000-0000-000000000009","credentialsFile":"credentials.json","hostname":"app.example.com"},
            "quickTunnels":[{"id":"00000000-0000-0000-0000-000000000002","name":"Preview","target":{"type":"routes","host":"app.example.com"}}]
        })).unwrap();
        config.validate().unwrap();
        config.quick_tunnels.clear();
        config.routes.clear();
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("Cloudflare public hostname")
        );
        config.tunnel.as_mut().unwrap().hostname = None;
        config.validate().unwrap();
    }
}

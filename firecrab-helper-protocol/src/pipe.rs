//! Shared Pipe manifests, engine configuration and privileged helper operations.

use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

/// Nginx Unix socket used to confirm that a route generation is serving requests.
pub const STATUS_SOCKET: &str = "/run/firecrab-ingress/status.sock";
/// Independent systemd unit that owns the Cloudflare connector.
pub const TUNNEL_UNIT: &str = "firecrab-tunnel.service";

/// Validation and manifest IO errors shared by CLI and helper callers.
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// First data-plane slice: HTTP on loopback, with public TLS at Cloudflare.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Unprivileged loopback listener reached by the tunnel connector.
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// Host and path rules served by the independent Nginx process.
    pub routes: Vec<Route>,
    /// Optional public connector; local routing also works without it.
    pub tunnel: Option<Tunnel>,
}

/// A literal host/path match and the app addresses that share its requests.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Route {
    /// Lowercase DNS hostname, without a wildcard or port.
    pub host: String,
    /// Absolute path matched exactly and as a slash-delimited subtree.
    #[serde(default = "default_path")]
    pub path_prefix: String,
    /// Distinct IP:port targets used by Nginx round-robin balancing.
    pub upstreams: Vec<SocketAddr>,
}

/// A locally managed Cloudflare tunnel and its source credential file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tunnel {
    /// Non-nil Cloudflare tunnel UUID, also required in the credentials.
    pub id: Uuid,
    /// Source credentials, copied into a root-only systemd credential file.
    pub credentials_file: PathBuf,
}

/// Default private HTTP endpoint for the public tunnel's origin traffic.
fn default_listen() -> SocketAddr {
    "127.0.0.1:8080".parse().unwrap()
}

/// Match every path when a route omits its path prefix.
fn default_path() -> String {
    "/".into()
}

/// Accept only literal lowercase DNS names safe for native engine configs.
fn hostname(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        })
}

/// Reject wildcard, interpolation and directive syntax in literal paths.
fn path(path: &str) -> bool {
    path.starts_with('/')
        && path.bytes().all(|c| {
            c.is_ascii_graphic()
                && !matches!(
                    c,
                    b'*' | b'?' | b'#' | b'\\' | b'{' | b'}' | b'%' | b'\"' | b'\'' | b';' | b'$'
                )
        })
}

impl Config {
    /// Read and validate this tunnel's credentials, or return none for local ingress.
    pub fn credentials(&self) -> Result<Option<Value>> {
        let Some(tunnel) = &self.tunnel else {
            return Ok(None);
        };
        let value: Value = serde_json::from_slice(&std::fs::read(&tunnel.credentials_file)?)?;
        self.validate_credentials(&value)?;
        Ok(Some(value))
    }

    /// Require a matching tunnel UUID and nonempty account tag and tunnel secret.
    pub fn validate_credentials(&self, value: &Value) -> Result<()> {
        let tunnel = self
            .tunnel
            .as_ref()
            .ok_or("provide tunnel configuration with credentials")?;
        let id = value
            .get("TunnelID")
            .and_then(Value::as_str)
            .and_then(|id| Uuid::parse_str(id).ok());
        if id != Some(tunnel.id)
            || ["AccountTag", "TunnelSecret"].iter().any(|key| {
                value
                    .get(key)
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
            })
        {
            return Err("tunnel credentials must contain AccountTag, TunnelSecret, and the matching TunnelID".into());
        }
        Ok(())
    }

    /// Load a validated manifest and resolve credentials relative to its directory.
    pub fn read(file: &Path) -> Result<Self> {
        let mut config: Self = serde_json::from_slice(&std::fs::read(file)?)?;
        if let Some(tunnel) = &mut config.tunnel
            && tunnel.credentials_file.is_relative()
        {
            tunnel.credentials_file = file
                .parent()
                .unwrap_or(Path::new("."))
                .join(&tunnel.credentials_file);
        }
        config.validate()?;
        Ok(config)
    }

    /// Reject unsafe engine syntax, public listeners and ambiguous or invalid routes.
    pub fn validate(&self) -> Result<()> {
        if serde_json::to_vec(self)?.len() > 48 * 1024 {
            return Err("pipe manifest exceeds 48 KiB".into());
        }
        if !self.listen.ip().is_loopback() || self.listen.port() < 1024 {
            return Err("listen must be a loopback IP and an unprivileged port (>= 1024); terminate public TLS at Cloudflare".into());
        }
        let mut matches = BTreeSet::new();
        for route in &self.routes {
            if !hostname(&route.host) {
                return Err(format!(
                    "invalid route host: {} (use a lowercase DNS name)",
                    route.host
                )
                .into());
            }
            if !path(&route.path_prefix) {
                return Err(format!("{}: paths must be literal absolute paths", route.host).into());
            }
            if !matches.insert((&route.host, route.path_prefix.trim_end_matches('/'))) {
                return Err(
                    format!("duplicate route: {} {}", route.host, route.path_prefix).into(),
                );
            }
            if route.upstreams.is_empty()
                || route.upstreams.iter().any(|address| {
                    address.port() == 0
                        || address.ip().is_unspecified()
                        || address.ip().is_multicast()
                        || *address == self.listen
                })
            {
                return Err(format!("{}: provide nonzero upstream IP:port addresses distinct from the ingress listener", route.host).into());
            }
            let unique: BTreeSet<_> = route.upstreams.iter().collect();
            if unique.len() != route.upstreams.len() {
                return Err(format!("{}: duplicate upstream", route.host).into());
            }
        }
        if self
            .tunnel
            .as_ref()
            .is_some_and(|tunnel| tunnel.id.is_nil())
        {
            return Err("tunnel.id must not be the nil UUID".into());
        }
        Ok(())
    }

    /// Hash only listener and routes so Nginx can acknowledge a graceful reload.
    pub fn generation(&self) -> String {
        let bytes =
            serde_json::to_vec(&(self.listen, &self.routes)).expect("serializable pipe routes");
        format!("{:x}", Sha256::digest(bytes))
    }

    /// Standalone config: never includes or edits the host's nginx.conf.
    pub fn nginx(&self) -> String {
        let mut text = format!(
            "worker_processes auto;\npid nginx.pid;\nerror_log stderr notice;\nworker_shutdown_timeout 5m;\nevents {{ worker_connections 1024; }}\nhttp {{\n  access_log off;\n  server_tokens off;\n  client_body_temp_path body;\n  proxy_temp_path proxy;\n  map $http_upgrade $fc_connection {{ default upgrade; '' ''; }}\n  map $http_x_forwarded_proto $fc_proto {{ default $scheme; https https; http http; }}\n  server {{ listen unix:{STATUS_SOCKET}; location / {{ return 200 \"{}\"; }} }}\n",
            self.generation()
        );
        let mut hosts: BTreeMap<&str, Vec<(usize, &Route)>> = BTreeMap::new();
        for (index, route) in self.routes.iter().enumerate() {
            text.push_str(&format!(
                "  upstream fc_route_{index} {{\n    zone fc_route_{index} 64k;\n"
            ));
            for address in &route.upstreams {
                text.push_str(&format!(
                    "    server {address} max_fails=1 fail_timeout=30s;\n"
                ));
            }
            text.push_str("    keepalive 16;\n  }\n");
            hosts.entry(&route.host).or_default().push((index, route));
        }
        text.push_str(&format!(
            "  server {{ listen {} default_server; server_name _; return 404; }}\n",
            self.listen
        ));
        for (host, routes) in hosts {
            text.push_str(&format!(
                "  server {{ listen {}; server_name {host};\n",
                self.listen
            ));
            let has_fallback = routes.iter().any(|(_, route)| route.path_prefix == "/");
            for (index, route) in routes {
                let prefix = route.path_prefix.trim_end_matches('/');
                let locations = if prefix.is_empty() {
                    vec!["/".to_owned()]
                } else {
                    vec![format!("= \"{prefix}\""), format!("^~ \"{prefix}/\"")]
                };
                for location in locations {
                    text.push_str(&format!("    location {location} {{\n      proxy_pass http://fc_route_{index};\n      proxy_http_version 1.1;\n      proxy_set_header Host $host;\n      proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;\n      proxy_set_header X-Forwarded-Proto $fc_proto;\n      proxy_set_header Upgrade $http_upgrade;\n      proxy_set_header Connection $fc_connection;\n      proxy_connect_timeout 2s;\n      proxy_read_timeout 30s;\n      proxy_next_upstream error timeout http_500 http_502 http_503 http_504;\n      proxy_next_upstream_timeout 3s;\n    }}\n"));
                }
            }
            if !has_fallback {
                text.push_str("    location / { return 404; }\n");
            }
            text.push_str("  }\n");
        }
        text.push_str("}\n");
        text
    }

    /// Render cloudflared ingress with systemd credentials and a final 404 rule.
    pub fn tunnel_config(&self) -> Option<Value> {
        self.tunnel.as_ref().map(|tunnel| {
            let hosts: BTreeSet<_> = self.routes.iter().map(|route| &route.host).collect();
            let mut ingress: Vec<_> = hosts
                .into_iter()
                .map(|host| json!({"hostname": host, "service": format!("http://{}", self.listen)}))
                .collect();
            ingress.push(json!({"service": "http_status:404"}));
            json!({
                "tunnel": tunnel.id,
                "credentials-file": format!("/run/credentials/{TUNNEL_UNIT}/credentials.json"),
                "ingress": ingress
            })
        })
    }
}

/// Desired Pipe state plus an optional replacement credential document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplyRequest {
    /// Complete manifest validated before privileged changes begin.
    pub config: Config,
    /// A supplied credential replaces the previous one; None retains it.
    pub credentials: Option<Value>,
}

/// Fixed privileged operations accepted by the helper; no arbitrary commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Operation {
    /// Read persisted configuration, engine availability and systemd state.
    Inspect,
    /// Apply a complete configuration with rollback on verification failure.
    Apply {
        /// Manifest and optional replacement tunnel credentials.
        request: ApplyRequest,
    },
    /// Create Cloudflare resources and verify the independent connector.
    ConnectCloudflare {
        /// Transient API token, selected zone and desired public route.
        request: crate::cloudflare::ConnectRequest,
    },
    /// Stop and disable both units while retaining their configuration.
    Disable,
}

/// Unmodified systemd state for one independent data-plane unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnitStatus {
    /// Full systemd unit name.
    pub unit: String,
    /// Whether systemd found and loaded the unit definition.
    pub load_state: String,
    /// High-level runtime state, such as active, inactive or failed.
    pub active_state: String,
    /// Detailed runtime state, such as running or dead.
    pub sub_state: String,
    /// Main process identifier, or zero when no main process is running.
    pub main_pid: u32,
}

/// Persisted Pipe configuration and live unit state returned to the dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Last installed manifest, absent before the first successful apply.
    pub config: Option<Config>,
    /// Ingress and tunnel unit states, independent of API process lifetime.
    pub units: Vec<UnitStatus>,
    /// Whether a usable Nginx executable exists on the managed host.
    pub nginx_available: bool,
    /// Whether the managed cloudflared executable exists on the host.
    pub cloudflared_available: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Config {
        serde_json::from_value(json!({"routes":[{"host":"app.example.com","upstreams":["127.0.0.1:9000","127.0.0.1:9001"]},{"host":"app.example.com","pathPrefix":"/api","upstreams":["127.0.0.1:9002"]}]})).unwrap()
    }
    #[test]
    fn configuration_preserves_paths_and_load_balances() {
        let config = config();
        config.validate().unwrap();
        let nginx = config.nginx();
        assert!(nginx.contains("location = \"/api\""));
        assert!(nginx.contains("location ^~ \"/api/\""));
        assert!(nginx.contains("proxy_pass http://fc_route_1;"));
        assert!(nginx.contains("server 127.0.0.1:9001 max_fails=1"));
        assert!(nginx.contains("default_server; server_name _; return 404"));
    }
    #[test]
    fn rejects_injection_and_unintended_listeners() {
        for host in [
            "*.example.com",
            "app.example.com:443",
            "App.example.com",
            "app..com",
            "app;return 200;",
        ] {
            let mut config = config();
            config.routes[0].host = host.into();
            assert!(config.validate().is_err());
        }
        for prefix in [
            "api",
            "/api*",
            "/api?x",
            "/api%2fsecret",
            "/a\";return 200;",
            "/a$host",
        ] {
            let mut config = config();
            config.routes[0].path_prefix = prefix.into();
            assert!(config.validate().is_err());
        }
        for listen in ["0.0.0.0:8080", "127.0.0.1:80"] {
            let mut config = config();
            config.listen = listen.parse().unwrap();
            assert!(config.validate().is_err());
        }
    }
}

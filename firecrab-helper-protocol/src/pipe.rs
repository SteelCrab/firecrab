//! Shared Pipe manifests, engine configuration and privileged helper operations.

use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

mod quick_tunnels;
pub use quick_tunnels::{
    LEGACY_QUICK_TUNNEL_ID, MAX_QUICK_TUNNELS, QuickTunnelEntry, QuickTunnelRequest,
    QuickTunnelSaveRequest, QuickTunnelSnapshot, QuickTunnelTarget, quick_tunnel_host,
    quick_tunnel_unit,
};

/// Nginx Unix socket used to confirm that a route generation is serving requests.
pub const STATUS_SOCKET: &str = "/run/firecrab-ingress/status.sock";
/// Independent systemd unit that owns the Cloudflare connector.
pub const TUNNEL_UNIT: &str = "firecrab-tunnel.service";
/// Internal Host header used only by the anonymous Cloudflare connector.
pub const QUICK_TUNNEL_HOST: &str = "quick.firecrab.internal";

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
    /// Optional anonymous connector with an assigned trycloudflare.com address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quick_tunnel: Option<QuickTunnel>,
    /// Independently named anonymous connectors.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quick_tunnels: Vec<QuickTunnelEntry>,
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
    /// Public DNS name provisioned during dashboard onboarding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
}

/// App addresses reached through an anonymous Cloudflare Quick Tunnel.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuickTunnel {
    /// Distinct app IP:port targets used by Nginx request distribution.
    pub upstreams: Vec<SocketAddr>,
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

/// Validate a user-selected saved Nginx hostname without allowing generated connector names.
pub fn valid_route_hostname(host: &str) -> bool {
    hostname(host) && !reserved_quick_host(host)
}

/// Reserve generated anonymous-connector hosts from editable local route groups.
fn reserved_quick_host(host: &str) -> bool {
    host == QUICK_TUNNEL_HOST
        || (host.starts_with("quick-") && host.ends_with(".firecrab.internal"))
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

/// Apply the same literal app-address rules to saved routes and anonymous access.
pub(crate) fn validate_upstreams(
    upstreams: &[SocketAddr],
    listen: Option<SocketAddr>,
    host: &str,
) -> Result<()> {
    if upstreams.is_empty()
        || upstreams.iter().any(|address| {
            address.port() == 0
                || address.ip().is_unspecified()
                || address.ip().is_multicast()
                || Some(*address) == listen
        })
    {
        return Err(format!(
            "{host}: provide nonzero upstream IP:port addresses distinct from the ingress listener"
        )
        .into());
    }
    let unique: BTreeSet<_> = upstreams.iter().collect();
    if unique.len() != upstreams.len() {
        return Err(format!("{host}: duplicate upstream").into());
    }
    Ok(())
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
        if self.tunnel.is_some() && self.quick_tunnel.is_some() {
            return Err("named and Quick Tunnel connections cannot be configured together".into());
        }
        if let Some(quick) = &self.quick_tunnel {
            validate_upstreams(&quick.upstreams, Some(self.listen), QUICK_TUNNEL_HOST)?;
        }
        if self.quick_tunnels.len() + usize::from(self.quick_tunnel.is_some()) > MAX_QUICK_TUNNELS {
            return Err("configure at most 16 Quick Tunnels".into());
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
            if reserved_quick_host(&route.host) {
                return Err(
                    "quick.firecrab.internal is reserved for Quick Tunnel connections".into(),
                );
            }
            if !path(&route.path_prefix) {
                return Err(format!("{}: paths must be literal absolute paths", route.host).into());
            }
            if !matches.insert((&route.host, route.path_prefix.trim_end_matches('/'))) {
                return Err(
                    format!("duplicate route: {} {}", route.host, route.path_prefix).into(),
                );
            }
            validate_upstreams(&route.upstreams, Some(self.listen), &route.host)?;
        }
        let mut ids = BTreeSet::new();
        if self.quick_tunnel.is_some() {
            ids.insert(LEGACY_QUICK_TUNNEL_ID);
        }
        for entry in &self.quick_tunnels {
            entry.validate(self)?;
            if !ids.insert(entry.id) {
                return Err(format!("duplicate Quick Tunnel id: {}", entry.id).into());
            }
        }
        if self
            .tunnel
            .as_ref()
            .is_some_and(|tunnel| tunnel.id.is_nil())
        {
            return Err("tunnel.id must not be the nil UUID".into());
        }
        if let Some(host) = self
            .tunnel
            .as_ref()
            .and_then(|tunnel| tunnel.hostname.as_ref())
            && (!hostname(host) || !self.routes.iter().any(|route| &route.host == host))
        {
            return Err(
                "keep a route for the Cloudflare public hostname, or disconnect its tunnel first"
                    .into(),
            );
        }
        Ok(())
    }

    /// Hash listener and all app targets so Nginx can acknowledge a graceful reload.
    pub fn generation(&self) -> String {
        // Preserve the generation returned by configs installed before Quick Tunnel support.
        let bytes = match (&self.quick_tunnel, self.quick_tunnels.is_empty()) {
            (None, true) => serde_json::to_vec(&(self.listen, &self.routes)),
            (Some(quick), true) => serde_json::to_vec(&(self.listen, &self.routes, quick)),
            (_, false) => serde_json::to_vec(&(
                self.listen,
                &self.routes,
                &self.quick_tunnel,
                &self.quick_tunnels,
            )),
        }
        .expect("serializable pipe routes");
        format!("{:x}", Sha256::digest(bytes))
    }

    /// Standalone config: never includes or edits the host's nginx.conf.
    pub fn nginx(&self) -> String {
        let mut text = format!(
            "worker_processes auto;\npid nginx.pid;\nerror_log stderr notice;\nworker_shutdown_timeout 5m;\nevents {{ worker_connections 1024; }}\nhttp {{\n  server_names_hash_bucket_size 512;\n  access_log off;\n  server_tokens off;\n  client_body_temp_path body;\n  proxy_temp_path proxy;\n  map $http_upgrade $fc_connection {{ default upgrade; '' ''; }}\n  map $http_x_forwarded_proto $fc_proto {{ default $scheme; https https; http http; }}\n  server {{ listen unix:{STATUS_SOCKET}; location / {{ return 200 \"{}\"; }} }}\n",
            self.generation()
        );
        let has_quick_tunnels = self.quick_tunnel.is_some() || !self.quick_tunnels.is_empty();
        if has_quick_tunnels {
            text.push_str("  map $http_x_forwarded_host $fc_quick_host { default $http_x_forwarded_host; '' $host; }\n");
        }
        let mut quick_routes: Vec<_> = self
            .quick_tunnel
            .as_ref()
            .map(|quick| Route {
                host: QUICK_TUNNEL_HOST.into(),
                path_prefix: "/".into(),
                upstreams: quick.upstreams.clone(),
            })
            .into_iter()
            .collect();
        for entry in &self.quick_tunnels {
            let host = quick_tunnel_host(entry.id);
            match &entry.target {
                QuickTunnelTarget::Apps { upstreams } => quick_routes.push(Route {
                    host,
                    path_prefix: "/".into(),
                    upstreams: upstreams.clone(),
                }),
                QuickTunnelTarget::Routes { host: selected } => {
                    quick_routes.extend(
                        self.routes
                            .iter()
                            .filter(|route| &route.host == selected)
                            .map(|route| Route {
                                host: host.clone(),
                                ..route.clone()
                            }),
                    );
                }
            }
        }
        let mut hosts: BTreeMap<&str, Vec<(usize, &Route)>> = BTreeMap::new();
        for (index, route) in self.routes.iter().chain(quick_routes.iter()).enumerate() {
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
            let proxy_host = if has_quick_tunnels && reserved_quick_host(host) {
                "$fc_quick_host"
            } else {
                "$host"
            };
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
                    text.push_str(&format!("    location {location} {{\n      proxy_pass http://fc_route_{index};\n      proxy_http_version 1.1;\n      proxy_set_header Host {proxy_host};\n      proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;\n      proxy_set_header X-Forwarded-Proto $fc_proto;\n      proxy_set_header Upgrade $http_upgrade;\n      proxy_set_header Connection $fc_connection;\n      proxy_connect_timeout 2s;\n      proxy_read_timeout 30s;\n      proxy_next_upstream error timeout http_500 http_502 http_503 http_504;\n      proxy_next_upstream_timeout 3s;\n    }}\n"));
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

/// Complete desired configuration for an anonymous Cloudflare connector.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuickConnectRequest {
    /// Saved local routes and the selected public app addresses.
    pub config: Config,
}

impl QuickConnectRequest {
    /// Require anonymous app targets and bound the complete request before setup.
    pub fn validate(&self) -> Result<()> {
        self.config.validate()?;
        if self.config.quick_tunnel.is_none() {
            return Err("select at least one app for the Quick Tunnel connection".into());
        }
        if serde_json::to_vec(self)?.len() > 48 * 1024 {
            return Err("Quick Tunnel request exceeds 48 KiB".into());
        }
        Ok(())
    }
}

/// Independent traffic service that can be controlled without changing its configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Service {
    /// Nginx host/path routing and request distribution.
    Ingress,
    /// Cloudflare connector to the configured ingress.
    Tunnel,
}

/// Fixed systemd jobs accepted for an individual Pipe service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAction {
    /// Start an already configured service.
    Start,
    /// Stop a service without disabling it or deleting configuration.
    Stop,
    /// Stop and start an already configured service.
    Restart,
}

/// A bounded service control request; callers cannot supply units or shell commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceRequest {
    /// One of the two Firecrab-owned traffic services.
    pub service: Service,
    /// Fixed lifecycle operation to enqueue in systemd.
    pub action: ServiceAction,
}

/// Fixed privileged operations accepted by the helper; no arbitrary commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Operation {
    /// Register, retarget or delete one stable Firecrab public address.
    ManageFirecrabTunnel {
        /// Non-secret local resource intent.
        request: crate::firecrab_tunnel::Request,
    },
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
    /// Install and connect an anonymous Cloudflare Quick Tunnel.
    ConnectQuickTunnel {
        /// Complete desired configuration without account credentials.
        request: QuickConnectRequest,
    },
    /// Remove only the local named connector while preserving routes and peers.
    DisconnectCloudflare,
    /// Queue a lifecycle operation for one configured service.
    Control {
        /// Fixed target service and lifecycle operation.
        request: ServiceRequest,
    },
    /// Add, edit, delete or control one independent anonymous connector.
    ManageQuickTunnel {
        /// Intent applied against the helper's latest saved configuration.
        request: QuickTunnelRequest,
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

/// Address assigned by the currently running anonymous Cloudflare connector.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickTunnelStatus {
    /// Public HTTPS URL, absent while the current connector is starting.
    pub public_url: Option<String>,
}

/// Persisted Pipe configuration and live unit state returned to the dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Stable public addresses, with ownership credentials removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub firecrab_tunnels: Vec<crate::firecrab_tunnel::Address>,
    /// Last installed manifest, absent before the first successful apply.
    pub config: Option<Config>,
    /// Ingress and tunnel unit states, independent of API process lifetime.
    pub units: Vec<UnitStatus>,
    /// Whether a usable Nginx executable exists on the managed host.
    pub nginx_available: bool,
    /// Whether the managed cloudflared executable exists on the host.
    pub cloudflared_available: bool,
    /// Anonymous connector address from the current systemd invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quick_tunnel: Option<QuickTunnelStatus>,
    /// Independent anonymous connectors, including a normalized legacy singleton.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quick_tunnels: Vec<QuickTunnelSnapshot>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Config {
        serde_json::from_value(json!({"routes":[{"host":"app.example.com","upstreams":["127.0.0.1:9000","127.0.0.1:9001"]},{"host":"app.example.com","pathPrefix":"/api","upstreams":["127.0.0.1:9002"]}]})).unwrap()
    }
    #[test]
    fn old_manifests_and_snapshots_do_not_require_quick_tunnel_fields() {
        let config = config();
        assert!(config.quick_tunnel.is_none());
        assert!(
            serde_json::to_value(&config)
                .unwrap()
                .get("quickTunnel")
                .is_none()
        );
        let snapshot: Snapshot = serde_json::from_value(json!({
            "config": config,
            "units": [],
            "nginxAvailable": true,
            "cloudflaredAvailable": false
        }))
        .unwrap();
        assert!(snapshot.quick_tunnel.is_none());
        assert!(
            serde_json::to_value(snapshot)
                .unwrap()
                .get("quickTunnel")
                .is_none()
        );
    }

    #[test]
    fn configs_without_quick_tunnel_keep_the_legacy_generation_for_rollback() {
        let mut config = config();
        // SHA-256 of the pre-Quick Tunnel (listen, routes) JSON fixture.
        let legacy = "cecf3e7ba686778581dfe1bccddaf4dec211106abf465ce78e9dd4109f2619a5";
        assert_eq!(config.generation(), legacy);
        config.tunnel = Some(Tunnel {
            id: Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
            credentials_file: "credentials.json".into(),
            hostname: None,
        });
        assert_eq!(config.generation(), legacy);
    }

    #[test]
    fn quick_tunnel_generates_a_private_root_route_without_editing_saved_routes() {
        let mut config = config();
        let saved_routes = config.routes.clone();
        let old_generation = config.generation();
        config.quick_tunnel = Some(QuickTunnel {
            upstreams: vec![
                "172.30.1.2:80".parse().unwrap(),
                "172.30.1.3:80".parse().unwrap(),
            ],
        });
        config.validate().unwrap();
        let nginx = config.nginx();
        assert_eq!(config.routes, saved_routes);
        assert!(nginx.contains(&format!("server_name {QUICK_TUNNEL_HOST};")));
        assert!(nginx.contains("upstream fc_route_2 {"));
        assert!(nginx.contains("server 172.30.1.2:80 max_fails=1"));
        assert!(nginx.contains("server 172.30.1.3:80 max_fails=1"));
        assert!(nginx.contains("proxy_pass http://fc_route_2;"));
        assert!(nginx.contains("location = \"/api\""));
        assert!(nginx.contains("default_server; server_name _; return 404"));
        assert!(config.tunnel_config().is_none());
        assert_ne!(old_generation, config.generation());
        let first_generation = config.generation();
        config.quick_tunnel.as_mut().unwrap().upstreams.pop();
        assert_ne!(first_generation, config.generation());
    }

    #[test]
    fn only_quick_tunnel_routes_restore_the_public_host_from_the_connector() {
        let mut config = config();
        let local = config.nginx();
        assert!(!local.contains("$fc_quick_host"));
        let local_host_headers = local.matches("proxy_set_header Host $host;").count();
        config.quick_tunnel = Some(QuickTunnel {
            upstreams: vec!["172.30.1.2:80".parse().unwrap()],
        });
        let nginx = config.nginx();
        assert!(nginx.contains("map $http_x_forwarded_host $fc_quick_host { default $http_x_forwarded_host; '' $host; }"));
        assert_eq!(
            nginx
                .matches("proxy_set_header Host $fc_quick_host;")
                .count(),
            1
        );
        assert_eq!(
            nginx.matches("proxy_set_header Host $host;").count(),
            local_host_headers
        );
        let quick_server = nginx
            .split(&format!("server_name {QUICK_TUNNEL_HOST};"))
            .nth(1)
            .unwrap();
        assert!(quick_server.contains("proxy_set_header Host $fc_quick_host;"));
    }

    #[test]
    fn quick_tunnel_rejects_unsafe_or_duplicate_app_targets() {
        for upstreams in [
            vec![],
            vec!["0.0.0.0:80"],
            vec!["[::]:80"],
            vec!["224.0.0.1:80"],
            vec!["127.0.0.1:0"],
            vec!["127.0.0.1:8080"],
            vec!["172.30.1.2:80", "172.30.1.2:80"],
        ] {
            let mut config = config();
            config.quick_tunnel = Some(QuickTunnel {
                upstreams: upstreams
                    .iter()
                    .map(|address| address.parse().unwrap())
                    .collect(),
            });
            assert!(config.validate().is_err(), "{upstreams:?}");
        }
    }

    #[test]
    fn quick_tunnel_cannot_share_a_named_connector_or_its_reserved_route() {
        let mut config = config();
        config.routes[0].host = QUICK_TUNNEL_HOST.into();
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("reserved")
        );
        config.routes.clear();
        config.quick_tunnel = Some(QuickTunnel {
            upstreams: vec!["172.30.1.2:80".parse().unwrap()],
        });
        config.tunnel = Some(Tunnel {
            id: Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
            credentials_file: "credentials.json".into(),
            hostname: None,
        });
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("cannot be configured together")
        );
    }

    #[test]
    fn quick_connect_requires_anonymous_targets_and_rejects_credentials_and_oversize() {
        let mut request = QuickConnectRequest { config: config() };
        assert!(request.validate().is_err());
        request.config.quick_tunnel = Some(QuickTunnel {
            upstreams: vec!["172.30.1.2:80".parse().unwrap()],
        });
        request.validate().unwrap();
        let operation = Operation::ConnectQuickTunnel {
            request: request.clone(),
        };
        let encoded = serde_json::to_value(&operation).unwrap();
        assert_eq!(encoded["action"], "connect_quick_tunnel");
        assert_eq!(
            serde_json::from_value::<Operation>(encoded).unwrap(),
            operation
        );
        assert!(
            serde_json::from_value::<QuickConnectRequest>(
                json!({"config": request.config, "apiToken":"not-needed"})
            )
            .is_err()
        );
        request.config.routes[0].host = "x".repeat(48 * 1024);
        assert!(
            request
                .validate()
                .unwrap_err()
                .to_string()
                .contains("48 KiB")
        );
    }

    #[test]
    fn service_controls_accept_only_fixed_targets_and_actions() {
        for service in ["ingress", "tunnel"] {
            for action in ["start", "stop", "restart"] {
                let value = json!({ "service": service, "action": action });
                let request: ServiceRequest = serde_json::from_value(value.clone()).unwrap();
                assert_eq!(serde_json::to_value(&request).unwrap(), value);
                let operation = Operation::Control { request };
                let encoded = serde_json::to_value(&operation).unwrap();
                assert_eq!(encoded, json!({ "action": "control", "request": value }));
                assert_eq!(
                    serde_json::from_value::<Operation>(encoded).unwrap(),
                    operation
                );
            }
        }
        for value in [
            json!({ "service": "sshd", "action": "restart" }),
            json!({ "service": "ingress", "action": "reload" }),
            json!({ "service": "ingress", "action": "stop", "unit": "sshd.service" }),
            json!({ "service": "ingress" }),
        ] {
            assert!(serde_json::from_value::<ServiceRequest>(value).is_err());
        }
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

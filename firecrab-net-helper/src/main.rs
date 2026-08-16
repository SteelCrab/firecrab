//! Privileged helper daemon: owns bridge/firewall host operations behind a
//! Unix socket so `firecrab-api` never needs root. Peers are authenticated
//! via `SO_PEERCRED` against an explicit UID allowlist, not the socket's
//! filesystem permissions alone.

use std::collections::HashSet;
use std::env;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

/// Firecrab bridge (`fcbr0`) creation/repair.
mod bridge;
/// DHCP (dnsmasq) for guest VMs.
mod dhcp;
/// Per-VM and global nftables firewall rules.
mod firewall;
/// Distro host firewall holes (UFW, firewalld, iptables, nft).
mod host_acl;
/// NAT/uplink detection, split out of `firewall`.
mod nat;
/// Per-VM TAP device lifecycle.
mod tap;

use firecrab_helper_protocol::PROTOCOL_VERSION;
use firecrab_helper_protocol::framing::{read_frame, write_frame};
use firecrab_helper_protocol::network::{
    HelperFailure, MicroNetworkSpec, NetworkRequest, NetworkRequestEnvelope,
    NetworkResponseEnvelope, VmPolicySpec,
};
use thiserror::Error;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use tokio::time::timeout;

/// Default Unix socket path, overridable via `FIRECRAB_NET_HELPER_SOCK`.
const DEFAULT_SOCKET_PATH: &str = "/run/firecrab/net-helper.sock";
/// Upper bound on concurrently handled connections; excess ones are dropped.
const MAX_CONNECTIONS: usize = 16;
/// How long to wait for a full request frame before closing the connection.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Failures that can prevent the helper from starting up.
#[derive(Debug, Error)]
enum StartupError {
    /// `FIRECRAB_NET_HELPER_ALLOWED_UID` isn't a valid `u32`.
    #[error("invalid FIRECRAB_NET_HELPER_ALLOWED_UID: {0}")]
    InvalidAllowedUid(String),
    /// Couldn't create the socket's parent directory.
    #[error("failed to prepare socket directory {path}")]
    SocketDir {
        /// The directory that couldn't be created.
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Couldn't bind the Unix socket.
    #[error("failed to bind helper socket {path}")]
    Bind {
        /// The socket path that couldn't be bound.
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Couldn't restrict the socket file's permissions after binding.
    #[error("failed to restrict permissions on helper socket {path}")]
    Permissions {
        /// The socket path whose permissions couldn't be set.
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Couldn't enable IPv4 forwarding globally.
    #[error("failed to enable net.ipv4.ip_forward")]
    IpForward(#[source] io::Error),
}

/// Resolved startup configuration plus the shared actors every connection
/// dispatches into.
#[derive(Debug)]
struct HelperConfig {
    /// Where the Unix socket is bound.
    socket_path: PathBuf,
    /// UIDs allowed to connect, checked via `SO_PEERCRED`.
    allowed_peer_uids: HashSet<u32>,
    /// MTU to apply to all bridges. Set from `FIRECRAB_BRIDGE_MTU` or
    /// auto-detected from the host's default-route uplink at startup.
    bridge_mtu: u32,
    /// Shared firewall state (single-writer mutex inside).
    firewall: firewall::FirewallActor,
    /// Shared bridge-creation state (single-writer mutex inside).
    bridge: bridge::BridgeActor,
    /// Shared DHCP (dnsmasq) state (single-writer mutex inside).
    dhcp: dhcp::DhcpActor,
}

impl HelperConfig {
    /// Reads configuration from the process environment.
    async fn load() -> Result<Self, StartupError> {
        let socket_path =
            env::var("FIRECRAB_NET_HELPER_SOCK").unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_owned());
        let allowed_uid = env::var("FIRECRAB_NET_HELPER_ALLOWED_UID").ok();
        let bridge_mtu = match env::var("FIRECRAB_BRIDGE_MTU").ok() {
            Some(val) => val
                .trim()
                .parse::<u32>()
                .unwrap_or(bridge::DEFAULT_BRIDGE_MTU),
            None => bridge::detect_uplink_mtu().await,
        };
        Self::from_values(&socket_path, allowed_uid.as_deref(), bridge_mtu)
    }

    /// Builds config from already-parsed values (used directly by tests).
    fn from_values(
        socket_path: &str,
        allowed_uid: Option<&str>,
        bridge_mtu: u32,
    ) -> Result<Self, StartupError> {
        // The helper always trusts its own uid so unprivileged local
        // development needs no extra configuration; production adds the
        // API service uid explicitly.
        let mut allowed_peer_uids = HashSet::from([effective_uid()]);
        if let Some(raw) = allowed_uid {
            let uid = raw
                .trim()
                .parse::<u32>()
                .map_err(|_| StartupError::InvalidAllowedUid(raw.to_owned()))?;
            allowed_peer_uids.insert(uid);
        }

        Ok(Self {
            socket_path: PathBuf::from(socket_path),
            allowed_peer_uids,
            bridge_mtu,
            firewall: firewall::FirewallActor::new(),
            bridge: bridge::BridgeActor::new(),
            dhcp: dhcp::DhcpActor::new(),
        })
    }

    /// Whether `uid` is on the allowlist.
    fn peer_allowed(&self, uid: u32) -> bool {
        self.allowed_peer_uids.contains(&uid)
    }
}

/// This process's effective UID, always implicitly trusted.
fn effective_uid() -> u32 {
    // SAFETY: geteuid has no failure modes or preconditions.
    unsafe { libc::geteuid() }
}

/// Entry point: runs the server and prints any startup error's full cause
/// chain before exiting non-zero.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("[ERROR] {error}");
            let mut source = std::error::Error::source(&error);
            while let Some(cause) = source {
                eprintln!("[ERROR] caused by: {cause}");
                source = cause.source();
            }
            ExitCode::FAILURE
        }
    }
}

/// Loads config, binds the socket, and serves until shutdown.
async fn run() -> Result<(), StartupError> {
    let config = Arc::new(HelperConfig::load().await?);
    // Required for NAT'd VM egress to work at all; previously a manual
    // operator step (public-docs/networking.md). Global and
    // idempotent, so doing it once here (rather than on every ensure_bridge
    // call) is enough.
    bridge::enable_ip_forward().map_err(StartupError::IpForward)?;
    println!("[INFO] bridge MTU: {}", config.bridge_mtu);
    let listener = bind_socket(&config.socket_path)?;
    println!(
        "[INFO] net-helper listening on {}",
        config.socket_path.display()
    );

    serve(listener, Arc::clone(&config), shutdown_signal()).await;
    let _ = fs::remove_file(&config.socket_path);
    Ok(())
}

/// Creates the socket's parent directory if needed, removes a stale socket
/// file, binds, and restricts permissions to owner/group only.
fn bind_socket(path: &Path) -> Result<UnixListener, StartupError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|source| StartupError::SocketDir {
            path: parent.to_owned(),
            source,
        })?;
    }
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(StartupError::Bind {
                path: path.to_owned(),
                source,
            });
        }
    }

    let listener = UnixListener::bind(path).map_err(|source| StartupError::Bind {
        path: path.to_owned(),
        source,
    })?;
    // Owner/group access only; peers are additionally checked via SO_PEERCRED.
    fs::set_permissions(path, fs::Permissions::from_mode(0o660)).map_err(|source| {
        StartupError::Permissions {
            path: path.to_owned(),
            source,
        }
    })?;
    Ok(listener)
}

/// Resolves once SIGTERM or Ctrl-C is received.
async fn shutdown_signal() {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
    }
}

/// Accepts connections until `shutdown` resolves, spawning one task per
/// connection bounded by [`MAX_CONNECTIONS`] concurrent permits.
async fn serve(
    listener: UnixListener,
    config: Arc<HelperConfig>,
    shutdown: impl Future<Output = ()>,
) {
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                // At capacity new connections are dropped, not queued.
                let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else { continue };
                let config = Arc::clone(&config);
                tokio::spawn(async move {
                    let _permit = permit;
                    handle_connection(stream, config).await;
                });
            }
        }
    }
}

/// Serves requests on one accepted connection until it errors, times out, or
/// a version-mismatch response is sent.
async fn handle_connection(stream: UnixStream, config: Arc<HelperConfig>) {
    let Ok(peer) = stream.peer_cred() else { return };
    // Silent close: unauthenticated peers learn nothing about the protocol.
    if !config.peer_allowed(peer.uid()) {
        return;
    }

    let (mut reader, mut writer) = stream.into_split();
    loop {
        let envelope: NetworkRequestEnvelope =
            match timeout(REQUEST_TIMEOUT, read_frame(&mut reader)).await {
                Ok(Ok(envelope)) => envelope,
                // EOF, oversized, malformed, or a stalled partial frame all
                // end the connection without a response.
                Ok(Err(_)) | Err(_) => return,
            };

        let response = respond_to(envelope, &config).await;
        let version_rejected = matches!(
            response.result,
            Err(HelperFailure::UnsupportedVersion { .. })
        );
        if write_frame(&mut writer, &response).await.is_err() || version_rejected {
            return;
        }
    }
}

/// Validates the envelope's protocol version, then dispatches its request.
async fn respond_to(
    envelope: NetworkRequestEnvelope,
    config: &HelperConfig,
) -> NetworkResponseEnvelope {
    let result = if envelope.version == PROTOCOL_VERSION {
        dispatch(envelope.request, config).await
    } else {
        Err(HelperFailure::UnsupportedVersion {
            supported: PROTOCOL_VERSION,
        })
    };
    NetworkResponseEnvelope {
        version: PROTOCOL_VERSION,
        request_id: envelope.request_id,
        result,
    }
}

/// Sanity bound on a prefix length that ultimately comes from user input (a
/// MicroNetwork's subnet CIDR) — the helper is the trust boundary and
/// re-validates rather than assuming the API's own check already caught it
/// (same reasoning as `egress_policy`'s allowlist lookup). 30 leaves at
/// least 2 host addresses; 8 keeps the reserved range from swallowing most
/// of the host's own address space.
fn validate_prefix(prefix: u8) -> Result<(), HelperFailure> {
    if (8..=30).contains(&prefix) {
        Ok(())
    } else {
        Err(HelperFailure::InvalidRequest {
            detail: format!("prefix {prefix} is out of the accepted 8-30 range"),
        })
    }
}

/// Same check across a whole network set, applied before any of it is
/// rendered into an nftables ruleset or a dnsmasq config.
fn validate_micro_networks(micro_networks: &[MicroNetworkSpec]) -> Result<(), HelperFailure> {
    micro_networks
        .iter()
        .try_for_each(|network| validate_prefix(network.prefix))
}

/// Re-validates every supplied per-network uplink before nft is touched.
/// Omitted means auto (the host default-route iface); a present name must
/// pass [`nat::validate_uplink`] and exist under `/sys/class/net`. Missing
/// is a client error, not an internal one — the helper is the trust
/// boundary. The API's sysfs check is UX only.
fn validate_uplinks(micro_networks: &[MicroNetworkSpec]) -> Result<(), HelperFailure> {
    for network in micro_networks {
        let Some(name) = network.uplink.as_deref() else {
            continue;
        };
        nat::validate_uplink(name).map_err(|error| HelperFailure::InvalidRequest {
            detail: error.to_string(),
        })?;
        if !nat::uplink_exists(name) {
            return Err(HelperFailure::InvalidRequest {
                detail: format!("uplink {name:?} is not a host interface"),
            });
        }
    }
    Ok(())
}

/// Re-validates port forwards against the same rules the API already
/// enforces — the helper is the trust boundary (same reasoning as
/// `validate_micro_networks` and the `egress_policy` allowlist lookup) and
/// does not assume the caller's own check already caught a malformed value.
/// In particular, an unrecognized protocol must be rejected outright:
/// `firewall::render_vm_policy` treats anything that isn't `"udp"` as TCP,
/// so a bad value silently reaching it would render a DNAT rule the caller
/// never asked for instead of failing loudly.
fn validate_port_forwards(
    port_forwards: &[firecrab_helper_protocol::network::PortForwardSpec],
) -> Result<(), HelperFailure> {
    for pf in port_forwards {
        if pf.host_port == 0 {
            return Err(HelperFailure::InvalidRequest {
                detail: "port forward host_port cannot be 0".to_owned(),
            });
        }
        if pf.guest_port == 0 {
            return Err(HelperFailure::InvalidRequest {
                detail: "port forward guest_port cannot be 0".to_owned(),
            });
        }
        if !pf.protocol.eq_ignore_ascii_case("tcp") && !pf.protocol.eq_ignore_ascii_case("udp") {
            return Err(HelperFailure::InvalidRequest {
                detail: format!("port forward protocol {:?} must be tcp or udp", pf.protocol),
            });
        }
    }
    Ok(())
}

/// Validates and converts the API's complete policy snapshot before any nft
/// state is touched. Duplicate identities, addresses, and host ports would
/// make the rendered snapshot ambiguous, so reject them at the privilege
/// boundary with an actionable client error.
fn validate_vm_policies(
    specs: Vec<VmPolicySpec>,
) -> Result<Vec<firewall::VmPolicy>, HelperFailure> {
    let mut vm_ids = HashSet::new();
    let mut ipv4s = HashSet::new();
    let mut host_ports = HashSet::new();
    let mut policies = Vec::with_capacity(specs.len());

    for spec in specs {
        if !vm_ids.insert(spec.vm_id) {
            return Err(HelperFailure::InvalidRequest {
                detail: format!("duplicate VM policy for {}", spec.vm_id),
            });
        }
        if !ipv4s.insert(spec.ipv4) {
            return Err(HelperFailure::InvalidRequest {
                detail: format!("duplicate VM policy IPv4 {}", spec.ipv4),
            });
        }
        validate_port_forwards(&spec.port_forwards)?;
        for port_forward in &spec.port_forwards {
            let key = (
                port_forward.protocol.to_ascii_lowercase(),
                port_forward.host_port,
            );
            if !host_ports.insert(key) {
                return Err(HelperFailure::InvalidRequest {
                    detail: format!(
                        "duplicate host port {}/{} in VM policy snapshot",
                        port_forward.host_port, port_forward.protocol
                    ),
                });
            }
        }
        let egress = firewall::EgressPolicy::from_id(&spec.egress_policy).ok_or_else(|| {
            HelperFailure::InvalidRequest {
                detail: format!("unknown egress policy id {:?}", spec.egress_policy),
            }
        })?;
        policies.push(firewall::VmPolicy {
            vm_id: spec.vm_id,
            ipv4: spec.ipv4,
            mac: spec.mac,
            egress,
            allow_host_ssh: spec.allow_host_ssh,
            port_forwards: spec.port_forwards,
        });
    }
    Ok(policies)
}

/// Routes a validated request to the matching bridge/firewall operation.
async fn dispatch(request: NetworkRequest, config: &HelperConfig) -> Result<(), HelperFailure> {
    match request {
        NetworkRequest::EnsureBridge => bridge::ensure_bridge(&config.bridge, config.bridge_mtu)
            .await
            .map_err(|error| HelperFailure::Internal {
                detail: error_chain(&error),
            }),
        NetworkRequest::EnsureMicroNetworkBridge {
            micro_network_id,
            gateway,
            prefix,
        } => {
            // Sanity bound on a value that ultimately comes from user input
            // (a MicroNetwork's subnet CIDR) — the helper is the trust
            // boundary and re-validates rather than assuming the API's own
            // check already caught it (same reasoning as egress_policy's
            // allowlist lookup below). 30 leaves at least 2 host addresses;
            // 8 keeps the reserved range from swallowing most of the host's
            // own address space.
            validate_prefix(prefix)?;
            bridge::ensure_micro_network_bridge(
                &config.bridge,
                micro_network_id,
                gateway,
                prefix,
                config.bridge_mtu,
            )
            .await
            .map_err(|error| HelperFailure::Internal {
                detail: error_chain(&error),
            })
        }
        NetworkRequest::RemoveMicroNetworkBridge { micro_network_id } => {
            bridge::delete_micro_network_bridge(&config.bridge, micro_network_id)
                .await
                .map_err(|error| HelperFailure::Internal {
                    detail: error_chain(&error),
                })
        }
        NetworkRequest::EnsureFirewall {
            micro_networks,
            vm_policies,
        } => {
            validate_micro_networks(&micro_networks)?;
            validate_uplinks(&micro_networks)?;
            let vm_policies = validate_vm_policies(vm_policies)?;
            firewall::ensure_firewall(&config.firewall, &micro_networks, &vm_policies)
                .await
                .map_err(|error| HelperFailure::Internal {
                    detail: error_chain(&error),
                })
        }
        NetworkRequest::ApplyVmPolicy {
            vm_id,
            ipv4,
            mac,
            egress_policy,
            allow_host_ssh,
            port_forwards,
        } => {
            // Resolve the API-supplied egress ID against the helper's own
            // allowlist; an unknown ID is a client error, not an internal one.
            let egress = firewall::EgressPolicy::from_id(&egress_policy).ok_or_else(|| {
                HelperFailure::InvalidRequest {
                    detail: format!("unknown egress policy id {egress_policy:?}"),
                }
            })?;
            validate_port_forwards(&port_forwards)?;
            let policy = firewall::VmPolicy {
                vm_id,
                ipv4,
                mac,
                egress,
                allow_host_ssh,
                port_forwards,
            };
            firewall::apply_vm_policy(&config.firewall, policy)
                .await
                .map_err(|error| HelperFailure::Internal {
                    detail: error_chain(&error),
                })
        }
        NetworkRequest::RemoveVmPolicy { vm_id } => {
            firewall::remove_vm_policy(&config.firewall, vm_id)
                .await
                .map_err(|error| HelperFailure::Internal {
                    detail: error_chain(&error),
                })
        }
        NetworkRequest::CreateTap {
            vm_id,
            micro_network_id,
        } => tap::create_tap(vm_id, micro_network_id)
            .await
            .map_err(|error| HelperFailure::Internal {
                detail: error_chain(&error),
            }),
        NetworkRequest::DeleteTap { vm_id } => {
            tap::delete_tap(vm_id)
                .await
                .map_err(|error| HelperFailure::Internal {
                    detail: error_chain(&error),
                })
        }
        NetworkRequest::SyncDhcpLeases {
            revision,
            leases,
            micro_networks,
        } => {
            validate_micro_networks(&micro_networks)?;
            dhcp::sync_dhcp_leases(&config.dhcp, revision, &leases, &micro_networks)
                .await
                .map_err(|error| HelperFailure::Internal {
                    detail: error_chain(&error),
                })
        }
    }
}

/// Flatten an error and its causes so the API-side log keeps the root cause
/// (for example the EPERM under a generic "rtnetlink operation failed").
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut detail = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        detail.push_str(": ");
        detail.push_str(&cause.to_string());
        source = cause.source();
    }
    detail
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::assert_matches;

    use tokio::io::AsyncWriteExt;
    use tokio::sync::oneshot;
    use uuid::Uuid;

    // Unix socket paths are limited to ~108 bytes; keep test sockets short.
    fn short_tempdir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("fc-net")
            .tempdir_in("/tmp")
            .expect("create tempdir")
    }

    fn start_helper(
        dir: &tempfile::TempDir,
    ) -> (PathBuf, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
        let path = dir.path().join("helper.sock");
        let config = Arc::new(
            HelperConfig::from_values(
                path.to_str().expect("utf-8 path"),
                None,
                bridge::DEFAULT_BRIDGE_MTU,
            )
            .expect("helper config"),
        );
        let listener = bind_socket(&config.socket_path).expect("bind helper socket");
        let (stop, stopped) = oneshot::channel::<()>();
        let handle = tokio::spawn(serve(listener, config, async {
            let _ = stopped.await;
        }));
        (path, stop, handle)
    }

    #[test]
    fn own_uid_is_allowed_and_configured_uid_is_added() {
        let config =
            HelperConfig::from_values("/tmp/x.sock", Some("12345"), bridge::DEFAULT_BRIDGE_MTU)
                .expect("config");
        assert!(config.peer_allowed(effective_uid()));
        assert!(config.peer_allowed(12345));
        assert!(!config.peer_allowed(54321));
    }

    #[test]
    fn non_numeric_allowed_uid_is_rejected() {
        assert_matches!(
            HelperConfig::from_values("/tmp/x.sock", Some("wheel"), bridge::DEFAULT_BRIDGE_MTU),
            Err(StartupError::InvalidAllowedUid(_))
        );
    }

    #[tokio::test]
    async fn deleting_a_tap_that_was_never_created_is_a_no_op() {
        // Read-only rtnetlink lookups need no special privilege, so this is
        // safe to run unprivileged: the delete never reaches the point of
        // needing CAP_NET_ADMIN because find_link reports nothing to delete.
        let config = HelperConfig::from_values("/tmp/x.sock", None, bridge::DEFAULT_BRIDGE_MTU)
            .expect("helper config");
        let request = NetworkRequest::DeleteTap {
            vm_id: Uuid::new_v4(),
        };
        assert_eq!(dispatch(request, &config).await, Ok(()));
    }

    #[tokio::test]
    async fn apply_vm_policy_rejects_an_unknown_egress_id_as_invalid_request() {
        let config = HelperConfig::from_values("/tmp/x.sock", None, bridge::DEFAULT_BRIDGE_MTU)
            .expect("helper config");
        let request = NetworkRequest::ApplyVmPolicy {
            vm_id: Uuid::nil(),
            ipv4: "172.30.0.9".parse().unwrap(),
            mac: "02:fc:00:00:00:09".parse().unwrap(),
            egress_policy: "0.0.0.0/0".to_owned(),
            allow_host_ssh: false,
            port_forwards: Vec::new(),
        };
        assert_matches!(
            dispatch(request, &config).await,
            Err(HelperFailure::InvalidRequest { .. })
        );
    }

    #[tokio::test]
    async fn apply_vm_policy_rejects_a_zero_port_or_unknown_protocol_as_invalid_request() {
        let config = HelperConfig::from_values("/tmp/x.sock", None, bridge::DEFAULT_BRIDGE_MTU)
            .expect("helper config");
        let base = |port_forwards| NetworkRequest::ApplyVmPolicy {
            vm_id: Uuid::nil(),
            ipv4: "172.30.0.9".parse().unwrap(),
            mac: "02:fc:00:00:00:09".parse().unwrap(),
            egress_policy: "internet".to_owned(),
            allow_host_ssh: false,
            port_forwards,
        };
        let cases = [
            vec![firecrab_helper_protocol::network::PortForwardSpec {
                host_port: 0,
                guest_port: 80,
                protocol: "tcp".to_owned(),
            }],
            vec![firecrab_helper_protocol::network::PortForwardSpec {
                host_port: 8080,
                guest_port: 0,
                protocol: "tcp".to_owned(),
            }],
            vec![firecrab_helper_protocol::network::PortForwardSpec {
                host_port: 8080,
                guest_port: 80,
                protocol: "icmp".to_owned(),
            }],
        ];
        for port_forwards in cases {
            assert_matches!(
                dispatch(base(port_forwards), &config).await,
                Err(HelperFailure::InvalidRequest { .. })
            );
        }
    }

    #[test]
    fn firewall_snapshot_rejects_duplicate_ipv4s_before_nft() {
        let first = VmPolicySpec {
            vm_id: Uuid::from_u128(1),
            ipv4: "172.30.0.40".parse().unwrap(),
            mac: "02:fc:00:00:00:01".parse().unwrap(),
            egress_policy: "internet".to_owned(),
            allow_host_ssh: false,
            port_forwards: Vec::new(),
        };
        let second = VmPolicySpec {
            vm_id: Uuid::from_u128(2),
            mac: "02:fc:00:00:00:02".parse().unwrap(),
            ..first.clone()
        };

        assert_matches!(validate_vm_policies(vec![first, second]),
            Err(HelperFailure::InvalidRequest { detail }) if detail.contains("duplicate VM policy IPv4"));
    }

    fn sample_spec(uplink: Option<&str>) -> MicroNetworkSpec {
        MicroNetworkSpec {
            micro_network_id: Uuid::nil(),
            gateway: "172.31.0.1".parse().unwrap(),
            prefix: 24,
            internet_enabled: true,
            uplink: uplink.map(str::to_owned),
        }
    }

    #[test]
    fn validate_uplinks_accepts_omitted_and_existing_ifaces() {
        assert_eq!(validate_uplinks(&[sample_spec(None)]), Ok(()));
        let name = std::fs::read_dir("/sys/class/net")
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .find(|name| nat::validate_uplink(name).is_ok() && nat::uplink_exists(name))
            .expect("test host has a usable interface");
        assert_eq!(validate_uplinks(&[sample_spec(Some(&name))]), Ok(()));
    }

    #[tokio::test]
    async fn ensure_firewall_rejects_an_unknown_uplink_as_invalid_request() {
        let config = HelperConfig::from_values("/tmp/x.sock", None, bridge::DEFAULT_BRIDGE_MTU)
            .expect("helper config");
        let request = NetworkRequest::EnsureFirewall {
            micro_networks: vec![sample_spec(Some("nosuchiface0"))],
            vm_policies: Vec::new(),
        };
        assert_matches!(dispatch(request, &config).await,
            Err(HelperFailure::InvalidRequest { detail }) if detail.contains("nosuchiface0"));
    }

    #[tokio::test]
    async fn ensure_firewall_rejects_unsafe_uplink_names_as_invalid_request() {
        let config = HelperConfig::from_values("/tmp/x.sock", None, bridge::DEFAULT_BRIDGE_MTU)
            .expect("helper config");
        for name in [
            "", "eth0/foo", "eth0;id", "lo", "fct0", "mnb0", "eth0\"x", "eth0\\x",
        ] {
            let request = NetworkRequest::EnsureFirewall {
                micro_networks: vec![sample_spec(Some(name))],
                vm_policies: Vec::new(),
            };
            assert_matches!(
                dispatch(request, &config).await,
                Err(HelperFailure::InvalidRequest { .. }),
                "{name:?} should be rejected before nft"
            );
        }
    }

    #[tokio::test]
    async fn ensure_micro_network_bridge_rejects_an_out_of_range_prefix_as_invalid_request() {
        let config = HelperConfig::from_values("/tmp/x.sock", None, bridge::DEFAULT_BRIDGE_MTU)
            .expect("helper config");
        for prefix in [0, 7, 31, 32] {
            let request = NetworkRequest::EnsureMicroNetworkBridge {
                micro_network_id: Uuid::nil(),
                gateway: "172.31.0.1".parse().unwrap(),
                prefix,
            };
            assert_matches!(
                dispatch(request, &config).await,
                Err(HelperFailure::InvalidRequest { .. }),
                "prefix {prefix} should have been rejected"
            );
        }
    }

    #[tokio::test]
    async fn serves_multiple_requests_per_connection() {
        let dir = short_tempdir();
        let (path, stop, handle) = start_helper(&dir);

        let mut stream = UnixStream::connect(&path).await.expect("connect");
        for _ in 0..2 {
            // DeleteTap of a nonexistent device is a deterministic, read-only
            // no-op (see deleting_a_tap_that_was_never_created_is_a_no_op),
            // so the framing loop is testable without privileges.
            let envelope = NetworkRequestEnvelope::new(
                Uuid::new_v4(),
                NetworkRequest::DeleteTap {
                    vm_id: Uuid::new_v4(),
                },
            );
            write_frame(&mut stream, &envelope)
                .await
                .expect("send request");
            let response: NetworkResponseEnvelope =
                read_frame(&mut stream).await.expect("receive response");
            assert_eq!(response.version, PROTOCOL_VERSION);
            assert_eq!(response.request_id, envelope.request_id);
            assert_eq!(response.result, Ok(()));
        }

        drop(stop);
        handle.await.expect("helper task");
    }

    #[tokio::test]
    async fn version_mismatch_is_answered_then_the_connection_closes() {
        let dir = short_tempdir();
        let (path, _stop, _handle) = start_helper(&dir);

        let mut stream = UnixStream::connect(&path).await.expect("connect");
        let mut envelope =
            NetworkRequestEnvelope::new(Uuid::new_v4(), NetworkRequest::EnsureBridge);
        envelope.version = PROTOCOL_VERSION + 1;
        write_frame(&mut stream, &envelope)
            .await
            .expect("send request");

        let response: NetworkResponseEnvelope =
            read_frame(&mut stream).await.expect("receive response");
        assert_eq!(
            response.result,
            Err(HelperFailure::UnsupportedVersion {
                supported: PROTOCOL_VERSION
            })
        );

        assert!(
            read_frame::<_, NetworkResponseEnvelope>(&mut stream)
                .await
                .is_err(),
            "connection should be closed after a version rejection"
        );
    }

    #[tokio::test]
    async fn oversized_frames_close_the_connection_without_a_reply() {
        let dir = short_tempdir();
        let (path, _stop, _handle) = start_helper(&dir);

        let mut stream = UnixStream::connect(&path).await.expect("connect");
        let oversized =
            ((firecrab_helper_protocol::framing::MAX_FRAME_BYTES + 1) as u32).to_be_bytes();
        stream
            .write_all(&oversized)
            .await
            .expect("send length prefix");

        assert!(
            read_frame::<_, NetworkResponseEnvelope>(&mut stream)
                .await
                .is_err(),
            "helper must drop the connection instead of answering"
        );
    }
}

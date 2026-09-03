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
/// Applying a downloaded host bundle (`ApplySelfUpdate`).
mod self_update;
/// Per-VM TAP device lifecycle.
mod tap;

use firecrab_helper_protocol::PROTOCOL_VERSION;
use firecrab_helper_protocol::framing::{read_frame, write_frame};
use firecrab_helper_protocol::network::{
    HelperFailure, MicroNetworkIpv6Spec, MicroNetworkSpec, NetworkRequest, NetworkRequestEnvelope,
    NetworkResponseEnvelope, VmPolicySpec,
};
// Only referenced by the ApplySelfUpdate dispatch tests below (dispatch
// itself just destructures and forwards the field, never naming the type).
#[cfg(test)]
use firecrab_helper_protocol::network::InstallLayout;
use thiserror::Error;
use tokio::io::AsyncWriteExt;
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

/// Failures tearing down every Firecrab-owned interface and nftables table
/// (`--teardown` mode).
#[derive(Debug, Error)]
enum TeardownError {
    /// Removing the owned nftables tables failed.
    #[error("failed to remove firewall")]
    Firewall(#[from] firewall::FirewallError),
    /// Removing an owned bridge or TAP device failed.
    #[error("failed to remove network interfaces")]
    Bridge(#[from] bridge::BridgeError),
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

/// Whether argv requests `--teardown` instead of serving.
fn wants_teardown(args: &[String]) -> bool {
    args.get(1).map(String::as_str) == Some("--teardown")
}

/// Entry point: hands off to [`run_cli`], the testable half of `main` — this
/// wrapper exists only because `#[tokio::main]` has to sit directly on
/// `main` itself.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    run_cli(env::args().collect()).await
}

/// `--teardown` removes every Firecrab-owned interface and nftables table
/// and exits instead of serving; otherwise runs the server. Either way,
/// prints any error's full cause chain before exiting non-zero.
async fn run_cli(args: Vec<String>) -> ExitCode {
    let result = if wants_teardown(&args) {
        teardown()
            .await
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error>)
    } else {
        run()
            .await
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error>)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => print_failure(error.as_ref()),
    }
}

/// Prints an error's full cause chain, for [`run_cli`] to report before
/// exiting non-zero.
fn print_failure(error: &dyn std::error::Error) -> ExitCode {
    eprintln!("[ERROR] {error}");
    let mut source = error.source();
    while let Some(cause) = source {
        eprintln!("[ERROR] caused by: {cause}");
        source = cause.source();
    }
    ExitCode::FAILURE
}

/// Removes every Firecrab-owned nftables table and network interface: the
/// default bridge, every MicroNetwork bridge, every VM TAP device. Run by
/// `install.sh --uninstall` right after stopping the service and before its
/// binary is deleted — a plain `systemctl stop` sends SIGTERM, which only
/// stops accepting connections (`shutdown_signal`) and leaves all of this in
/// place; only a host reboot would otherwise clear it.
async fn teardown() -> Result<(), TeardownError> {
    firewall::remove_firewall(&firewall::FirewallActor::new()).await?;
    bridge::teardown_all(&bridge::BridgeActor::new()).await?;
    Ok(())
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

/// What the connection loop must do *after* the response frame is flushed.
/// Returned by [`dispatch`] rather than performed inside it, so a request that
/// restarts this very process cannot run before its answer is on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterResponse {
    /// Normal: keep serving requests on this connection.
    Continue,
    /// A self-update was applied; restart both units as the last action.
    RestartUnits,
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

        let (response, after) = respond_to(envelope, &config).await;
        let version_rejected = matches!(
            response.result,
            Err(HelperFailure::UnsupportedVersion { .. })
        );
        let wrote = write_frame(&mut writer, &response).await.is_ok();
        if after == AfterResponse::RestartUnits {
            // Best-effort clean FIN so the CLI sees the frame end before we go
            // away. The restart runs even when `wrote` is false: the binaries
            // are already swapped, and skipping it would leave the old process
            // running on top of the new files.
            let _ = writer.shutdown().await;
            self_update::restart_units().await;
            return;
        }
        if !wrote || version_rejected {
            return;
        }
    }
}

/// Validates the envelope's protocol version, then dispatches its request,
/// returning both the answer and whatever the loop must do once that answer is
/// on the wire.
async fn respond_to(
    envelope: NetworkRequestEnvelope,
    config: &HelperConfig,
) -> (NetworkResponseEnvelope, AfterResponse) {
    let result = if envelope.version == PROTOCOL_VERSION {
        dispatch(envelope.request, config).await
    } else {
        Err(HelperFailure::UnsupportedVersion {
            supported: PROTOCOL_VERSION,
        })
    };
    let after = match &result {
        Ok(after) => *after,
        Err(_) => AfterResponse::Continue,
    };
    (
        NetworkResponseEnvelope {
            version: PROTOCOL_VERSION,
            request_id: envelope.request_id,
            result: result.map(|_| ()),
        },
        after,
    )
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

/// The same trust-boundary re-validation for a network's IPv6 plan. Only a
/// `/64` is accepted: SLAAC's EUI-64 interface identifier is exactly 64 bits
/// wide, so any other length hands guests a prefix they cannot build the
/// stored address from. The prefix must be Unique Local (`fc00::/7`) or
/// global unicast (`2000::/3`) — an allowlist, not a denylist of
/// unspecified / loopback / multicast / link-local.
fn validate_ipv6(ipv6: &MicroNetworkIpv6Spec) -> Result<(), HelperFailure> {
    if ipv6.prefix != 64 {
        return Err(HelperFailure::InvalidRequest {
            detail: format!("IPv6 prefix length {} must be 64", ipv6.prefix),
        });
    }
    if ipv6.is_routable_scope() {
        Ok(())
    } else {
        Err(HelperFailure::InvalidRequest {
            detail: format!(
                "IPv6 gateway {} is not a global or unique-local address",
                ipv6.gateway
            ),
        })
    }
}

/// Same check across a whole network set, applied before any of it is
/// rendered into an nftables ruleset or a dnsmasq config.
fn validate_micro_networks(micro_networks: &[MicroNetworkSpec]) -> Result<(), HelperFailure> {
    micro_networks.iter().try_for_each(|network| {
        validate_prefix(network.prefix)?;
        network.ipv6.as_ref().map_or(Ok(()), validate_ipv6)
    })
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
/// `firewall::render_vm_policy_for_network` treats anything that isn't `"udp"` as TCP,
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
    let mut ipv6s = HashSet::new();
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
        // Two VMs pinned to one v6 address would collide in the vm_egress6 /
        // vm_ingress6 maps, which are keyed by address exactly like their v4
        // counterparts.
        if let Some(ipv6) = spec.ipv6
            && !ipv6s.insert(ipv6)
        {
            return Err(HelperFailure::InvalidRequest {
                detail: format!("duplicate VM policy IPv6 {ipv6}"),
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
            ipv6: spec.ipv6,
            mac: spec.mac,
            egress,
            allow_host_ssh: spec.allow_host_ssh,
            port_forwards: spec.port_forwards,
        });
    }
    Ok(policies)
}

/// Routes a validated request to the matching bridge/firewall operation, and
/// reports what the connection loop must do once the answer is written.
async fn dispatch(
    request: NetworkRequest,
    config: &HelperConfig,
) -> Result<AfterResponse, HelperFailure> {
    match request {
        NetworkRequest::EnsureBridge => bridge::ensure_bridge(&config.bridge, config.bridge_mtu)
            .await
            .map(|()| AfterResponse::Continue)
            .map_err(|error| HelperFailure::Internal {
                detail: error_chain(&error),
            }),
        NetworkRequest::EnsureMicroNetworkBridge {
            micro_network_id,
            gateway,
            prefix,
            ipv6,
        } => {
            // Sanity bound on a value that ultimately comes from user input
            // (a MicroNetwork's subnet CIDR) — the helper is the trust
            // boundary and re-validates rather than assuming the API's own
            // check already caught it (same reasoning as egress_policy's
            // allowlist lookup below). 30 leaves at least 2 host addresses;
            // 8 keeps the reserved range from swallowing most of the host's
            // own address space.
            validate_prefix(prefix)?;
            if let Some(ipv6) = ipv6.as_ref() {
                validate_ipv6(ipv6)?;
            }
            bridge::ensure_micro_network_bridge(
                &config.bridge,
                micro_network_id,
                gateway,
                prefix,
                config.bridge_mtu,
                ipv6,
            )
            .await
            .map(|()| AfterResponse::Continue)
            .map_err(|error| HelperFailure::Internal {
                detail: error_chain(&error),
            })
        }
        NetworkRequest::RemoveMicroNetworkBridge { micro_network_id } => {
            bridge::delete_micro_network_bridge(&config.bridge, micro_network_id)
                .await
                .map(|()| AfterResponse::Continue)
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
                .map(|()| AfterResponse::Continue)
                .map_err(|error| HelperFailure::Internal {
                    detail: error_chain(&error),
                })
        }
        NetworkRequest::ApplyVmPolicy {
            vm_id,
            ipv4,
            ipv6,
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
                ipv6,
                mac,
                egress,
                allow_host_ssh,
                port_forwards,
            };
            firewall::apply_vm_policy(&config.firewall, policy)
                .await
                .map(|()| AfterResponse::Continue)
                .map_err(|error| HelperFailure::Internal {
                    detail: error_chain(&error),
                })
        }
        NetworkRequest::RemoveVmPolicy { vm_id } => {
            firewall::remove_vm_policy(&config.firewall, vm_id)
                .await
                .map(|()| AfterResponse::Continue)
                .map_err(|error| HelperFailure::Internal {
                    detail: error_chain(&error),
                })
        }
        NetworkRequest::CreateTap {
            vm_id,
            micro_network_id,
        } => tap::create_tap(vm_id, micro_network_id)
            .await
            .map(|()| AfterResponse::Continue)
            .map_err(|error| HelperFailure::Internal {
                detail: error_chain(&error),
            }),
        NetworkRequest::DeleteTap { vm_id } => tap::delete_tap(vm_id)
            .await
            .map(|()| AfterResponse::Continue)
            .map_err(|error| HelperFailure::Internal {
                detail: error_chain(&error),
            }),
        NetworkRequest::SyncDhcpLeases {
            revision,
            leases,
            micro_networks,
        } => {
            validate_micro_networks(&micro_networks)?;
            dhcp::sync_dhcp_leases(&config.dhcp, revision, &leases, &micro_networks)
                .await
                .map(|()| AfterResponse::Continue)
                .map_err(|error| HelperFailure::Internal {
                    detail: error_chain(&error),
                })
        }
        NetworkRequest::ApplySelfUpdate {
            tarball_path,
            sha256,
            layout,
        } => self_update::apply_bundle(&layout, &tarball_path, &sha256)
            .await
            .map(|()| AfterResponse::RestartUnits)
            .map_err(|error| {
                let chain = error_chain(&error);
                match error {
                    self_update::SelfUpdateError::Invalid(detail) => {
                        HelperFailure::InvalidRequest { detail }
                    }
                    self_update::SelfUpdateError::Checksum { expected, actual } => {
                        HelperFailure::UpdateChecksumMismatch { expected, actual }
                    }
                    self_update::SelfUpdateError::Apply { restored, .. } => {
                        HelperFailure::UpdateApplyFailed {
                            detail: chain,
                            restored,
                        }
                    }
                }
            }),
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
    use firecrab_helper_protocol::network::Ipv6AddressMode;

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
        let result =
            HelperConfig::from_values("/tmp/x.sock", Some("wheel"), bridge::DEFAULT_BRIDGE_MTU);
        assert_matches!(result, Err(StartupError::InvalidAllowedUid(_)));
    }

    #[test]
    fn wants_teardown_matches_only_the_flag_in_argv1() {
        let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(wants_teardown(&args(&[
            "firecrab-net-helper",
            "--teardown"
        ])));
        assert!(!wants_teardown(&args(&["firecrab-net-helper"])));
        assert!(!wants_teardown(&args(&["firecrab-net-helper", "--other"])));
        // Only argv[1]; a later --teardown doesn't count.
        assert!(!wants_teardown(&args(&[
            "firecrab-net-helper",
            "-x",
            "--teardown"
        ])));
    }

    #[test]
    fn print_failure_walks_the_full_cause_chain() {
        #[derive(Debug)]
        struct Root;
        impl std::fmt::Display for Root {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "root cause")
            }
        }
        impl std::error::Error for Root {}

        #[derive(Debug)]
        struct Top(Root);
        impl std::fmt::Display for Top {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "top-level failure")
            }
        }
        impl std::error::Error for Top {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        // No panic and a real walk over one cause is the assertion; ExitCode
        // has no PartialEq to compare against.
        let _ = print_failure(&Top(Root));
    }

    #[tokio::test]
    async fn run_cli_teardown_mode_exercises_the_whole_dispatch_without_hanging() {
        // Safe to call directly, unlike the default (no-args) path: teardown()
        // never blocks waiting on a signal, while `run()` would hang this test
        // forever if it ever got far enough to call `serve()`. Whichever way
        // this resolves is fine — remove_firewall's nft call needs
        // CAP_NET_ADMIN (same reasoning as
        // firewall::tests::remove_firewall_clears_cached_networks), so an
        // unprivileged test process usually takes the error branch, but
        // either outcome exercises the real dispatch.
        let _ = run_cli(vec![
            "firecrab-net-helper".to_owned(),
            "--teardown".to_owned(),
        ])
        .await;
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
        assert_eq!(
            dispatch(request, &config).await,
            Ok(AfterResponse::Continue)
        );
    }

    #[tokio::test]
    async fn apply_vm_policy_rejects_an_unknown_egress_id_as_invalid_request() {
        let config = HelperConfig::from_values("/tmp/x.sock", None, bridge::DEFAULT_BRIDGE_MTU)
            .expect("helper config");
        let request = NetworkRequest::ApplyVmPolicy {
            vm_id: Uuid::nil(),
            ipv4: "172.30.0.9".parse().unwrap(),
            ipv6: None,
            mac: "02:fc:00:00:00:09".parse().unwrap(),
            egress_policy: "0.0.0.0/0".to_owned(),
            allow_host_ssh: false,
            port_forwards: Vec::new(),
        };
        let result = dispatch(request, &config).await;
        assert_matches!(result, Err(HelperFailure::InvalidRequest { .. }));
    }

    #[tokio::test]
    async fn apply_vm_policy_rejects_a_zero_port_or_unknown_protocol_as_invalid_request() {
        let config = HelperConfig::from_values("/tmp/x.sock", None, bridge::DEFAULT_BRIDGE_MTU)
            .expect("helper config");
        let base = |port_forwards| NetworkRequest::ApplyVmPolicy {
            vm_id: Uuid::nil(),
            ipv4: "172.30.0.9".parse().unwrap(),
            ipv6: None,
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
            let result = dispatch(base(port_forwards), &config).await;
            assert_matches!(result, Err(HelperFailure::InvalidRequest { .. }));
        }
    }

    #[test]
    fn firewall_snapshot_rejects_duplicate_ipv4s_before_nft() {
        let first = VmPolicySpec {
            vm_id: Uuid::from_u128(1),
            ipv4: "172.30.0.40".parse().unwrap(),
            ipv6: None,
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
            ipv6: None,
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
            let result = dispatch(request, &config).await;
            assert_matches!(result, Err(HelperFailure::InvalidRequest { .. }));
        }
    }

    fn ipv6_spec(gateway: &str, prefix: u8) -> MicroNetworkIpv6Spec {
        MicroNetworkIpv6Spec {
            gateway: gateway.parse().unwrap(),
            prefix,
            address_mode: Ipv6AddressMode::Slaac,
        }
    }

    #[test]
    fn an_ipv6_prefix_that_is_not_a_64_is_rejected() {
        // SLAAC's EUI-64 interface identifier is 64 bits wide, so anything
        // else silently produces addresses no guest would configure.
        for prefix in [0, 48, 56, 63, 65, 128] {
            assert_matches!(
                validate_ipv6(&ipv6_spec("fd00:1::1", prefix)),
                Err(HelperFailure::InvalidRequest { .. }),
                "{prefix}"
            );
        }
        assert!(validate_ipv6(&ipv6_spec("fd00:1::1", 64)).is_ok());
        assert!(validate_ipv6(&ipv6_spec("2001:db8:1::1", 64)).is_ok());
    }

    #[test]
    fn an_ipv6_gateway_outside_global_or_unique_local_space_is_rejected() {
        // Allowlist: Unique Local (fc00::/7) or global unicast (2000::/3).
        // A denylist would still accept deprecated site-local, discard-only
        // and NAT64 prefixes, none of which can back a MicroNetwork.
        for gateway in [
            "fe80::1",
            "ff02::1",
            "::1",
            "::",
            "fec0::1",
            "100::1",
            "64:ff9b::1",
        ] {
            assert_matches!(
                validate_ipv6(&ipv6_spec(gateway, 64)),
                Err(HelperFailure::InvalidRequest { .. }),
                "{gateway}"
            );
        }
    }

    #[test]
    fn firewall_snapshot_rejects_duplicate_ipv6s_before_nft() {
        let first = VmPolicySpec {
            vm_id: Uuid::from_u128(1),
            ipv4: "172.30.0.40".parse().unwrap(),
            ipv6: Some("fd00:1::5".parse().unwrap()),
            mac: "02:fc:00:00:00:01".parse().unwrap(),
            egress_policy: "internet".to_owned(),
            allow_host_ssh: false,
            port_forwards: Vec::new(),
        };
        let second = VmPolicySpec {
            vm_id: Uuid::from_u128(2),
            ipv4: "172.30.0.41".parse().unwrap(),
            mac: "02:fc:00:00:00:02".parse().unwrap(),
            ..first.clone()
        };

        assert_matches!(validate_vm_policies(vec![first, second]),
            Err(HelperFailure::InvalidRequest { detail }) if detail.contains("duplicate VM policy IPv6"));
    }

    #[tokio::test]
    async fn ensure_micro_network_bridge_rejects_a_bad_ipv6_plan_as_invalid_request() {
        let config = HelperConfig::from_values("/tmp/x.sock", None, bridge::DEFAULT_BRIDGE_MTU)
            .expect("helper config");
        let request = NetworkRequest::EnsureMicroNetworkBridge {
            micro_network_id: Uuid::nil(),
            gateway: "172.31.0.1".parse().unwrap(),
            prefix: 24,
            ipv6: Some(ipv6_spec("fe80::1", 64)),
        };
        assert_matches!(
            dispatch(request, &config).await,
            Err(HelperFailure::InvalidRequest { .. })
        );
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
                ipv6: None,
            };
            let result = dispatch(request, &config).await;
            assert_matches!(result, Err(HelperFailure::InvalidRequest { .. }));
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

    /// A tempdir layout plus a real bundle, for the ApplySelfUpdate dispatch
    /// tests below. The layout is the one `self_update::host_layout` resolves
    /// for `PREFIX={dir}`, because `dispatch` now rejects any layout that is
    /// not this host's real install — the caller must therefore also hold a
    /// `HostLayoutEnv` pointing `PREFIX` at the same tempdir.
    fn self_update_fixture(dir: &tempfile::TempDir) -> (PathBuf, String, InstallLayout) {
        use std::io::Write as _;

        let root = dir.path();
        let layout = self_update::test_support::layout_for_prefix(root);
        let updates = root.join("updates/job");
        std::fs::create_dir_all(&updates).expect("create download dir");
        let tarball = updates.join("firecrab-host-x86_64-gnu.tar.gz");
        {
            let encoder = flate2::write::GzEncoder::new(
                std::fs::File::create(&tarball).expect("create tarball"),
                flate2::Compression::fast(),
            );
            let mut builder = tar::Builder::new(encoder);
            for (name, bytes) in [
                ("firecrab-api", b"api" as &[u8]),
                ("firecrab-net-helper", b"helper"),
                ("firecrab", b"cli"),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                builder
                    .append_data(&mut header, name, bytes)
                    .expect("append");
            }
            let mut file = builder
                .into_inner()
                .expect("finish tar")
                .finish()
                .expect("finish gzip");
            file.flush().expect("flush");
        }
        let digest = {
            use sha2::{Digest, Sha256};
            let bytes = std::fs::read(&tarball).expect("read tarball");
            format!("{:x}", Sha256::digest(&bytes))
        };
        (tarball, digest, layout)
    }

    /// A current-thread runtime for the ApplySelfUpdate dispatch tests. They
    /// hold `HostLayoutEnv`'s process-wide env lock for their whole body, and a
    /// `MutexGuard` may not be held across a bare `.await` — `block_on` is a
    /// plain blocking call, so the guard never crosses a suspension point.
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build runtime")
    }

    #[test]
    fn apply_self_update_rejects_a_relative_tarball_path() {
        let config = HelperConfig::from_values("/tmp/x.sock", None, bridge::DEFAULT_BRIDGE_MTU)
            .expect("helper config");
        let dir = short_tempdir();
        let (_, digest, layout) = self_update_fixture(&dir);
        let _env = self_update::test_support::HostLayoutEnv::set(dir.path());
        let request = NetworkRequest::ApplySelfUpdate {
            tarball_path: PathBuf::from("bundle.tar.gz"),
            sha256: digest,
            layout,
        };
        assert_matches!(
            runtime().block_on(dispatch(request, &config)),
            Err(HelperFailure::InvalidRequest { .. })
        );
    }

    #[test]
    fn apply_self_update_rejects_a_layout_that_is_not_this_hosts_install() {
        // The helper derives its own layout from PREFIX and treats the wire
        // field as a cross-check. Without this, the one unprivileged account
        // the socket admits could aim root-owned writes anywhere it liked.
        let config = HelperConfig::from_values("/tmp/x.sock", None, bridge::DEFAULT_BRIDGE_MTU)
            .expect("helper config");
        let dir = short_tempdir();
        let (tarball, digest, _) = self_update_fixture(&dir);
        let elsewhere = short_tempdir();
        let layout = self_update::test_support::layout_for_prefix(elsewhere.path());
        let _env = self_update::test_support::HostLayoutEnv::set(dir.path());
        let request = NetworkRequest::ApplySelfUpdate {
            tarball_path: tarball,
            sha256: digest,
            layout,
        };
        assert_matches!(
            runtime().block_on(dispatch(request, &config)),
            Err(HelperFailure::InvalidRequest { .. })
        );
    }

    #[test]
    fn apply_self_update_asks_the_loop_to_restart_after_responding() {
        // dispatch must only *report* that a restart is due — it must never
        // run systemctl itself, which is exactly why this test can call it.
        let config = HelperConfig::from_values("/tmp/x.sock", None, bridge::DEFAULT_BRIDGE_MTU)
            .expect("helper config");
        let dir = short_tempdir();
        let (tarball, digest, layout) = self_update_fixture(&dir);
        let _env = self_update::test_support::HostLayoutEnv::set(dir.path());
        let request = NetworkRequest::ApplySelfUpdate {
            tarball_path: tarball,
            sha256: digest,
            layout,
        };
        assert_eq!(
            runtime().block_on(dispatch(request, &config)),
            Ok(AfterResponse::RestartUnits)
        );
    }

    #[test]
    fn a_self_update_response_is_written_before_the_connection_closes() {
        // A checksum mismatch fails *after* full validation and before any
        // restart, so this exercises the real response path without ever
        // reaching AfterResponse::RestartUnits.
        let dir = short_tempdir();
        let fixture_dir = short_tempdir();
        let (tarball, _, layout) = self_update_fixture(&fixture_dir);
        let _env = self_update::test_support::HostLayoutEnv::set(fixture_dir.path());

        runtime().block_on(async {
            let (path, stop, handle) = start_helper(&dir);
            let mut stream = UnixStream::connect(&path).await.expect("connect");
            let envelope = NetworkRequestEnvelope::new(
                Uuid::new_v4(),
                NetworkRequest::ApplySelfUpdate {
                    tarball_path: tarball,
                    sha256: "0".repeat(64),
                    layout,
                },
            );
            write_frame(&mut stream, &envelope)
                .await
                .expect("send request");
            let response: NetworkResponseEnvelope =
                read_frame(&mut stream).await.expect("receive response");
            assert_eq!(response.request_id, envelope.request_id);
            assert_matches!(
                response.result,
                Err(HelperFailure::UpdateChecksumMismatch { .. })
            );

            // The connection is still usable: a failed apply is not a restart.
            let next = NetworkRequestEnvelope::new(
                Uuid::new_v4(),
                NetworkRequest::DeleteTap {
                    vm_id: Uuid::new_v4(),
                },
            );
            write_frame(&mut stream, &next)
                .await
                .expect("send follow-up");
            let response: NetworkResponseEnvelope =
                read_frame(&mut stream).await.expect("receive follow-up");
            assert_eq!(response.result, Ok(()));

            drop(stop);
            handle.await.expect("helper task");
        });
    }
}

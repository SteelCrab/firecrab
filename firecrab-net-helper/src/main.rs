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
/// Per-VM and global nftables firewall rules.
mod firewall;

use firecrab_helper_protocol::PROTOCOL_VERSION;
use firecrab_helper_protocol::framing::{read_frame, write_frame};
use firecrab_helper_protocol::network::{
    HelperFailure, NetworkRequest, NetworkRequestEnvelope, NetworkResponseEnvelope,
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
}

/// Resolved startup configuration plus the shared actors every connection
/// dispatches into.
#[derive(Debug)]
struct HelperConfig {
    /// Where the Unix socket is bound.
    socket_path: PathBuf,
    /// UIDs allowed to connect, checked via `SO_PEERCRED`.
    allowed_peer_uids: HashSet<u32>,
    /// Shared firewall state (single-writer mutex inside).
    firewall: firewall::FirewallActor,
    /// Shared bridge-creation state (single-writer mutex inside).
    bridge: bridge::BridgeActor,
}

impl HelperConfig {
    /// Reads configuration from the process environment.
    fn load() -> Result<Self, StartupError> {
        let socket_path =
            env::var("FIRECRAB_NET_HELPER_SOCK").unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_owned());
        let allowed_uid = env::var("FIRECRAB_NET_HELPER_ALLOWED_UID").ok();
        Self::from_values(&socket_path, allowed_uid.as_deref())
    }

    /// Builds config from already-parsed values (used directly by tests).
    fn from_values(socket_path: &str, allowed_uid: Option<&str>) -> Result<Self, StartupError> {
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
            firewall: firewall::FirewallActor::new(),
            bridge: bridge::BridgeActor::new(),
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
    let config = Arc::new(HelperConfig::load()?);
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

/// Routes a validated request to the matching bridge/firewall operation.
async fn dispatch(request: NetworkRequest, config: &HelperConfig) -> Result<(), HelperFailure> {
    match request {
        NetworkRequest::EnsureBridge => {
            bridge::ensure_bridge(&config.bridge)
                .await
                .map_err(|error| HelperFailure::Internal {
                    detail: error_chain(&error),
                })
        }
        NetworkRequest::EnsureFirewall => firewall::ensure_firewall(&config.firewall)
            .await
            .map_err(|error| HelperFailure::Internal {
                detail: error_chain(&error),
            }),
        NetworkRequest::ApplyVmPolicy {
            vm_id,
            ipv4,
            mac,
            egress_policy,
            allow_host_ssh,
        } => {
            // Resolve the API-supplied egress ID against the helper's own
            // allowlist; an unknown ID is a client error, not an internal one.
            let egress = firewall::EgressPolicy::from_id(&egress_policy).ok_or_else(|| {
                HelperFailure::InvalidRequest {
                    detail: format!("unknown egress policy id {egress_policy:?}"),
                }
            })?;
            let policy = firewall::VmPolicy {
                vm_id,
                ipv4,
                mac,
                egress,
                allow_host_ssh,
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
        NetworkRequest::CreateTap { .. } | NetworkRequest::DeleteTap { .. } => {
            Err(HelperFailure::UnsupportedOperation)
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
            HelperConfig::from_values(path.to_str().expect("utf-8 path"), None)
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
        let config = HelperConfig::from_values("/tmp/x.sock", Some("12345")).expect("config");
        assert!(config.peer_allowed(effective_uid()));
        assert!(config.peer_allowed(12345));
        assert!(!config.peer_allowed(54321));
    }

    #[test]
    fn non_numeric_allowed_uid_is_rejected() {
        assert!(matches!(
            HelperConfig::from_values("/tmp/x.sock", Some("wheel")),
            Err(StartupError::InvalidAllowedUid(_))
        ));
    }

    #[tokio::test]
    async fn unimplemented_operations_are_rejected() {
        // Only TAP creation/deletion is still unimplemented; the policy
        // operations are handled (RemoveVmPolicy is a no-op when nothing is
        // installed, so it does not reach nft here).
        let config = HelperConfig::from_values("/tmp/x.sock", None).expect("helper config");
        let requests = [
            NetworkRequest::CreateTap { vm_id: Uuid::nil() },
            NetworkRequest::DeleteTap { vm_id: Uuid::nil() },
        ];
        for request in requests {
            assert_eq!(
                dispatch(request, &config).await,
                Err(HelperFailure::UnsupportedOperation)
            );
        }
    }

    #[tokio::test]
    async fn apply_vm_policy_rejects_an_unknown_egress_id_as_invalid_request() {
        let config = HelperConfig::from_values("/tmp/x.sock", None).expect("helper config");
        let request = NetworkRequest::ApplyVmPolicy {
            vm_id: Uuid::nil(),
            ipv4: "172.30.0.9".parse().unwrap(),
            mac: "02:fc:00:00:00:09".parse().unwrap(),
            egress_policy: "0.0.0.0/0".to_owned(),
            allow_host_ssh: false,
        };
        assert!(matches!(
            dispatch(request, &config).await,
            Err(HelperFailure::InvalidRequest { .. })
        ));
    }

    #[tokio::test]
    async fn serves_multiple_requests_per_connection() {
        let dir = short_tempdir();
        let (path, stop, handle) = start_helper(&dir);

        let mut stream = UnixStream::connect(&path).await.expect("connect");
        for _ in 0..2 {
            // DeleteTap answers deterministically without touching netlink,
            // so the framing loop is testable without privileges.
            let envelope = NetworkRequestEnvelope::new(
                Uuid::new_v4(),
                NetworkRequest::DeleteTap { vm_id: Uuid::nil() },
            );
            write_frame(&mut stream, &envelope)
                .await
                .expect("send request");
            let response: NetworkResponseEnvelope =
                read_frame(&mut stream).await.expect("receive response");
            assert_eq!(response.version, PROTOCOL_VERSION);
            assert_eq!(response.request_id, envelope.request_id);
            assert_eq!(response.result, Err(HelperFailure::UnsupportedOperation));
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

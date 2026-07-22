use std::env;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::time::Duration;

use firecrab_helper_protocol::framing::{FrameError, read_frame, write_frame};
pub use firecrab_helper_protocol::network::tap_name;
use firecrab_helper_protocol::network::{
    DhcpLeaseEntry, HelperFailure, MacAddr, NetworkRequest, NetworkRequestEnvelope,
    NetworkResponseEnvelope,
};
use thiserror::Error;
use tokio::net::UnixStream;
use uuid::Uuid;

use crate::network_policy::EgressPolicy;

pub const DEFAULT_HELPER_SOCKET: &str = "/run/firecrab/net-helper.sock";
const HELPER_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("network helper is unavailable at {path}")]
    Unavailable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("network helper did not answer in time")]
    Timeout,
    #[error("network helper connection failed")]
    Frame(#[from] FrameError),
    #[error("network helper answered for a different request")]
    MismatchedResponse,
    #[error("network helper rejected the request")]
    Helper(#[source] HelperFailure),
}

/// Client for the privileged firecrab-net-helper; one connection per call.
#[derive(Debug, Clone)]
pub struct NetworkClient {
    socket_path: PathBuf,
    timeout: Duration,
}

impl NetworkClient {
    pub fn from_env() -> Self {
        let socket_path = env::var("FIRECRAB_NET_HELPER_SOCK")
            .map_or_else(|_| PathBuf::from(DEFAULT_HELPER_SOCKET), PathBuf::from);
        Self {
            socket_path,
            timeout: HELPER_TIMEOUT,
        }
    }

    pub async fn call(&self, request: NetworkRequest) -> Result<(), NetworkError> {
        tokio::time::timeout(self.timeout, self.exchange(request))
            .await
            .map_err(|_| NetworkError::Timeout)?
    }

    /// Idempotently ensures the shared bridge exists.
    pub async fn ensure_bridge(&self) -> Result<(), NetworkError> {
        self.call(NetworkRequest::EnsureBridge).await
    }

    /// Idempotently (re)applies the owned nftables tables.
    pub async fn ensure_firewall(&self) -> Result<(), NetworkError> {
        self.call(NetworkRequest::EnsureFirewall).await
    }

    /// Creates `vm_id`'s TAP device, attaches it to the bridge, and returns
    /// its deterministic name (also derivable locally via [`tap_name`], so
    /// callers that already know it don't have to wait on this to build a
    /// Firecracker config referencing it).
    pub async fn create_tap(&self, vm_id: Uuid) -> Result<String, NetworkError> {
        self.call(NetworkRequest::CreateTap { vm_id }).await?;
        Ok(tap_name(vm_id))
    }

    /// Removes `vm_id`'s TAP device; a no-op if it's already gone.
    pub async fn delete_tap(&self, vm_id: Uuid) -> Result<(), NetworkError> {
        self.call(NetworkRequest::DeleteTap { vm_id }).await
    }

    /// Applies `vm_id`'s isolation + egress firewall policy for its lease.
    pub async fn apply_vm_policy(
        &self,
        vm_id: Uuid,
        ipv4: Ipv4Addr,
        mac: MacAddr,
        egress_policy: EgressPolicy,
        allow_host_ssh: bool,
    ) -> Result<(), NetworkError> {
        self.call(NetworkRequest::ApplyVmPolicy {
            vm_id,
            ipv4,
            mac,
            egress_policy: egress_policy.id().to_owned(),
            allow_host_ssh,
        })
        .await
    }

    /// Removes `vm_id`'s firewall policy; a no-op if none is installed.
    pub async fn remove_vm_policy(&self, vm_id: Uuid) -> Result<(), NetworkError> {
        self.call(NetworkRequest::RemoveVmPolicy { vm_id }).await
    }

    /// Replaces the DHCP reservation snapshot with `leases` in full,
    /// tagged with `revision` (see `Store::lease_revision`) so the helper
    /// can ignore a stale/out-of-order snapshot instead of applying it.
    pub async fn sync_dhcp_leases(
        &self,
        revision: u64,
        leases: Vec<DhcpLeaseEntry>,
    ) -> Result<(), NetworkError> {
        self.call(NetworkRequest::SyncDhcpLeases { revision, leases })
            .await
    }

    /// Builds a client pointed at an explicit socket path, for lifecycle
    /// tests that spin up a fake net-helper (see `test_support`).
    #[cfg(test)]
    pub(crate) fn with_socket_path(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            timeout: HELPER_TIMEOUT,
        }
    }

    async fn exchange(&self, request: NetworkRequest) -> Result<(), NetworkError> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .await
            .map_err(|source| NetworkError::Unavailable {
                path: self.socket_path.clone(),
                source,
            })?;

        let envelope = NetworkRequestEnvelope::new(Uuid::new_v4(), request);
        write_frame(&mut stream, &envelope).await?;

        let response: NetworkResponseEnvelope = read_frame(&mut stream).await?;
        if response.request_id != envelope.request_id {
            return Err(NetworkError::MismatchedResponse);
        }
        response.result.map_err(NetworkError::Helper)
    }
}

/// Test-only stand-in for the privileged net-helper daemon, for lifecycle
/// tests elsewhere in the crate that need `NetworkClient` calls to succeed
/// without a real (privileged) helper process.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use firecrab_helper_protocol::PROTOCOL_VERSION;
    use tokio::net::UnixListener;

    use super::*;

    /// Spawns a fake net-helper bound to `socket_path` that answers every
    /// request with `Ok(())`, looping until the returned task is dropped —
    /// tied to the test's own tokio runtime, so it needs no explicit
    /// shutdown.
    pub fn spawn_always_ok_helper(socket_path: &Path) -> tokio::task::JoinHandle<()> {
        let listener = UnixListener::bind(socket_path).expect("bind fake net-helper socket");
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(serve_always_ok(stream));
            }
        })
    }

    async fn serve_always_ok(mut stream: UnixStream) {
        loop {
            let envelope: NetworkRequestEnvelope = match read_frame(&mut stream).await {
                Ok(envelope) => envelope,
                Err(_) => return,
            };
            let response = NetworkResponseEnvelope {
                version: PROTOCOL_VERSION,
                request_id: envelope.request_id,
                result: Ok(()),
            };
            if write_frame(&mut stream, &response).await.is_err() {
                return;
            }
        }
    }

    /// Short name for each request variant, for tests asserting a call
    /// sequence rather than each request's full payload.
    fn operation_name(request: &NetworkRequest) -> &'static str {
        match request {
            NetworkRequest::EnsureBridge => "ensure_bridge",
            NetworkRequest::EnsureFirewall => "ensure_firewall",
            NetworkRequest::CreateTap { .. } => "create_tap",
            NetworkRequest::DeleteTap { .. } => "delete_tap",
            NetworkRequest::ApplyVmPolicy { .. } => "apply_vm_policy",
            NetworkRequest::RemoveVmPolicy { .. } => "remove_vm_policy",
            NetworkRequest::SyncDhcpLeases { .. } => "sync_dhcp_leases",
        }
    }

    /// Spawns a fake net-helper that records each request's operation name
    /// (in arrival order) and answers `fail_operation` (if any) with an
    /// error while answering everything else `Ok` — for tests asserting a
    /// call sequence, whether that's a compensation path (e.g. "a failed
    /// apply_vm_policy is still followed by remove_vm_policy and
    /// delete_tap") or just that some path was reached at all (`None`).
    pub fn spawn_recording_helper(
        socket_path: &Path,
        fail_operation: Option<&'static str>,
    ) -> (tokio::task::JoinHandle<()>, Arc<Mutex<Vec<&'static str>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let log_for_task = Arc::clone(&log);
        let listener = UnixListener::bind(socket_path).expect("bind fake net-helper socket");
        let handle = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(serve_recording(
                    stream,
                    Arc::clone(&log_for_task),
                    fail_operation,
                ));
            }
        });
        (handle, log)
    }

    async fn serve_recording(
        mut stream: UnixStream,
        log: Arc<Mutex<Vec<&'static str>>>,
        fail_operation: Option<&'static str>,
    ) {
        loop {
            let envelope: NetworkRequestEnvelope = match read_frame(&mut stream).await {
                Ok(envelope) => envelope,
                Err(_) => return,
            };
            let operation = operation_name(&envelope.request);
            log.lock().unwrap().push(operation);
            let result = if fail_operation == Some(operation) {
                Err(HelperFailure::Internal {
                    detail: "forced failure for test".to_owned(),
                })
            } else {
                Ok(())
            };
            let response = NetworkResponseEnvelope {
                version: PROTOCOL_VERSION,
                request_id: envelope.request_id,
                result,
            };
            if write_frame(&mut stream, &response).await.is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;

    use firecrab_helper_protocol::PROTOCOL_VERSION;
    use tokio::net::UnixListener;

    // Unix socket paths are limited to ~108 bytes; keep test sockets short.
    fn short_tempdir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("fc-net")
            .tempdir_in("/tmp")
            .expect("create tempdir")
    }

    fn client(path: &Path, timeout: Duration) -> NetworkClient {
        NetworkClient {
            socket_path: path.to_owned(),
            timeout,
        }
    }

    fn fake_helper(
        listener: UnixListener,
        respond: impl FnOnce(NetworkRequestEnvelope) -> NetworkResponseEnvelope + Send + 'static,
    ) {
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let envelope: NetworkRequestEnvelope =
                read_frame(&mut stream).await.expect("read request");
            write_frame(&mut stream, &respond(envelope))
                .await
                .expect("write response");
        });
    }

    #[tokio::test]
    async fn call_round_trips_a_successful_response() {
        let dir = short_tempdir();
        let path = dir.path().join("helper.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        fake_helper(listener, |envelope| NetworkResponseEnvelope {
            version: PROTOCOL_VERSION,
            request_id: envelope.request_id,
            result: Ok(()),
        });

        let result = client(&path, HELPER_TIMEOUT)
            .call(NetworkRequest::EnsureBridge)
            .await;
        assert!(result.is_ok(), "{result:?}");
    }

    #[tokio::test]
    async fn helper_failures_surface_as_helper_errors() {
        let dir = short_tempdir();
        let path = dir.path().join("helper.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        fake_helper(listener, |envelope| NetworkResponseEnvelope {
            version: PROTOCOL_VERSION,
            request_id: envelope.request_id,
            result: Err(HelperFailure::UnsupportedOperation),
        });

        let result = client(&path, HELPER_TIMEOUT)
            .call(NetworkRequest::EnsureFirewall)
            .await;
        assert!(matches!(
            result,
            Err(NetworkError::Helper(HelperFailure::UnsupportedOperation))
        ));
    }

    #[tokio::test]
    async fn responses_for_other_requests_are_rejected() {
        let dir = short_tempdir();
        let path = dir.path().join("helper.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        fake_helper(listener, |_| NetworkResponseEnvelope {
            version: PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            result: Ok(()),
        });

        let result = client(&path, HELPER_TIMEOUT)
            .call(NetworkRequest::EnsureBridge)
            .await;
        assert!(matches!(result, Err(NetworkError::MismatchedResponse)));
    }

    #[tokio::test]
    async fn missing_socket_reports_unavailable() {
        let dir = short_tempdir();
        let path = dir.path().join("absent.sock");

        let result = client(&path, HELPER_TIMEOUT)
            .call(NetworkRequest::EnsureBridge)
            .await;
        assert!(matches!(result, Err(NetworkError::Unavailable { .. })));
    }

    #[tokio::test]
    async fn unresponsive_helper_times_out() {
        let dir = short_tempdir();
        let path = dir.path().join("helper.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        tokio::spawn(async move {
            // Accept and hold the connection without ever answering.
            let (stream, _) = listener.accept().await.expect("accept");
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(stream);
        });

        let result = client(&path, Duration::from_millis(100))
            .call(NetworkRequest::EnsureBridge)
            .await;
        assert!(matches!(result, Err(NetworkError::Timeout)));
    }
}

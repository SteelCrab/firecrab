//! The `firecrab-net-helper` client used by `firecrab update --apply`.
//!
//! `firecrab-helper-protocol`'s framing is async-only, so this is the one part
//! of the CLI that spins up a tokio runtime; everything else stays synchronous.

use std::path::{Path, PathBuf};
use std::time::Duration;

use firecrab_helper_protocol::framing::{read_frame, write_frame};
use firecrab_helper_protocol::network::{
    InstallLayout, NetworkRequest, NetworkRequestEnvelope, NetworkResponseEnvelope,
};
use tokio::net::UnixStream;
use tokio::time::timeout;
use uuid::Uuid;

use super::UpdateError;

/// Same default as `firecrab-api/src/network.rs`'s `DEFAULT_HELPER_SOCKET`.
pub const DEFAULT_HELPER_SOCKET: &str = "/run/firecrab/net-helper.sock";

/// Extraction plus the swap is far past the API's 5s helper budget, so this
/// path gets its own, much larger bound.
pub const APPLY_TIMEOUT: Duration = Duration::from_secs(300);

/// `FIRECRAB_NET_HELPER_SOCK`, else [`DEFAULT_HELPER_SOCKET`].
pub fn helper_socket_path() -> PathBuf {
    std::env::var("FIRECRAB_NET_HELPER_SOCK")
        .ok()
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from(DEFAULT_HELPER_SOCKET), PathBuf::from)
}

/// Sends one `ApplySelfUpdate` and waits for its answer.
///
/// An EOF with no response frame is reported as
/// [`UpdateError::HelperClosedWithoutAnswering`]: the helper closes silently
/// both when `peer_allowed` refuses this uid and when an older helper cannot
/// parse the new request tag, and nothing on this side can tell the two apart.
pub fn send_apply_self_update(
    socket: &Path,
    tarball_path: &Path,
    sha256: &str,
    layout: InstallLayout,
) -> Result<(), UpdateError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build a current-thread tokio runtime");

    runtime.block_on(async move {
        let mut stream =
            UnixStream::connect(socket)
                .await
                .map_err(|_| UpdateError::HelperUnavailable {
                    path: socket.display().to_string(),
                })?;
        let envelope = NetworkRequestEnvelope::new(
            Uuid::new_v4(),
            NetworkRequest::ApplySelfUpdate {
                tarball_path: tarball_path.to_path_buf(),
                sha256: sha256.to_owned(),
                layout,
            },
        );
        write_frame(&mut stream, &envelope)
            .await
            .map_err(|_| UpdateError::HelperClosedWithoutAnswering)?;

        let response: NetworkResponseEnvelope =
            match timeout(APPLY_TIMEOUT, read_frame(&mut stream)).await {
                Err(_) => return Err(UpdateError::Timeout(APPLY_TIMEOUT.as_secs())),
                Ok(Err(_)) => return Err(UpdateError::HelperClosedWithoutAnswering),
                Ok(Ok(response)) => response,
            };
        response.result.map_err(UpdateError::HelperRejected)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::ENV_LOCK;

    #[test]
    fn helper_socket_path_prefers_the_env_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by ENV_LOCK against every other env-touching test.
        unsafe { std::env::set_var("FIRECRAB_NET_HELPER_SOCK", "/tmp/fc-test.sock") };
        let overridden = helper_socket_path();
        unsafe { std::env::remove_var("FIRECRAB_NET_HELPER_SOCK") };
        let default = helper_socket_path();

        assert_eq!(overridden, std::path::Path::new("/tmp/fc-test.sock"));
        assert_eq!(default, std::path::Path::new(DEFAULT_HELPER_SOCKET));
    }

    #[test]
    fn send_apply_self_update_reports_an_unavailable_helper() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("absent.sock");
        let layout = InstallLayout {
            bindir: dir.path().join("bin"),
            libdir: dir.path().join("lib"),
            sharedir: dir.path().join("share"),
        };
        let error = send_apply_self_update(
            &socket,
            &dir.path().join("bundle.tar.gz"),
            &"a".repeat(64),
            layout,
        )
        .unwrap_err();
        assert!(
            matches!(&error, UpdateError::HelperUnavailable { path } if path.contains("absent.sock")),
            "{error}"
        );
    }
}

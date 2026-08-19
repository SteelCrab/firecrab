//! `firecrab update`: check GitHub Releases for a newer host build, download
//! and verify the matching host bundle, and hand the privileged swap to
//! `firecrab-net-helper`.
//!
//! The split is a privilege boundary, not a stylistic one: everything in this
//! module runs unprivileged and never writes to `$LIBDIR`, `$PREFIX/bin` or
//! `$SHAREDIR`, and never calls `systemctl`.

/// arch/libc detection, asset naming, download and SHA-256 verification.
pub mod bundle;

/// GitHub Releases lookup and version comparison.
pub mod check;

/// Serializes the tests across this module tree that read or write the real
/// process environment (`FIRECRAB_LIBC`, `FIRECRAB_RELEASE_REPO`,
/// `FIRECRAB_RELEASE_API`, `FIRECRAB_RELEASE_BASE`, `PREFIX`,
/// `FIRECRAB_LIBDIR`, `DATADIR`) — `set_var` is process-wide, so without one
/// shared lock they race under `cargo test`'s parallel runner.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Everything `firecrab update` can fail at, so `run_update` renders one
/// actionable line per cause instead of a bare `Box<dyn Error>`.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// `std::env::consts::ARCH` is neither `x86_64` nor `aarch64`.
    #[error("unsupported architecture: {0} (need x86_64 or aarch64)")]
    UnsupportedArch(String),
    /// `FIRECRAB_LIBC` was set to something other than gnu/glibc/musl.
    #[error("unsupported libc: {0} (need gnu or musl)")]
    UnsupportedLibc(String),
    /// The release check never produced a usable `tag_name`.
    #[error("release check failed: {0}")]
    Check(String),
    /// An asset (bundle or `SHA256SUMS`) could not be downloaded.
    #[error("failed to download {url}: {detail}")]
    Download {
        /// The asset URL that failed.
        url: String,
        /// Transport-level detail from reqwest.
        detail: String,
    },
    /// The downloaded bundle's hash didn't match the release's `SHA256SUMS`.
    #[error("checksum mismatch for {asset}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        /// Asset file name.
        asset: String,
        /// Hash the release published.
        expected: String,
        /// Hash of the file that landed on disk.
        actual: String,
    },
    /// The helper socket could not be connected at all.
    #[error("network helper is unavailable at {path}")]
    HelperUnavailable {
        /// The socket path that was tried.
        path: String,
    },
    /// The helper accepted the connection and then closed it without ever
    /// writing a response frame. Two causes are indistinguishable from this
    /// side, so the message names both: `peer_allowed` refused this uid, or
    /// the installed helper predates the `apply_self_update` request tag.
    #[error(
        "the network helper closed the connection without answering — run as root \
         or the firecrab service account, and re-run install.sh if the helper is older \
         than this CLI"
    )]
    HelperClosedWithoutAnswering,
    /// The helper answered, and its answer was a failure.
    #[error("network helper rejected the update: {0}")]
    HelperRejected(#[source] firecrab_helper_protocol::network::HelperFailure),
    /// The helper did not answer within the apply timeout.
    #[error("network helper did not answer within {0} seconds")]
    Timeout(u64),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_error_messages_name_the_actionable_fix() {
        assert_eq!(
            UpdateError::UnsupportedArch("riscv64".to_owned()).to_string(),
            "unsupported architecture: riscv64 (need x86_64 or aarch64)"
        );
        assert_eq!(
            UpdateError::UnsupportedLibc("uclibc".to_owned()).to_string(),
            "unsupported libc: uclibc (need gnu or musl)"
        );
        assert_eq!(
            UpdateError::Check("unreachable: refused".to_owned()).to_string(),
            "release check failed: unreachable: refused"
        );
        assert_eq!(
            UpdateError::ChecksumMismatch {
                asset: "firecrab-host-x86_64-gnu.tar.gz".to_owned(),
                expected: "aa".to_owned(),
                actual: "bb".to_owned(),
            }
            .to_string(),
            "checksum mismatch for firecrab-host-x86_64-gnu.tar.gz: expected aa, got bb"
        );
        assert_eq!(
            UpdateError::HelperUnavailable {
                path: "/run/firecrab/net-helper.sock".to_owned()
            }
            .to_string(),
            "network helper is unavailable at /run/firecrab/net-helper.sock"
        );
        let closed = UpdateError::HelperClosedWithoutAnswering.to_string();
        assert!(closed.contains("run as root"), "{closed}");
        assert!(closed.contains("install.sh"), "{closed}");
        assert_eq!(
            UpdateError::Timeout(300).to_string(),
            "network helper did not answer within 300 seconds"
        );
    }
}

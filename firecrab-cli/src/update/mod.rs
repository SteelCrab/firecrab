//! `firecrab update`: check GitHub Releases for a newer host build, download
//! and verify the matching host bundle, and hand the privileged swap to
//! `firecrab-helper`.
//!
//! The split is a privilege boundary, not a stylistic one: everything in this
//! module runs unprivileged and never writes to `$LIBDIR`, `$PREFIX/bin` or
//! `$SHAREDIR`, and never calls `systemctl`.

/// arch/libc detection, asset naming, download and SHA-256 verification.
pub mod bundle;

/// GitHub Releases lookup and version comparison.
pub mod check;

/// The stage lines, gauge and record file of an `--apply` run.
pub mod progress;

/// Unix-socket client that sends `ApplySelfUpdate` to `firecrab-helper`.
pub mod helper;

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

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use firecrab_api_types::{UpdateCheckResponse, UpdatePhase};
use firecrab_helper_protocol::network::InstallLayout;

use progress::{Progress, format_bytes};

/// A completed release check: the shared wire report plus the raw tag the
/// download URL needs (`report.latest` has its `v` stripped for display, and
/// the asset URL must use the tag exactly as published).
#[derive(Debug, Clone)]
pub struct CheckOutcome {
    /// The report `--json` prints and `GET /api/update` relays.
    pub report: UpdateCheckResponse,
    /// The newest release's raw `tag_name`, when the check succeeded.
    pub tag: Option<String>,
}

/// What an `--apply` run actually did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// The host already runs the newest release; nothing was downloaded.
    AlreadyCurrent,
    /// The helper accepted the bundle and both services are restarting.
    Applied {
        /// The version now installed.
        version: String,
    },
}

/// `$PREFIX` (else `/usr/local`), resolved exactly the way `firecrab info`
/// does, with `FIRECRAB_LIBDIR` overriding only `libdir` — the same override
/// `firecrab doctor` already honours.
///
/// **Must stay in sync with `firecrab-helper`'s
/// `self_update::host_layout`.** The helper does not write where this points:
/// it resolves the same three paths from its *own* environment and rejects the
/// request unless the two agree byte-for-byte, so any change to the rules here
/// has to be made there too or every `--apply` starts failing with
/// "layout is not this host's install". Both processes get `PREFIX` and
/// `FIRECRAB_LIBDIR` from their systemd units, rendered from the same
/// `install.sh` values.
pub fn resolve_layout() -> InstallLayout {
    let prefix = PathBuf::from(std::env::var("PREFIX").unwrap_or_else(|_| "/usr/local".to_owned()));
    let libdir = std::env::var("FIRECRAB_LIBDIR")
        .ok()
        .filter(|value| !value.is_empty())
        .map_or_else(|| prefix.join("lib/firecrab"), PathBuf::from);
    InstallLayout {
        bindir: prefix.join("bin"),
        libdir,
        sharedir: prefix.join("share/firecrab"),
    }
}

/// `$DATADIR`, else `install.sh`'s `/var/lib/firecrab`.
pub fn datadir() -> PathBuf {
    PathBuf::from(std::env::var("DATADIR").unwrap_or_else(|_| "/var/lib/firecrab".to_owned()))
}

/// Runs the release check. Never fails: a failure is reported *in* the report
/// (`latest: None` + `error`), because the API handler parses this same shape
/// out of stdout regardless of the exit code.
pub fn run_check() -> CheckOutcome {
    let current_text = env!("CARGO_PKG_VERSION").to_owned();
    let mut report = UpdateCheckResponse {
        current: current_text.clone(),
        latest: None,
        update_available: false,
        error: None,
        notes: None,
        release_url: None,
    };

    let release =
        match check::fetch_latest_release(&check::release_api_url(&bundle::release_repo())) {
            Ok(release) => release,
            Err(error) => {
                report.error = Some(error.to_string());
                return CheckOutcome { report, tag: None };
            }
        };
    let tag = release.tag_name;
    let Some(latest) = check::parse_version(&tag) else {
        report.error = Some(format!("unrecognized release tag {tag}"));
        return CheckOutcome { report, tag: None };
    };
    let Some(current) = check::parse_version(&current_text) else {
        report.error = Some(format!("unrecognized build version {current_text}"));
        return CheckOutcome { report, tag: None };
    };

    report.latest = Some(check::strip_v(&tag).to_owned());
    report.update_available = check::is_newer(latest, current);
    report.notes = release.body.as_deref().and_then(check::release_notes);
    report.release_url = release.html_url.as_deref().and_then(check::release_page);
    CheckOutcome {
        report,
        tag: Some(tag),
    }
}

/// The plain-text rendering, built as a `String` so tests can assert on it
/// without capturing stdout (same split as `info::format_human`).
pub fn format_check_human(report: &UpdateCheckResponse) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    writeln!(out, "firecrab {}", report.current).unwrap();
    if let Some(error) = &report.error {
        writeln!(out, "  check failed: {error}").unwrap();
        return out;
    }
    writeln!(out, "  latest: {}", report.latest.as_deref().unwrap_or("-")).unwrap();
    if report.update_available {
        writeln!(
            out,
            "  update available — run: sudo firecrab update --apply"
        )
        .unwrap();
    } else {
        writeln!(out, "  up to date").unwrap();
    }
    out
}

/// Plain-text rendering for a terminal (the default output mode).
pub fn print_check_human(report: &UpdateCheckResponse) {
    print!("{}", format_check_human(report));
}

/// `--json` output mode, for scripting and for `GET /api/update`.
pub fn print_check_json(report: &UpdateCheckResponse) {
    println!("{}", serde_json::to_string(report).unwrap());
}

/// Downloads the matching host bundle, verifies it against the release's
/// `SHA256SUMS`, and hands the swap to `firecrab-helper`, telling `progress`
/// how far along it is. A failure is recorded there before it is returned.
///
/// This function never writes to `$LIBDIR`, `$PREFIX/bin` or `$SHAREDIR`, and
/// never calls `systemctl` — that is the whole point of the privilege split.
pub fn run_apply(
    outcome: &CheckOutcome,
    progress: &mut Progress,
) -> Result<ApplyOutcome, UpdateError> {
    progress.begin(outcome.report.latest.as_deref());
    let result = apply(outcome, progress);
    if let Err(error) = &result {
        progress.failed(&error.to_string());
    }
    result
}

fn apply(outcome: &CheckOutcome, progress: &mut Progress) -> Result<ApplyOutcome, UpdateError> {
    if let Some(error) = &outcome.report.error {
        return Err(UpdateError::Check(error.clone()));
    }
    if !outcome.report.update_available {
        progress.up_to_date();
        return Ok(ApplyOutcome::AlreadyCurrent);
    }
    let tag = outcome
        .tag
        .as_deref()
        .ok_or_else(|| UpdateError::Check("no release tag to download".to_owned()))?;
    let latest = outcome.report.latest.as_deref().unwrap_or(tag);

    progress.stage(UpdatePhase::Checking, "looking up the newest release");
    progress.stage_done(&format!("{} → {latest}", outcome.report.current));

    let arch = bundle::host_arch()?;
    let libc = bundle::host_libc(None)?;
    let asset = bundle::host_tarball(arch, libc);
    let base = bundle::release_base();

    // $DATADIR, not /tmp: firecrab-api.service sets PrivateTmp=yes, so a child
    // it spawns has a /tmp the helper cannot see, and the helper's
    // CapabilityBoundingSet has no CAP_DAC_OVERRIDE to read around it.
    let staging = datadir()
        .join("updates")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&staging).map_err(|error| UpdateError::Download {
        url: staging.display().to_string(),
        detail: error.to_string(),
    })?;
    let _ = std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o750));

    let cleanup = |staging: &std::path::Path| {
        let _ = std::fs::remove_dir_all(staging);
    };

    let tarball = staging.join(&asset);
    let sums = staging.join("SHA256SUMS");
    // The few hundred bytes of checksums first, so a release that lists no
    // checksums fails before the bundle is fetched, and the bundle's progress
    // is the whole of the download stage.
    progress.stage(UpdatePhase::Downloading, &asset);
    let downloaded = bundle::download_to(&bundle::asset_url(&base, tag, "SHA256SUMS"), &sums)
        .and_then(|()| {
            bundle::download_with_progress(
                &bundle::asset_url(&base, tag, &asset),
                &tarball,
                &mut |received, total| progress.downloaded(received, total),
            )
        });
    if let Err(error) = downloaded {
        cleanup(&staging);
        return Err(error);
    }
    for dest in [&sums, &tarball] {
        let _ = std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o640));
    }
    let size = std::fs::metadata(&tarball).map_or(0, |metadata| metadata.len());
    progress.stage_done(&format_bytes(size));

    progress.stage(UpdatePhase::Verifying, "SHA-256 of the bundle");
    let sums_text = std::fs::read_to_string(&sums).unwrap_or_default();
    let Some(expected) = bundle::expected_sha256(&sums_text, &asset) else {
        cleanup(&staging);
        return Err(UpdateError::ChecksumMismatch {
            asset: asset.clone(),
            expected: "(not listed in SHA256SUMS)".to_owned(),
            actual: String::new(),
        });
    };
    let actual = match bundle::file_sha256(&tarball) {
        Ok(actual) => actual,
        Err(error) => {
            cleanup(&staging);
            return Err(error);
        }
    };
    if actual != expected {
        cleanup(&staging);
        return Err(UpdateError::ChecksumMismatch {
            asset,
            expected,
            actual,
        });
    }
    progress.stage_done("matches SHA256SUMS");

    progress.stage(
        UpdatePhase::Applying,
        "handing the bundle to firecrab-helper",
    );
    let result = helper::send_apply_self_update(
        &helper::helper_socket_path(),
        &tarball,
        &expected,
        resolve_layout(),
    );
    if let Err(error) = result {
        cleanup(&staging);
        return Err(error);
    }
    progress.stage_done("firecrab-helper replaced the installed binaries");

    // From here the helper restarts this process's own service, so nothing
    // after it can be relied on to run: this is the last thing recorded.
    progress.stage(UpdatePhase::Restarting, "restarting the services");
    progress.stage_done("firecrab-api and firecrab-helper are restarting now");

    // On success the helper owns the staging directory's cleanup: it removes
    // the bundle and the directory itself after the swap.
    Ok(ApplyOutcome::Applied {
        version: latest.to_owned(),
    })
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

    #[test]
    fn resolve_layout_follows_install_sh_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by ENV_LOCK against every other env-touching test.
        unsafe {
            std::env::remove_var("PREFIX");
            std::env::remove_var("FIRECRAB_LIBDIR");
        }
        let layout = resolve_layout();
        assert_eq!(layout.bindir, std::path::Path::new("/usr/local/bin"));
        assert_eq!(
            layout.libdir,
            std::path::Path::new("/usr/local/lib/firecrab")
        );
        assert_eq!(
            layout.sharedir,
            std::path::Path::new("/usr/local/share/firecrab")
        );
    }

    #[test]
    fn resolve_layout_honours_prefix_and_firecrab_libdir() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by ENV_LOCK — see the note above.
        unsafe {
            std::env::set_var("PREFIX", "/opt/fc");
            std::env::set_var("FIRECRAB_LIBDIR", "/opt/other/lib");
        }
        let layout = resolve_layout();
        unsafe {
            std::env::remove_var("PREFIX");
            std::env::remove_var("FIRECRAB_LIBDIR");
        }
        assert_eq!(layout.bindir, std::path::Path::new("/opt/fc/bin"));
        assert_eq!(layout.libdir, std::path::Path::new("/opt/other/lib"));
        assert_eq!(
            layout.sharedir,
            std::path::Path::new("/opt/fc/share/firecrab")
        );
    }

    #[test]
    fn format_check_human_names_the_apply_command_when_an_update_exists() {
        let report = UpdateCheckResponse {
            current: "0.1.1".to_owned(),
            latest: Some("0.1.2".to_owned()),
            update_available: true,
            error: None,
            notes: None,
            release_url: None,
        };
        let text = format_check_human(&report);
        assert!(text.starts_with("firecrab 0.1.1\n"), "{text}");
        assert!(text.contains("latest: 0.1.2"), "{text}");
        assert!(text.contains("sudo firecrab update --apply"), "{text}");
    }

    #[test]
    fn format_check_human_reports_being_current_and_reports_errors() {
        let current = UpdateCheckResponse {
            current: "0.1.1".to_owned(),
            latest: Some("0.1.1".to_owned()),
            update_available: false,
            error: None,
            notes: None,
            release_url: None,
        };
        assert!(format_check_human(&current).contains("up to date"));

        let failed = UpdateCheckResponse {
            current: "0.1.1".to_owned(),
            latest: None,
            update_available: false,
            error: Some("unreachable: refused".to_owned()),
            notes: None,
            release_url: None,
        };
        let text = format_check_human(&failed);
        assert!(
            text.contains("check failed: unreachable: refused"),
            "{text}"
        );
        assert!(!text.contains("--apply"), "{text}");
    }

    #[test]
    fn run_check_reports_an_error_instead_of_panicking_when_github_is_unreachable() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by ENV_LOCK — see the note above.
        unsafe { std::env::set_var("FIRECRAB_RELEASE_API", "http://127.0.0.1:1/releases/latest") };
        let outcome = run_check();
        unsafe { std::env::remove_var("FIRECRAB_RELEASE_API") };

        assert_eq!(outcome.report.current, env!("CARGO_PKG_VERSION"));
        assert_eq!(outcome.report.latest, None);
        assert!(!outcome.report.update_available);
        assert!(outcome.report.error.is_some());
        assert_eq!(outcome.tag, None);
        // The JSON report must still be emittable on a failed check — the API
        // handler parses stdout regardless of exit code.
        print_check_json(&outcome.report);
    }

    #[test]
    fn run_apply_does_nothing_when_the_check_failed_or_the_host_is_current() {
        let failed = CheckOutcome {
            report: UpdateCheckResponse {
                current: "0.1.1".to_owned(),
                latest: None,
                update_available: false,
                error: Some("unreachable: refused".to_owned()),
                notes: None,
                release_url: None,
            },
            tag: None,
        };
        assert!(matches!(
            run_apply(&failed, &mut Progress::silent()),
            Err(UpdateError::Check(_))
        ));

        let current = CheckOutcome {
            report: UpdateCheckResponse {
                current: "0.1.1".to_owned(),
                latest: Some("0.1.1".to_owned()),
                update_available: false,
                error: None,
                notes: None,
                release_url: None,
            },
            tag: Some("v0.1.1".to_owned()),
        };
        assert!(matches!(
            run_apply(&current, &mut Progress::silent()),
            Ok(ApplyOutcome::AlreadyCurrent)
        ));
    }

    fn recorded(path: &std::path::Path) -> firecrab_api_types::UpdateProgressResponse {
        serde_json::from_slice(&std::fs::read(path).expect("record exists")).expect("record parses")
    }

    #[test]
    fn a_failed_check_leaves_a_failed_record_for_the_dashboard() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("progress.json");
        let failed = CheckOutcome {
            report: UpdateCheckResponse {
                current: "0.1.1".to_owned(),
                latest: None,
                update_available: false,
                error: Some("unreachable: refused".to_owned()),
                notes: None,
                release_url: None,
            },
            tag: None,
        };
        let mut progress = Progress::new(None, Some(path.clone()));
        assert!(run_apply(&failed, &mut progress).is_err());

        let record = recorded(&path);
        assert_eq!(record.phase, UpdatePhase::Failed);
        assert_eq!(
            record.error.as_deref(),
            Some("release check failed: unreachable: refused")
        );
        assert_eq!(record.pid, Some(std::process::id()));
    }

    #[test]
    fn a_host_that_is_current_leaves_a_finished_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("progress.json");
        let current = CheckOutcome {
            report: UpdateCheckResponse {
                current: "0.1.1".to_owned(),
                latest: Some("0.1.1".to_owned()),
                update_available: false,
                error: None,
                notes: None,
                release_url: None,
            },
            tag: Some("v0.1.1".to_owned()),
        };
        let mut progress = Progress::new(None, Some(path.clone()));
        assert!(matches!(
            run_apply(&current, &mut progress),
            Ok(ApplyOutcome::AlreadyCurrent)
        ));

        let record = recorded(&path);
        assert_eq!(record.phase, UpdatePhase::Done);
        assert_eq!(record.percent, 100);
        assert_eq!(record.target.as_deref(), Some("0.1.1"));
    }
}

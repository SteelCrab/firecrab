//! `GET /api/update` (a cached release check) and `POST /api/update` (fire the
//! detached updater).
//!
//! Both shell out to the `firecrab` CLI rather than re-implementing the check
//! here: the CLI is the one place that knows the release naming rules, and
//! running it as a child keeps this handler out of the download path entirely.
//!
//! There is deliberately **no** job-status tracker for the apply. Trackers like
//! `ImageInstallTracker` live in this process's memory, and the last step of a
//! self-update is restarting this very process — any in-memory progress would
//! be destroyed before anyone could read it. The real completion signal is "the
//! API came back and `GET /api/update` reports a higher `current`", which the
//! dashboard already observes.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::{Extension, Json};
use firecrab_api_types::{UpdateCheckResponse, UpdateStartResponse};
use tokio::process::Command;
use uuid::Uuid;

use crate::error::AppError;
use crate::server::RequestId;
use crate::state::AppState;

/// How long one successful check is reused. GitHub's unauthenticated rate
/// limit is 60 requests/hour per IP; with the dashboard polling every 15
/// minutes this keeps the host at 2-4 GitHub calls an hour no matter how many
/// browser tabs are open.
const CACHE_TTL: Duration = Duration::from_secs(30 * 60);
/// Upper bound on the child check, so a wedged CLI cannot hold the handler.
const CHECK_TIMEOUT: Duration = Duration::from_secs(8);

/// `FIRECRAB_CLI_BIN` for an explicit override, then a `$PATH` search
/// (systemd's default `PATH` includes `/usr/local/bin`, so this normally wins
/// on an installed host), then `{PREFIX}/bin/firecrab` as a last resort.
fn resolve_cli_binary() -> PathBuf {
    resolve_cli_binary_from(
        std::env::var_os("FIRECRAB_CLI_BIN"),
        std::env::var_os("PATH"),
        std::env::var("PREFIX").ok(),
    )
}

/// The pure half of [`resolve_cli_binary`], so the chain is testable without
/// touching the process environment.
fn resolve_cli_binary_from(
    override_bin: Option<OsString>,
    path: Option<OsString>,
    prefix: Option<String>,
) -> PathBuf {
    if let Some(bin) = override_bin
        && !bin.is_empty()
    {
        return PathBuf::from(bin);
    }
    if let Some(path) = path {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("firecrab");
            if is_executable(&candidate) {
                return candidate;
            }
        }
    }
    PathBuf::from(prefix.unwrap_or_else(|| "/usr/local".to_owned()))
        .join("bin")
        .join("firecrab")
}

/// Whether `path` is a file this process could exec.
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// A report describing only this build, for when the child never produced one.
fn fallback_report(error: String) -> UpdateCheckResponse {
    UpdateCheckResponse {
        current: env!("CARGO_PKG_VERSION").to_owned(),
        latest: None,
        update_available: false,
        error: Some(error),
    }
}

/// Runs `firecrab update --check --json` and parses its stdout.
///
/// Never fails: an unusable child becomes a report with `error` filled in, so
/// one side widget on the dashboard can never turn into a 500.
///
/// `kill_on_drop(true)` matters here: on the timeout branch below, the
/// in-flight `output()` future — and the `Child` it owns — is dropped without
/// ever being awaited to completion. Tokio's `Child` does not kill its
/// process on drop unless this is set, so without it a `firecrab` that stalls
/// past `timeout` (e.g. GitHub not answering) would keep running as an
/// orphan; because a failed check is deliberately never cached
/// ([`cached_check`]), every subsequent poll during an outage would leak
/// another one.
///
/// The `ETXTBSY` retry mirrors [`crate::firecracker`]'s spawn path. A test's
/// stub CLI is written and immediately exec'd, and a *different* test's forked
/// child can still hold a write descriptor on it — `rename` does not change
/// the inode, so the write-then-rename dance cannot prevent this. Without the
/// retry the spawn error becomes a [`fallback_report`], whose `current` is
/// this build's own version rather than the stub's. Production `firecrab` is
/// an installed binary, so the retry is test-only.
async fn run_check_command(cli: &Path, timeout: Duration) -> UpdateCheckResponse {
    const BUSY_ATTEMPTS: u32 = 8;
    let mut attempt = 0;
    loop {
        let child = Command::new(cli)
            .args(["update", "--check", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output();
        let output = match tokio::time::timeout(timeout, child).await {
            Err(_) => return fallback_report(format!("{} did not answer in time", cli.display())),
            Ok(Err(error))
                if cfg!(test)
                    && error.kind() == io::ErrorKind::ExecutableFileBusy
                    && attempt < BUSY_ATTEMPTS =>
            {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(10 * u64::from(attempt))).await;
                continue;
            }
            Ok(Err(error)) => {
                return fallback_report(format!("cannot run {}: {error}", cli.display()));
            }
            Ok(Ok(output)) => output,
        };
        return match serde_json::from_slice::<UpdateCheckResponse>(&output.stdout) {
            Ok(report) => report,
            Err(error) => fallback_report(format!("unreadable check output: {error}")),
        };
    }
}

/// The cached check. A successful result is stored for [`CACHE_TTL`]; a failed
/// one is not, so the next poll retries instead of pinning the failure for half
/// an hour.
async fn cached_check(state: &AppState, cli: &Path) -> UpdateCheckResponse {
    let mut slot = state.update_check.lock().await;
    if let Some((taken, report)) = slot.as_ref()
        && taken.elapsed() < CACHE_TTL
    {
        return report.clone();
    }
    let report = run_check_command(cli, CHECK_TIMEOUT).await;
    if report.error.is_none() {
        *slot = Some((Instant::now(), report.clone()));
    }
    report
}

/// `GET /api/update`: the newest release, at most 30 minutes stale.
pub async fn get_update_check(State(state): State<AppState>) -> Json<UpdateCheckResponse> {
    let cli = resolve_cli_binary();
    Json(cached_check(&state, &cli).await)
}

/// Spawns `firecrab update --apply --json` fully detached and returns its pid.
///
/// `process_group(0)` puts the child in its own group so it survives the API's
/// own restart, which the helper triggers a few seconds later; all three stdio
/// handles are null because nothing here ever reads them.
fn start_update_inner(
    cli: &Path,
    request_id: Uuid,
) -> Result<(StatusCode, Json<UpdateStartResponse>), AppError> {
    let child = Command::new(cli)
        .args(["update", "--apply", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|_| AppError::internal(request_id))?;
    let pid = child.id().ok_or_else(|| AppError::internal(request_id))?;
    Ok((
        StatusCode::ACCEPTED,
        Json(UpdateStartResponse {
            current: env!("CARGO_PKG_VERSION").to_owned(),
            pid,
        }),
    ))
}

/// `POST /api/update`: launch the updater and answer immediately.
pub async fn start_update(
    State(_state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
) -> Result<(StatusCode, Json<UpdateStartResponse>), AppError> {
    start_update_inner(&resolve_cli_binary(), request_id.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use crate::templates::TemplateRegistry;

    /// A tiny AppState: the update handlers touch neither templates nor VMs,
    /// so this avoids `vms::test_support::test_state`'s mkfs.ext4/debugfs
    /// requirement (same builder `handlers/network.rs`'s first test uses).
    async fn state_for(root: &std::path::Path) -> AppState {
        let templates = TemplateRegistry::from_specs(root, std::iter::empty())
            .expect("empty template spec list should always verify");
        AppState::with_db_file(templates, root.join("state.db"))
            .await
            .expect("fresh temp db should open cleanly")
    }

    /// A stub `firecrab` that prints `body` and appends a line to `counter`.
    fn stub_cli(dir: &std::path::Path, body: &str, counter: &std::path::Path) -> PathBuf {
        let path = dir.join("firecrab-stub");
        fs::write(
            &path,
            format!(
                "#!/bin/sh\necho run >> {}\ncat <<'JSON'\n{}\nJSON\n",
                counter.display(),
                body
            ),
        )
        .expect("write stub");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod stub");
        path
    }

    fn run_count(counter: &std::path::Path) -> usize {
        fs::read_to_string(counter)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn get_update_check_relays_the_cli_json() {
        let dir = tempdir().unwrap();
        let state = state_for(dir.path()).await;
        let counter = dir.path().join("runs");
        let cli = stub_cli(
            dir.path(),
            r#"{"current":"0.1.1","latest":"0.1.2","updateAvailable":true}"#,
            &counter,
        );

        let report = cached_check(&state, &cli).await;
        assert_eq!(report.current, "0.1.1");
        assert_eq!(report.latest.as_deref(), Some("0.1.2"));
        assert!(report.update_available);
        assert!(report.error.is_none());
        assert_eq!(run_count(&counter), 1);
    }

    #[tokio::test]
    async fn get_update_check_serves_the_cached_result() {
        let dir = tempdir().unwrap();
        let state = state_for(dir.path()).await;
        let counter = dir.path().join("runs");
        let cli = stub_cli(
            dir.path(),
            r#"{"current":"0.1.1","latest":"0.1.2","updateAvailable":true}"#,
            &counter,
        );

        let first = cached_check(&state, &cli).await;
        let second = cached_check(&state, &cli).await;
        assert_eq!(first, second);
        assert_eq!(
            run_count(&counter),
            1,
            "the second call must not re-run the CLI"
        );
    }

    #[tokio::test]
    async fn a_failed_check_is_not_cached() {
        let dir = tempdir().unwrap();
        let state = state_for(dir.path()).await;
        let counter = dir.path().join("runs");
        let cli = stub_cli(
            dir.path(),
            r#"{"current":"0.1.1","updateAvailable":false,"error":"unreachable: refused"}"#,
            &counter,
        );

        let first = cached_check(&state, &cli).await;
        assert!(first.error.is_some());
        let _ = cached_check(&state, &cli).await;
        assert_eq!(run_count(&counter), 2, "a failed check must be retried");
    }

    #[tokio::test]
    async fn get_update_check_answers_200_when_the_cli_is_missing() {
        let dir = tempdir().unwrap();
        let state = state_for(dir.path()).await;

        let report = cached_check(&state, std::path::Path::new("/no/such/binary")).await;
        // A side widget on the dashboard is not worth a 500.
        assert_eq!(report.current, env!("CARGO_PKG_VERSION"));
        assert_eq!(report.latest, None);
        assert!(!report.update_available);
        assert!(report.error.is_some());
    }

    #[tokio::test]
    async fn get_update_check_answers_200_when_the_cli_prints_garbage() {
        let dir = tempdir().unwrap();
        let state = state_for(dir.path()).await;
        let counter = dir.path().join("runs");
        let cli = stub_cli(dir.path(), "not json at all", &counter);

        let report = cached_check(&state, &cli).await;
        assert!(
            report.error.is_some(),
            "unparsable stdout must surface as an error field"
        );
    }

    /// Waits (bounded) for `pidfile` to contain a pid, so the test only
    /// starts asserting the child is dead once it has proof the child ever
    /// actually started — otherwise a too-early check would pass vacuously.
    async fn wait_for_pid(pidfile: &std::path::Path) -> u32 {
        for _ in 0..100 {
            if let Ok(text) = fs::read_to_string(pidfile)
                && let Ok(pid) = text.trim().parse()
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("stub never wrote its pid to {}", pidfile.display());
    }

    /// Whether `pid` is still running (as opposed to gone or a zombie
    /// awaiting reap) per `/proc/<pid>/stat`'s state field. Zombie counts as
    /// "not running": `kill_on_drop` only promises the signal was sent, not
    /// that tokio's background reaper won on this exact tick.
    fn process_is_running(pid: u32) -> bool {
        match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat
                .rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().next())
                .is_some_and(|state| state != "Z"),
            Err(_) => false,
        }
    }

    #[tokio::test]
    async fn a_timed_out_check_kills_the_child_instead_of_leaving_an_orphan() {
        let dir = tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let path = dir.path().join("hanger");
        // `exec`s into `sleep` so the recorded pid is the long-running
        // process itself, not a shell that later forks it.
        fs::write(
            &path,
            format!("#!/bin/sh\necho $$ > {}\nexec sleep 5\n", pidfile.display()),
        )
        .expect("write stub");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod stub");

        let report = run_check_command(&path, Duration::from_millis(100)).await;
        assert!(
            report.error.is_some(),
            "a stalled child must surface as an error rather than hang the handler"
        );

        let pid = wait_for_pid(&pidfile).await;
        for _ in 0..100 {
            if !process_is_running(pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("child pid {pid} is still running 2s after the timeout; kill_on_drop did not fire");
    }

    #[tokio::test]
    async fn start_update_answers_202_with_the_spawned_pid() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sleeper");
        fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write stub");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod stub");

        let (status, Json(response)) =
            start_update_inner(&path, uuid::Uuid::nil()).expect("spawn should succeed");
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(
            response.pid > 0,
            "pid must identify the child in the journal"
        );
        assert_eq!(response.current, env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn start_update_reports_a_missing_binary_as_an_internal_error() {
        assert!(
            start_update_inner(std::path::Path::new("/no/such/binary"), uuid::Uuid::nil()).is_err()
        );
    }

    #[test]
    fn resolve_cli_binary_prefers_the_env_override() {
        let resolved = resolve_cli_binary_from(
            Some(std::ffi::OsString::from("/opt/custom/firecrab")),
            Some(std::ffi::OsString::from("/usr/bin")),
            Some("/usr/local".to_owned()),
        );
        assert_eq!(resolved, PathBuf::from("/opt/custom/firecrab"));
    }

    #[test]
    fn resolve_cli_binary_finds_an_executable_on_path_then_falls_back_to_the_prefix() {
        let dir = tempdir().unwrap();
        let on_path = dir.path().join("firecrab");
        std::fs::write(&on_path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&on_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(
            resolve_cli_binary_from(
                None,
                Some(std::ffi::OsString::from(dir.path().as_os_str())),
                Some("/usr/local".to_owned())
            ),
            on_path
        );
        assert_eq!(
            resolve_cli_binary_from(
                None,
                Some(std::ffi::OsString::from("/nope")),
                Some("/opt/fc".to_owned())
            ),
            PathBuf::from("/opt/fc/bin/firecrab")
        );
    }
}

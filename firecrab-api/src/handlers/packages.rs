//! OS package update: runs `apt`/`apk` upgrade on a running VM's serial
//! console and reports the result. Reuses the same "print a fixed sentinel,
//! scan the console for it" pattern `wait_for_network_ready`
//! (`handlers::vms`) uses for boot readiness, since there's no guest agent
//! to ask directly (`docs/task-guest-network-configuration.md`).

use std::collections::BTreeMap;
use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, Path, State};
use firecrab_api_types::{PackageUpdateStatus, VmResponse};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::error::AppError;
use crate::firecracker::VmProcess;
use crate::model::VmState;
use crate::server::RequestId;
use crate::state::AppState;

use super::vms::{lease_for, parse_id, vm_response};

/// How long a package update may run before it's considered failed. Far
/// more generous than the boot-readiness timeout: apt/apk output is only
/// observable through the guest's own (slow, character-oriented) serial
/// console, and a real upgrade can pull a meaningful amount of data.
const PACKAGE_UPDATE_TIMEOUT: Duration = Duration::from_secs(600);

/// Bytes of the command's own output kept for the response's `outputTail` —
/// enough for the last several dozen lines without holding a full apt/apk
/// transcript in memory for the life of the VM.
const OUTPUT_TAIL_CAP: usize = 8 * 1024;

/// Sentinel the update command prints once it's done, followed by `:` and
/// its exit code (e.g. `FIRECRAB_PKG_UPDATE_DONE:0`).
const DONE_SENTINEL: &str = "FIRECRAB_PKG_UPDATE_DONE";

/// The guest distro families this project ships templates for, inferred
/// from the template alias rather than stored separately — `ubuntu-*` and
/// `alpine-*` are the only aliases `TemplateRegistry::load_default` ever
/// registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageManager {
    Apt,
    Apk,
}

impl PackageManager {
    fn for_template(template: &str) -> Option<Self> {
        if template.starts_with("ubuntu") {
            Some(Self::Apt)
        } else if template.starts_with("alpine") {
            Some(Self::Apk)
        } else {
            None
        }
    }

    /// The shell command run on the guest's console, wrapped in a subshell
    /// so the final `$?` reflects the compound command's own exit status
    /// rather than just the trailing `echo`.
    fn update_command(self) -> &'static str {
        match self {
            Self::Apt => "apt-get update && DEBIAN_FRONTEND=noninteractive apt-get upgrade -y",
            Self::Apk => "apk update && apk upgrade",
        }
    }
}

/// Starts an OS package update on the guest's console and returns
/// immediately with `packageUpdate: {"state":"running"}` — the update
/// itself can run for minutes, far past `enforce_limits`' request timeout,
/// so it's detached the same way `start_vm` detaches its own pipeline (see
/// that function's doc comment in `handlers::vms`). The dashboard's
/// existing polling picks up the eventual `succeeded`/`failed` result.
pub async fn update_packages(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<VmResponse>, AppError> {
    let id = parse_id(&id, request_id.0)?;

    let template = {
        let vms = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let vm = vms
            .get(&id)
            .ok_or_else(|| AppError::not_found(request_id.0))?;
        if vm.state != VmState::Running {
            return Err(AppError::invalid_state(vm.state, request_id.0));
        }
        vm.template.clone()
    };

    let manager = PackageManager::for_template(&template).ok_or_else(|| {
        let mut fields = BTreeMap::new();
        fields.insert(
            "template".to_owned(),
            "has no known package manager".to_owned(),
        );
        AppError::validation(fields, request_id.0)
    })?;

    let process = state
        .processes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&id)
        .cloned()
        .ok_or_else(|| AppError::vm_not_running(request_id.0))?;

    if let Some(vm) = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get_mut(&id)
    {
        vm.package_update = Some(PackageUpdateStatus::Running);
    }

    let state_for_task = state.clone();
    tokio::spawn(async move {
        run_update(&state_for_task, id, process, manager).await;
    });

    let vm = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&id)
        .cloned()
        .ok_or_else(|| AppError::not_found(request_id.0))?;
    let lease = lease_for(&state, id).await;
    Ok(Json(vm_response(&vm, lease.as_ref())))
}

/// Writes the update command to the guest's console, waits for its
/// completion sentinel (or the timeout/console closing), and records the
/// resulting [`PackageUpdateStatus`].
async fn run_update(state: &AppState, id: Uuid, process: VmProcess, manager: PackageManager) {
    // Subscribed before the command is even sent, so nothing the command
    // prints can land only in the backlog and be missed — see
    // `wait_for_network_ready`'s doc comment for the same reasoning. The
    // backlog itself (everything printed *before* this call) is dropped:
    // scanning it could match a previous run's sentinel still in scrollback
    // and report a false, instant success.
    let (_backlog, mut receiver) = process.console.subscribe();
    let command = format!(
        "({}); echo \"{DONE_SENTINEL}:$?\"\n",
        manager.update_command()
    );
    process.console.write_input(command.as_bytes()).await;

    let status = match wait_for_completion(&mut receiver, PACKAGE_UPDATE_TIMEOUT).await {
        Ok((0, tail)) => PackageUpdateStatus::Succeeded { output_tail: tail },
        Ok((code, tail)) => PackageUpdateStatus::Failed {
            reason: format!("exited with code {code}"),
            output_tail: tail,
        },
        Err(reason) => PackageUpdateStatus::Failed {
            reason,
            output_tail: String::new(),
        },
    };

    if let Some(vm) = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get_mut(&id)
    {
        vm.package_update = Some(status);
    }
}

/// Reads console output until [`DONE_SENTINEL`] appears, the console
/// closes, or `timeout` elapses — `Ok` carries the guest-reported exit code
/// plus the output seen so far (capped at [`OUTPUT_TAIL_CAP`]).
async fn wait_for_completion(
    receiver: &mut broadcast::Receiver<Vec<u8>>,
    timeout: Duration,
) -> Result<(i32, String), String> {
    let mut tail = Vec::new();
    let wait = async {
        loop {
            match receiver.recv().await {
                Ok(chunk) => {
                    tail.extend_from_slice(&chunk);
                    if tail.len() > OUTPUT_TAIL_CAP {
                        let excess = tail.len() - OUTPUT_TAIL_CAP;
                        tail.drain(..excess);
                    }
                    if let Some(code) = find_done_sentinel(&tail) {
                        return Ok((code, String::from_utf8_lossy(&tail).into_owned()));
                    }
                }
                // A lagged receiver just missed some buffered output — the
                // command is still running fine, so keep reading forward
                // instead of treating it as the console having closed (see
                // `wait_for_network_ready`'s identical handling).
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    return Err("console closed before the update finished".to_owned());
                }
            }
        }
    };

    tokio::time::timeout(timeout, wait)
        .await
        .unwrap_or_else(|_| Err("timed out waiting for the update to finish".to_owned()))
}

/// Finds the last complete `FIRECRAB_PKG_UPDATE_DONE:<code>` line in
/// `buffer` and parses its exit code. The literal command text itself
/// (echoed back as it's typed, containing the unexpanded `$?`) never
/// parses as a number, so it can't be mistaken for the real result.
fn find_done_sentinel(buffer: &[u8]) -> Option<i32> {
    let text = String::from_utf8_lossy(buffer);
    text.lines().rev().find_map(|line| {
        let (_, rest) = line.split_once(DONE_SENTINEL)?;
        rest.trim_start_matches(':').trim().parse().ok()
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use tempfile::tempdir;
    use tokio::sync::watch;

    use super::*;
    use crate::console::ConsoleBroker;
    use crate::handlers::vms::test_support::{record, seed_vm, test_state};
    use crate::model::VmRecord;

    fn register_fake_process(state: &AppState, id: Uuid) -> Arc<ConsoleBroker> {
        let console = Arc::new(ConsoleBroker::new());
        let (_exited_tx, exited_rx) = watch::channel(false);
        state.processes.lock().unwrap().insert(
            id,
            VmProcess {
                pid: 0,
                exited: exited_rx,
                console: console.clone(),
            },
        );
        console
    }

    #[tokio::test]
    async fn update_packages_rejects_a_vm_that_is_not_running() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = record("test-vm", Uuid::new_v4());
        seed_vm(&state, &vm);

        let error = update_packages(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(vm.id.to_string()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn update_packages_rejects_a_template_with_no_known_package_manager() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = VmRecord {
            state: VmState::Running,
            template: "windows-11".to_owned(),
            ..record("test-vm", Uuid::new_v4())
        };
        seed_vm(&state, &vm);
        register_fake_process(&state, vm.id);

        let error = update_packages(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(vm.id.to_string()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_packages_rejects_a_running_vm_with_no_live_process() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = VmRecord {
            state: VmState::Running,
            ..record("test-vm", Uuid::new_v4())
        };
        seed_vm(&state, &vm);

        let error = update_packages(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(vm.id.to_string()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn update_packages_reports_running_then_succeeded_once_the_console_finishes() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = VmRecord {
            state: VmState::Running,
            ..record("test-vm", Uuid::new_v4())
        };
        seed_vm(&state, &vm);
        let console = register_fake_process(&state, vm.id);

        let Json(body) = update_packages(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(vm.id.to_string()),
        )
        .await
        .unwrap();
        assert_eq!(body.package_update, Some(PackageUpdateStatus::Running));

        console.push_output(b"FIRECRAB_PKG_UPDATE_DONE:0\n");

        for _ in 0..100 {
            let status = state
                .vms
                .lock()
                .unwrap()
                .get(&vm.id)
                .and_then(|vm| vm.package_update.clone());
            if matches!(status, Some(PackageUpdateStatus::Succeeded { .. })) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("package update never reached Succeeded");
    }

    #[tokio::test]
    async fn update_packages_reports_failed_on_a_nonzero_exit_code() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = VmRecord {
            state: VmState::Running,
            template: "alpine-3.24".to_owned(),
            ..record("test-vm", Uuid::new_v4())
        };
        seed_vm(&state, &vm);
        let console = register_fake_process(&state, vm.id);

        let _ = update_packages(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(vm.id.to_string()),
        )
        .await
        .unwrap();

        console.push_output(b"FIRECRAB_PKG_UPDATE_DONE:1\n");

        for _ in 0..100 {
            let status = state
                .vms
                .lock()
                .unwrap()
                .get(&vm.id)
                .and_then(|vm| vm.package_update.clone());
            if matches!(status, Some(PackageUpdateStatus::Failed { .. })) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("package update never reached Failed");
    }

    #[test]
    fn package_manager_is_inferred_from_the_template_alias() {
        assert_eq!(
            PackageManager::for_template("ubuntu-26.04"),
            Some(PackageManager::Apt)
        );
        assert_eq!(
            PackageManager::for_template("alpine-3.24"),
            Some(PackageManager::Apk)
        );
        assert_eq!(PackageManager::for_template("windows-11"), None);
    }

    #[test]
    fn find_done_sentinel_ignores_the_echoed_command_and_finds_the_real_result() {
        let buffer = b"Reading package lists...\n\
            echo \"FIRECRAB_PKG_UPDATE_DONE:$?\"\n\
            FIRECRAB_PKG_UPDATE_DONE:0\n";
        assert_eq!(find_done_sentinel(buffer), Some(0));
    }

    #[test]
    fn find_done_sentinel_reports_a_nonzero_exit_code() {
        assert_eq!(
            find_done_sentinel(b"FIRECRAB_PKG_UPDATE_DONE:100\n"),
            Some(100)
        );
    }

    #[test]
    fn find_done_sentinel_is_none_before_the_command_finishes() {
        assert_eq!(find_done_sentinel(b"Reading package lists...\n"), None);
    }

    #[tokio::test]
    async fn wait_for_completion_survives_a_lagged_receiver() {
        let console = ConsoleBroker::new();
        let (_backlog, mut receiver) = console.subscribe();

        let waiter = tokio::spawn(async move {
            wait_for_completion(&mut receiver, Duration::from_secs(5)).await
        });
        tokio::task::yield_now().await;

        for _ in 0..300 {
            console.push_output(b"Unpacking...\n");
        }
        console.push_output(b"FIRECRAB_PKG_UPDATE_DONE:0\n");

        let (code, tail) = waiter.await.expect("waiter task panicked").unwrap();
        assert_eq!(code, 0);
        assert!(tail.contains("FIRECRAB_PKG_UPDATE_DONE:0"));
    }

    #[tokio::test]
    async fn wait_for_completion_caps_the_tail_at_output_tail_cap() {
        let console = ConsoleBroker::new();
        let (_backlog, mut receiver) = console.subscribe();

        let waiter = tokio::spawn(async move {
            wait_for_completion(&mut receiver, Duration::from_secs(5)).await
        });
        tokio::task::yield_now().await;

        // One line short of OUTPUT_TAIL_CAP by itself is well past it once
        // repeated — enough to force the rolling-buffer trim, not just fill
        // it exactly.
        let line = "x".repeat(100) + "\n";
        for _ in 0..(OUTPUT_TAIL_CAP / line.len() + 10) {
            console.push_output(line.as_bytes());
        }
        console.push_output(b"FIRECRAB_PKG_UPDATE_DONE:0\n");

        let (code, tail) = waiter.await.expect("waiter task panicked").unwrap();
        assert_eq!(code, 0);
        assert!(tail.len() <= OUTPUT_TAIL_CAP);
        assert!(tail.contains("FIRECRAB_PKG_UPDATE_DONE:0"));
    }

    #[tokio::test]
    async fn wait_for_completion_fails_when_the_console_closes_first() {
        let console = ConsoleBroker::new();
        let (_backlog, mut receiver) = console.subscribe();
        drop(console);

        let error = wait_for_completion(&mut receiver, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(error.contains("closed"));
    }
}

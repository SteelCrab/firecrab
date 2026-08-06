//! Web-triggered from-scratch distro bootstraps: boot a builder VM off
//! *any* already-installed template (a disposable environment, not the
//! target), run a bootstrap script over its console that downloads the
//! target's official base, chroots in, installs packages + kernel via the
//! target's own package manager, and `mkfs.ext4 -d`s a finished rootfs —
//! then dump the result out of the builder VM's disk and package it as
//! `{alias}.tar.zst` for the existing `image_install.rs` pipeline to pick
//! up unchanged. See
//! `docs/superpowers/specs/2026-08-03-m2image-web-rebuild-design.md`.

use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use firecrab_api_types::{BootstrapResponse, BootstrapStatus, CreateVmRequest, EgressPolicy};
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::model::VmState;
use crate::server::RequestId;
use crate::state::AppState;

use super::builds::{builder_micro_network_id, builder_vm_name, mark_as_builder};
use super::vms::{create_vm, start_vm_request};

/// Matches `handlers::builds`'s own poll cadence.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Generous on purpose, same reasoning as `handlers::builds::BUILDER_BOOT_TIMEOUT`.
const BUILDER_BOOT_TIMEOUT: Duration = Duration::from_secs(600);

/// The 3 aliases this feature can bootstrap — deliberately not
/// `TemplateRegistry::known_specs()` directly, so a future built-in
/// addition doesn't silently become bootstrap-eligible without its own
/// guest script (Task 6 covers exactly these 3, no more).
const BOOTSTRAPPABLE_ALIASES: [&str; 3] = ["alpine-3.24", "ubuntu-26.04", "rocky-9"];

/// Sentinel the pushed script prints once it's done, followed by `:` and
/// its exit code — same shape as `packages::DONE_SENTINEL`, kept as its
/// own distinct string so a bootstrap's completion can never be confused
/// with an unrelated package action finishing on the same console.
const BOOTSTRAP_DONE_SENTINEL: &str = "FIRECRAB_BOOTSTRAP_DONE";

/// How long the guest-side bootstrap script may run before this module
/// gives up waiting — real network downloads (hundreds of MB) plus a real
/// package install, so far more generous than
/// `packages::PACKAGE_UPDATE_TIMEOUT`.
const BOOTSTRAP_SCRIPT_TIMEOUT: Duration = Duration::from_secs(1800);

const ALPINE_SCRIPT: &str =
    include_str!("../../../scripts/firecracker-menual/bootstrap-alpine-in-guest.sh");
const UBUNTU_SCRIPT: &str =
    include_str!("../../../scripts/firecracker-menual/bootstrap-ubuntu-in-guest.sh");
const ROCKY_SCRIPT: &str =
    include_str!("../../../scripts/firecracker-menual/bootstrap-rocky-in-guest.sh");

fn script_for(alias: &str) -> &'static str {
    match alias {
        "alpine-3.24" => ALPINE_SCRIPT,
        "ubuntu-26.04" => UBUNTU_SCRIPT,
        "rocky-9" => ROCKY_SCRIPT,
        other => unreachable!("start_bootstrap already rejected unknown alias {other}"),
    }
}

/// Alpine and Ubuntu bootstrap by chrooting into a freshly-downloaded base
/// that carries its own package manager, so any installed template can
/// serve as the outer builder environment. Rocky's bootstrap needs `dnf`
/// already present in the *outer* guest (see
/// `scripts/firecracker-menual/bootstrap-rocky-in-guest.sh`'s doc comment),
/// so its own builder VM must itself already be `rocky-9`.
fn requires_matching_source(target_alias: &str) -> bool {
    target_alias == "rocky-9"
}

/// `POST /api/images/{alias}/bootstrap` — boots a builder VM off any
/// already-installed template and registers a new bootstrap session for
/// `alias`. Returns immediately, same convention as `start_build`; the
/// caller polls `GET /api/images/bootstrap/{bootstrapId}`.
pub async fn start_bootstrap(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(alias): Path<String>,
) -> Result<(StatusCode, Json<BootstrapResponse>), AppError> {
    if !BOOTSTRAPPABLE_ALIASES.contains(&alias.as_str()) {
        return Err(AppError::not_found(request_id.0));
    }

    if state.bootstraps.any_active() {
        return Err(AppError::conflict(
            "bootstrap_in_progress",
            "a bootstrap is already running; wait for it to finish before starting another",
            request_id.0,
        ));
    }

    let source_alias = pick_builder_source(&state, &alias, request_id.0)?;
    let source = state
        .templates
        .resolve_alias(&source_alias)
        .ok_or_else(|| AppError::internal(request_id.0))?;

    let micro_network_id = builder_micro_network_id(&state, request_id.0).await?;

    let create_request = CreateVmRequest {
        name: builder_vm_name(&format!("bootstrap-{alias}")),
        template: source_alias.clone(),
        ram: 1024,
        cpu: 1,
        disk_gb: bootstrap_disk_gb(&alias),
        egress_policy: EgressPolicy::Internet,
        micro_network_id,
        storage_root: None,
    };
    let _ = source; // only needed to confirm the source alias actually resolves

    let (_status, Json(created)) = create_vm(
        State(state.clone()),
        Extension(request_id),
        ValidatedJson(create_request),
    )
    .await?;

    mark_as_builder(&state, created.id, request_id.0).await?;

    let _vm_response = start_vm_request(
        State(state.clone()),
        Extension(request_id),
        Path(created.id.to_string()),
    )
    .await?;

    let bootstrap_id = state.bootstraps.begin(&alias, &source_alias, created.id);

    let state_for_watch = state.clone();
    tokio::spawn(async move {
        watch_bootstrap_boot(&state_for_watch, bootstrap_id, created.id).await;
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(state.bootstraps.get(bootstrap_id).expect("just inserted")),
    ))
}

/// Every installed rootfs's disk floor plus generous headroom for: the
/// downloaded official base archive, the fully-installed staging tree, and
/// the final `mkfs.ext4`'d image sitting alongside it before it's dumped
/// out — all three exist on the builder VM's own disk at once mid-build.
/// Sized per target rather than derived from the source template, since the
/// source is just a disposable outer environment unrelated to how big the
/// target ends up.
fn bootstrap_disk_gb(target_alias: &str) -> u16 {
    match target_alias {
        "alpine-3.24" => 4,
        _ => 8, // ubuntu-26.04, rocky-9 — 2G rootfs_size each, per default_specs()
    }
}

/// Picks an already-installed template to boot as the builder VM.
/// `requires_matching_source` narrows this to the target itself for
/// aliases whose bootstrap needs the outer guest to already have that
/// distro's own package manager (currently just `rocky-9`, see its own
/// doc comment) — everything else accepts any installed alias, preferring
/// the smallest rootfs since it boots fastest.
fn pick_builder_source(
    state: &AppState,
    target_alias: &str,
    request_id: Uuid,
) -> Result<String, AppError> {
    let candidates = state.templates.list_aliases();
    let mut eligible: Vec<_> = candidates
        .into_iter()
        .filter(|version| !requires_matching_source(target_alias) || version.name == target_alias)
        .collect();
    eligible.sort_by_key(|version| version.rootfs.length());

    eligible
        .into_iter()
        .next()
        .map(|version| version.name.clone())
        .ok_or_else(|| {
            AppError::unavailable(
                if requires_matching_source(target_alias) {
                    "bootstrapping rocky-9 needs rocky-9 already installed to provide dnf — install it first"
                } else {
                    "no template is installed yet to serve as the builder VM — install one first"
                },
                request_id,
            )
        })
}

/// Polls the builder VM's lifecycle state until it reaches `Running`
/// (session becomes `Running`... — see note below), a terminal failure, or
/// [`BUILDER_BOOT_TIMEOUT`] elapses. Mirrors
/// `handlers::builds::watch_builder_boot` exactly (same CAS-against-`Booting`
/// safety reasoning), adapted to `BootstrapStatus`'s own states — note this
/// module's `Running` means "VM up, bootstrap script executing", not
/// `BuildStatus::Ready`'s "VM up, waiting for a command" — because a
/// bootstrap session has no separate `Ready`-then-command step: the whole
/// script is dispatched as soon as the VM is usable (Task 7).
pub(crate) async fn watch_bootstrap_boot(state: &AppState, bootstrap_id: Uuid, vm_id: Uuid) {
    let deadline = tokio::time::Instant::now() + BUILDER_BOOT_TIMEOUT;
    loop {
        match state.bootstraps.get(bootstrap_id) {
            Some(session) if session.status == BootstrapStatus::Booting => {}
            _ => return,
        }

        let vm_state = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&vm_id)
            .map(|vm| vm.state);

        match vm_state {
            Some(VmState::Running) => {
                state.bootstraps.append_log(
                    bootstrap_id,
                    "builder VM is running — starting bootstrap script",
                );
                if state.bootstraps.set_status_from(
                    bootstrap_id,
                    BootstrapStatus::Booting,
                    BootstrapStatus::Running,
                ) {
                    let state_for_script = state.clone();
                    tokio::spawn(async move {
                        run_bootstrap_script(&state_for_script, bootstrap_id, vm_id).await;
                    });
                }
                return;
            }
            Some(state_now @ (VmState::Error | VmState::Stopped)) => {
                state.bootstraps.finish_err_from(
                    bootstrap_id,
                    BootstrapStatus::Booting,
                    format!("builder VM {vm_id} failed to boot (state: {state_now:?})"),
                );
                return;
            }
            None => return,
            Some(_) => {}
        }

        if tokio::time::Instant::now() >= deadline {
            state.bootstraps.finish_err_from(
                bootstrap_id,
                BootstrapStatus::Booting,
                format!(
                    "builder VM {vm_id} did not reach running within {}s",
                    BUILDER_BOOT_TIMEOUT.as_secs()
                ),
            );
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Writes the guest-native bootstrap script for `state.bootstraps.get(id).alias`
/// to the builder VM's console as a single heredoc (so the whole script
/// lands as one shell invocation — no chunking or base64 needed, since
/// `write_input` writes raw bytes to the guest's stdin pipe and the
/// guest's own shell parses embedded newlines exactly the way it would
/// typed input, including multi-line constructs), waits for
/// [`BOOTSTRAP_DONE_SENTINEL`], and advances the session on success.
pub(crate) async fn run_bootstrap_script(state: &AppState, bootstrap_id: Uuid, vm_id: Uuid) {
    let Some(session) = state.bootstraps.get(bootstrap_id) else {
        return;
    };
    let script = script_for(&session.alias);

    let Some(process) = state
        .processes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&vm_id)
        .cloned()
    else {
        state.bootstraps.finish_err_from(
            bootstrap_id,
            BootstrapStatus::Running,
            "builder VM's console process is no longer available",
        );
        return;
    };

    let (_backlog, mut receiver) = process.console.subscribe();
    let heredoc = format!(
        "cat > /root/fc-bootstrap.sh <<'FIRECRAB_BOOTSTRAP_SCRIPT_EOF'\n{script}\nFIRECRAB_BOOTSTRAP_SCRIPT_EOF\nsh /root/fc-bootstrap.sh; echo \"{BOOTSTRAP_DONE_SENTINEL}:$?\"\n"
    );
    process.console.write_input(heredoc.as_bytes()).await;

    match super::packages::wait_for_completion_with_sentinel(
        &mut receiver,
        BOOTSTRAP_SCRIPT_TIMEOUT,
        BOOTSTRAP_DONE_SENTINEL,
    )
    .await
    {
        Ok((0, tail)) => {
            state.bootstraps.append_log(bootstrap_id, tail);
            if state.bootstraps.set_status_from(
                bootstrap_id,
                BootstrapStatus::Running,
                BootstrapStatus::Packaging,
            ) {
                let state_for_package = state.clone();
                tokio::spawn(async move {
                    package_bootstrap(&state_for_package, bootstrap_id, vm_id).await;
                });
            }
        }
        Ok((code, tail)) => {
            state.bootstraps.finish_err_from(
                bootstrap_id,
                BootstrapStatus::Running,
                format!("bootstrap script exited with code {code}\n{tail}"),
            );
        }
        Err(reason) => {
            state
                .bootstraps
                .finish_err_from(bootstrap_id, BootstrapStatus::Running, reason);
        }
    }
}

/// Task 8 replaces this stub with the real implementation: extracts the
/// finished rootfs from the builder VM's disk and packages it as
/// `{alias}.tar.zst` for the existing `image_install.rs` pipeline.
async fn package_bootstrap(state: &AppState, bootstrap_id: Uuid, vm_id: Uuid) {
    let _ = vm_id;
    state
        .bootstraps
        .finish_err(bootstrap_id, "packaging not yet implemented");
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::response::IntoResponse;
    use tempfile::tempdir;
    use tokio::sync::watch;

    use super::*;
    use crate::console::ConsoleBroker;
    use crate::firecracker::VmProcess;
    use crate::handlers::vms::test_support::test_state;

    /// Registers a fake console+process for `id`, the same way
    /// `handlers::packages`'s and `handlers::builds`'s own tests do —
    /// `run_bootstrap_script` requires a live `VmProcess` to write the
    /// heredoc + sentinel-wait command to, and the test fixture never
    /// actually boots Firecracker.
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

    /// See `handlers::packages`'s identical helper: `run_bootstrap_script`
    /// returns as soon as it has *spawned*, which only then subscribes to
    /// the console — output pushed before that subscription would be lost.
    async fn wait_for_console_subscriber(console: &ConsoleBroker) {
        for _ in 0..200 {
            if console.subscriber_count() > 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("run_bootstrap_script never subscribed to the console");
    }

    #[tokio::test]
    async fn start_bootstrap_rejects_an_unknown_target_alias() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let error = start_bootstrap(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path("no-such-alias".to_owned()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn start_bootstrap_rejects_when_no_matching_source_is_installed() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);

        let error = start_bootstrap(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path("rocky-9".to_owned()),
        )
        .await
        .unwrap_err();

        assert_eq!(
            error.into_response().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn run_bootstrap_script_records_the_console_output_and_reaches_running_terminal_wait() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = crate::handlers::vms::test_support::record("builder", Uuid::new_v4());
        crate::handlers::vms::test_support::seed_vm(&state, &vm);
        let console = register_fake_process(&state, vm.id);
        let bootstrap_id = state.bootstraps.begin("ubuntu-26.04", "alpine-3.24", vm.id);
        state
            .bootstraps
            .set_status(bootstrap_id, BootstrapStatus::Running);

        let vm_id = vm.id;
        let push_sentinel = async {
            wait_for_console_subscriber(&console).await;
            console.push_output(format!("{}:0\n", BOOTSTRAP_DONE_SENTINEL).as_bytes());
        };
        // Driven with `join!` on this same task rather than a separate
        // `tokio::spawn` + `.await`: `run_bootstrap_script`'s own success
        // path spawns `package_bootstrap` (Task 8's stub, which fails the
        // session immediately — see its doc comment) as its very last step,
        // right after moving the session to `Packaging`. Spawning
        // `run_bootstrap_script` itself as a separate task would let the
        // runtime schedule that spawned successor ahead of this test's
        // resumption, so the assertion below could observe `Failed`
        // instead — a race that's real today only because the stub
        // finishes instantly; Task 8's real implementation will take
        // actual wall-clock time. Running everything on one task means the
        // assertion executes synchronously in the same poll cycle
        // `run_bootstrap_script` completes in, before the runtime gets a
        // chance to run that spawned successor.
        tokio::join!(
            run_bootstrap_script(&state, bootstrap_id, vm_id),
            push_sentinel
        );

        let snapshot = state.bootstraps.get(bootstrap_id).unwrap();
        assert_eq!(snapshot.status, BootstrapStatus::Packaging);
        assert!(snapshot.log.contains(BOOTSTRAP_DONE_SENTINEL));
    }
}

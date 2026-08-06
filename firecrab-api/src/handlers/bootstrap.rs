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
use firecrab_api_types::{
    BootstrapResponse, BootstrapStatus, BootstrapStep, CreateVmRequest, EgressPolicy,
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::model::VmState;
use crate::server::RequestId;
use crate::state::AppState;

use super::builds::{builder_micro_network_id, builder_vm_name, mark_as_builder};
use super::vms::{create_vm, parse_id, start_vm_request};

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

/// Marker the console-readiness probe asks the guest's shell to echo back,
/// kept distinct from [`BOOTSTRAP_DONE_SENTINEL`] so a probe reply can never
/// be mistaken for the script itself finishing.
const CONSOLE_PROBE_SENTINEL: &str = "FIRECRAB_BOOTSTRAP_SHELL_READY";

/// How long one probe waits for its own reply before probing again. Short:
/// a live shell answers in milliseconds, and the point is to keep retrying
/// cheaply while agetty is still coming up.
const CONSOLE_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Bounded so an unbootable console fails in ~30s with a clear message
/// instead of hanging until [`BOOTSTRAP_SCRIPT_TIMEOUT`].
const CONSOLE_PROBE_ATTEMPTS: usize = 10;

/// How often the session log gets a "still running" line while the script
/// executes. Frequent enough that the dashboard never looks frozen for long,
/// rare enough not to bury the real output the script emits at the end.
const BOOTSTRAP_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

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

    // Cheap fast path so the overwhelmingly common "one already running"
    // rejection costs nothing — the *authoritative* single-session gate is
    // `try_begin` below, which does this check and the insertion under one
    // lock. This check alone is a TOCTOU window: two POSTs (a double-click
    // in the dashboard is enough) can both pass it before either registers
    // its session.
    if state.bootstraps.any_active() {
        return Err(AppError::conflict(
            "bootstrap_in_progress",
            "a bootstrap is already running; wait for it to finish before starting another",
            request_id.0,
        ));
    }

    let source_alias = pick_builder_source(&state, request_id.0).await?;
    let source = state
        .templates
        .resolve_alias(&source_alias)
        .ok_or_else(|| AppError::internal(request_id.0))?;

    let micro_network_id = builder_micro_network_id(&state, request_id.0).await?;

    let create_request = CreateVmRequest {
        name: builder_vm_name(&format!("bootstrap-{alias}")),
        template: source_alias.clone(),
        // MicroBoot boots into Alpine's RAM-backed recovery shell, not a
        // disk-backed template rootfs — every guest script's
        // download+extract+chroot-install staging area now lives in guest
        // RAM instead of on disk, so the builder needs far more of it than
        // it used to. 1024 MiB OOM-killed ubuntu's apt-get outright while
        // verifying Task 8 of the MicroBoot plan (one dependency,
        // linux-firmware-nvidia-graphics, is 109 MB by itself). Firecracker
        // allocates guest memory lazily, so this headroom only costs what a
        // given distro actually touches. Rocky additionally sizes its tmpfs
        // work area off this number — see bootstrap-rocky-in-guest.sh.
        ram: 8192,
        cpu: 1,
        // The target's own build headroom, but never below the floor the
        // source template itself needs to boot at all — a source rootfs
        // bigger than the target's build budget would otherwise fail
        // create validation (`handlers::images::min_disk_gb_for`).
        disk_gb: bootstrap_disk_gb(&alias)
            .max(super::builds::builder_disk_gb(source.rootfs.length())),
        egress_policy: EgressPolicy::Internet,
        micro_network_id,
        storage_root: None,
    };

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

    // Registered last, mirroring where `begin` always sat: a session
    // inserted before `start_vm_request` would be stranded in `Booting`
    // forever (blocking every future bootstrap) if starting the VM failed.
    // `try_begin` returning `None` means a concurrent request won the race
    // — tear this VM back down rather than leaking it, since nothing else
    // tracks it.
    let Some(bootstrap_id) = state
        .bootstraps
        .try_begin(&alias, &source_alias, created.id)
    else {
        teardown_builder_vm(&state, created.id, request_id).await;
        return Err(AppError::conflict(
            "bootstrap_in_progress",
            "a bootstrap is already running; wait for it to finish before starting another",
            request_id.0,
        ));
    };

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

/// Ensures the shared MicroBoot builder source is downloaded, converted and
/// registered (a no-op after the first call — see `crate::microboot`'s own
/// doc comment), and returns its alias for `CreateVmRequest.template`. No
/// longer depends on any template being installed: this is what closes the
/// bootstrap boundary `docs/superpowers/specs/2026-08-03-m2image-web-rebuild-design.md`
/// left open (`docs/superpowers/specs/2026-08-05-m2image-microboot-design.md`).
///
/// `ensure_registered`'s failure detail is a dynamic `String` (a download or
/// filesystem error), while `AppError::unavailable` takes a `&'static str`
/// message (every other `AppError` constructor in this crate does too — see
/// `error.rs`) — so, matching the convention `handlers::micro_networks` and
/// `handlers::vms` already use for dynamic failures, the real reason is
/// logged via `tracing::error!` and the client gets a fixed, still-actionable
/// message instead.
async fn pick_builder_source(state: &AppState, request_id: Uuid) -> Result<String, AppError> {
    crate::microboot::ensure_registered(state)
        .await
        .map_err(|reason| {
            tracing::error!(
                request_id = %request_id,
                reason,
                "failed to prepare the MicroBoot builder source"
            );
            AppError::unavailable(
                "failed to prepare the MicroBoot builder source for bootstrapping — see server logs",
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
                teardown_builder_vm(state, vm_id, RequestId(Uuid::new_v4())).await;
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
            teardown_builder_vm(state, vm_id, RequestId(Uuid::new_v4())).await;
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Waits for a shell to actually claim the builder VM's console
/// ([`wait_for_console_shell`]), then writes the guest-native bootstrap
/// script for `state.bootstraps.get(id).alias` to it as a single heredoc (so
/// the whole script lands as one shell invocation — no chunking or base64
/// needed, since `write_input` writes raw bytes to the guest's stdin pipe
/// and the guest's own shell parses embedded newlines exactly the way it
/// would typed input, including multi-line constructs), waits for
/// [`BOOTSTRAP_DONE_SENTINEL`] while a heartbeat keeps the session log
/// moving, and advances the session on success.
///
/// Every failure arm tears the builder VM down before returning: a
/// `VmPurpose::Builder` VM is invisible to `list_vms`, so one left running
/// here could never be found or reclaimed from the dashboard.
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
        teardown_builder_vm(state, vm_id, RequestId(Uuid::new_v4())).await;
        return;
    };

    let (_backlog, mut receiver) = process.console.subscribe();

    if let Err(reason) = wait_for_console_shell(&process.console, &mut receiver).await {
        state
            .bootstraps
            .finish_err_from(bootstrap_id, BootstrapStatus::Running, reason);
        teardown_builder_vm(state, vm_id, RequestId(Uuid::new_v4())).await;
        return;
    }

    // The builder is up and answering; everything from here until the
    // sentinel is the guest script doing the actual install.
    state
        .bootstraps
        .set_step(bootstrap_id, BootstrapStep::InstallingSystem);

    let heredoc = format!(
        "cat > /root/fc-bootstrap.sh <<'FIRECRAB_BOOTSTRAP_SCRIPT_EOF'\n{script}\nFIRECRAB_BOOTSTRAP_SCRIPT_EOF\nsh /root/fc-bootstrap.sh; echo \"{BOOTSTRAP_DONE_SENTINEL}:$?\"\n"
    );
    process.console.write_input(heredoc.as_bytes()).await;

    let heartbeat = spawn_progress_heartbeat(state, bootstrap_id);
    let outcome = super::packages::wait_for_completion_with_sentinel(
        &mut receiver,
        BOOTSTRAP_SCRIPT_TIMEOUT,
        BOOTSTRAP_DONE_SENTINEL,
    )
    .await;
    heartbeat.abort();

    match outcome {
        Ok((0, tail)) => {
            state.bootstraps.append_log(bootstrap_id, tail);
            if state.bootstraps.set_status_from(
                bootstrap_id,
                BootstrapStatus::Running,
                BootstrapStatus::Packaging,
            ) {
                state
                    .bootstraps
                    .set_step(bootstrap_id, BootstrapStep::Packaging);
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
            teardown_builder_vm(state, vm_id, RequestId(Uuid::new_v4())).await;
        }
        Err(reason) => {
            state
                .bootstraps
                .finish_err_from(bootstrap_id, BootstrapStatus::Running, reason);
            teardown_builder_vm(state, vm_id, RequestId(Uuid::new_v4())).await;
        }
    }
}

/// Confirms a shell is really reading the builder VM's console before the
/// bootstrap heredoc is pushed at it. `VmState::Running` is set by the
/// guest's network-readiness sentinel — an init service that fires *before*
/// agetty has necessarily autologin'd a shell onto ttyS0 — and agetty
/// flushes the tty's input queue when it opens it, so a heredoc written a
/// moment too early is silently swallowed and the session then sits until
/// [`BOOTSTRAP_SCRIPT_TIMEOUT`] (~30 min) with nothing to show for it.
///
/// The probe deliberately asks the guest to expand `$?` rather than echoing
/// a literal code: the tty's own line discipline echoes typed bytes back
/// even with no process reading them, so only an unexpanded-`$?`-turned-
/// number proves a shell actually consumed the input (the same reason
/// `packages::find_sentinel` can't be fooled by the echoed command text).
async fn wait_for_console_shell(
    console: &crate::console::ConsoleBroker,
    receiver: &mut tokio::sync::broadcast::Receiver<Vec<u8>>,
) -> Result<(), String> {
    for _ in 0..CONSOLE_PROBE_ATTEMPTS {
        console
            .write_input(format!("echo \"{CONSOLE_PROBE_SENTINEL}:$?\"\n").as_bytes())
            .await;
        match super::packages::wait_for_completion_with_sentinel(
            receiver,
            CONSOLE_PROBE_TIMEOUT,
            CONSOLE_PROBE_SENTINEL,
        )
        .await
        {
            Ok(_) => return Ok(()),
            // A closed console will never answer a later probe either, so
            // stop immediately instead of burning the whole retry budget.
            Err(reason) if reason.starts_with("console closed") => {
                return Err(format!(
                    "builder VM's console closed before a shell became responsive ({reason})"
                ));
            }
            Err(_) => {}
        }
    }
    Err(format!(
        "builder VM's console shell never became responsive after {CONSOLE_PROBE_ATTEMPTS} probes"
    ))
}

/// Appends a "still running" line to the session log every
/// [`BOOTSTRAP_HEARTBEAT_INTERVAL`] while the script executes. Live
/// progress is the inline console's job now; this line's remaining purpose
/// is the exported log, which is otherwise silent for the whole
/// multi-minute install and gives a later reader no way to tell a slow run
/// from a hung one. Runs on its own task so it never delays the sentinel
/// wait it accompanies — the caller aborts it the moment that wait returns.
fn spawn_progress_heartbeat(state: &AppState, bootstrap_id: Uuid) -> tokio::task::JoinHandle<()> {
    let state = state.clone();
    tokio::spawn(async move {
        let started = tokio::time::Instant::now();
        loop {
            tokio::time::sleep(BOOTSTRAP_HEARTBEAT_INTERVAL).await;
            // Stop as soon as the session leaves `Running` (finished, failed,
            // or cancelled) — an abort may not have landed yet, and a
            // heartbeat after the terminal line would be misleading.
            match state.bootstraps.get(bootstrap_id) {
                Some(session) if session.status == BootstrapStatus::Running => {
                    state.bootstraps.append_log(
                        bootstrap_id,
                        format!(
                            "still running the install script ({}m elapsed)",
                            started.elapsed().as_secs() / 60
                        ),
                    );
                }
                _ => return,
            }
        }
    })
}

/// Stops (only when it isn't already delete-eligible) and then deletes a
/// builder VM. Every bootstrap failure path funnels through here: a
/// `VmPurpose::Builder` VM is hidden from `list_vms`, so one left behind by
/// an unhandled error is unreachable from the dashboard — its Firecracker
/// process, TAP device, IP lease and multi-GB disk would all leak with no
/// in-product way to reclaim them. Same stop-then-delete sequence
/// `cancel_bootstrap` and `handlers::builds::cancel_build` use, and
/// best-effort for the same reason: the failure already being reported is
/// the one that matters, not a second one raised while cleaning up after it.
async fn teardown_builder_vm(state: &AppState, vm_id: Uuid, request_id: RequestId) {
    let can_delete_now = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&vm_id)
        .map(|vm| vm.state.can_delete())
        .unwrap_or(true);
    if !can_delete_now {
        let _ = super::vms::stop_vm(
            State(state.clone()),
            Extension(request_id),
            Path(vm_id.to_string()),
        )
        .await;
    }

    let _ = super::vms::delete_vm(
        State(state.clone()),
        Extension(request_id),
        Path(vm_id.to_string()),
    )
    .await;
}

/// Stops the builder VM, dumps the finished rootfs/kernel/initrd out of its
/// now-quiesced disk, converts the raw kernel to an uncompressed ELF vmlinux
/// the same way `install-{alpine,ubuntu,rocky}-rootfs.sh` always did on the
/// host, packs everything into `{alias}.tar.zst` at the exact layout
/// `templates.rs::default_specs()` expects, and writes it to
/// `image_install::staged_package_path` — where `GET /api/images` reports it
/// as `packageStaged`, so the dashboard can offer a local install
/// (`POST /api/images/{alias}/install`) with no remote package URL involved
/// — then deletes the builder VM.
///
/// Ordering is load-bearing and mirrors `handlers::builds::run_finalize`:
/// stop first, package second, delete last.
async fn package_bootstrap(state: &AppState, bootstrap_id: Uuid, vm_id: Uuid) {
    let request_id = RequestId(Uuid::new_v4());
    let Some(session) = state.bootstraps.get(bootstrap_id) else {
        return;
    };
    let Some(spec) = crate::templates::TemplateRegistry::known_spec(&session.alias) else {
        state
            .bootstraps
            .finish_err(bootstrap_id, format!("no known spec for {}", session.alias));
        teardown_builder_vm(state, vm_id, request_id).await;
        return;
    };

    // The disk below is read with `debugfs` straight off the host
    // filesystem. While Firecracker is still running, the guest has that
    // very image mounted read-write with its own dirty page cache over it,
    // so a read here can see a half-written ext4 — and `dump_from_image`'s
    // only success check is "the file exists and is non-empty", which a
    // truncated rootfs passes. Stopping first is what makes the read safe.
    if super::vms::stop_vm(
        State(state.clone()),
        Extension(request_id),
        Path(vm_id.to_string()),
    )
    .await
    .is_err()
    {
        // Nothing has been read or published yet, so this really is an
        // end-to-end failure — land on a terminal status rather than
        // packaging a live disk anyway. Same reasoning (and same deliberate
        // absence of a delete attempt, which `stop_vm`'s failure means would
        // be refused too) as `run_finalize`'s own stop failure branch.
        state.bootstraps.finish_err(
            bootstrap_id,
            format!("failed to stop builder VM {vm_id} before packaging; nothing was published"),
        );
        return;
    }

    if let Err(reason) = package_bootstrap_inner(state, &session, &spec).await {
        state.bootstraps.finish_err(bootstrap_id, reason);
        teardown_builder_vm(state, vm_id, request_id).await;
        return;
    }

    // Package is staged; the session's remaining work is teardown.
    state
        .bootstraps
        .set_step(bootstrap_id, BootstrapStep::Finalizing);

    if super::vms::delete_vm(
        State(state.clone()),
        Extension(request_id),
        Path(vm_id.to_string()),
    )
    .await
    .is_err()
    {
        // The package IS staged and installable by this point — the actual
        // goal of this session already succeeded — so it still reports
        // success. Log the vm_id, since `VmPurpose::Builder` hides the VM
        // from `list_vms` and this line is the operator's only way to find
        // it for manual removal (same handling as `run_finalize`'s).
        state.bootstraps.append_log(
            bootstrap_id,
            format!(
                "warning: builder VM {vm_id} could not be deleted after packaging succeeded; remove it manually"
            ),
        );
    }

    state.bootstraps.finish_ok(bootstrap_id);
}

async fn package_bootstrap_inner(
    state: &AppState,
    session: &BootstrapResponse,
    spec: &crate::templates::TemplateSpec,
) -> Result<(), String> {
    let vm_record = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&session.vm_id)
        .cloned()
        .ok_or_else(|| "builder VM record vanished before packaging".to_owned())?;
    let disk_generation = vm_record
        .disk_generation
        .ok_or_else(|| "builder VM has no disk generation to package from".to_owned())?;
    let artifact_paths = crate::artifacts::VmArtifactPaths::for_vm(
        &state.vms_dir_for(&vm_record.storage_root),
        session.vm_id,
    );
    let guest_disk = artifact_paths.rootfs(disk_generation);
    let alias = session.alias.clone();
    let spec = spec.clone();
    let image_root = state.templates.image_root_path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        build_package_blocking(&guest_disk, &alias, &spec, &image_root)
    })
    .await
    .map_err(|error| format!("packaging task panicked: {error}"))?
}

/// The synchronous half of packaging: dump files, convert the kernel,
/// stage them under a scratch directory in the right `kernel/`/`rootfs/`
/// layout, tar+zstd, then publish atomically into the staged package
/// cache (temp-file-then-rename, same discipline as
/// `image_install::download_to`).
fn build_package_blocking(
    guest_disk: &std::path::Path,
    alias: &str,
    spec: &crate::templates::TemplateSpec,
    image_root: &std::path::Path,
) -> Result<(), String> {
    let scratch = image_root
        .join(".packages")
        .join(format!(".{alias}-bootstrap-scratch"));
    let _ = std::fs::remove_dir_all(&scratch);
    let kernel_dir = scratch.join("kernel");
    let rootfs_dir = scratch.join("rootfs");
    std::fs::create_dir_all(&kernel_dir)
        .map_err(|e| format!("mkdir {}: {e}", kernel_dir.display()))?;
    std::fs::create_dir_all(&rootfs_dir)
        .map_err(|e| format!("mkdir {}: {e}", rootfs_dir.display()))?;

    let raw_rootfs = scratch.join("rootfs.raw.ext4");
    // Root-level, not /root/fc-bootstrap/out/... — MicroBoot's guest
    // scripts wrap their whole $out directory directly onto /dev/vda as
    // its own filesystem (see the bootstrap-{alpine,ubuntu,rocky}-in-guest.sh
    // scripts' own comments on this), so the paths inside it are relative
    // to that filesystem's root.
    crate::rootfs::dump_from_image(guest_disk, "/rootfs.ext4", &raw_rootfs)
        .map_err(|e| format!("dump rootfs: {e}"))?;
    let rootfs_dest = scratch.join(&spec.rootfs); // spec.rootfs = "rootfs/<exact-filename>.ext4"
    std::fs::create_dir_all(rootfs_dest.parent().unwrap()).ok();
    std::fs::rename(&raw_rootfs, &rootfs_dest).map_err(|e| format!("place rootfs: {e}"))?;

    let raw_kernel_name = if alias == "alpine-3.24" {
        "vmlinuz-virt-raw"
    } else {
        "vmlinuz-raw"
    };
    let raw_kernel = scratch.join("kernel.raw");
    crate::rootfs::dump_from_image(guest_disk, &format!("/{raw_kernel_name}"), &raw_kernel)
        .map_err(|e| format!("dump kernel: {e}"))?;

    let kernel_dest = scratch.join(&spec.kernel); // e.g. "kernel/vmlinux-ubuntu-26.04-x86_64"
    std::fs::create_dir_all(kernel_dest.parent().unwrap()).ok();
    // Compile-time repo-relative path, not `std::env::current_dir()`-based:
    // the deployed `firecrab-api.service` unit pins `WorkingDirectory` to
    // `@DATADIR@` (see `packaging/systemd/firecrab-api.service`), which has
    // no `scripts/` subdirectory at all, so a cwd-relative join would fail
    // in production every time regardless of where the process happens to
    // be started from. `env!("CARGO_MANIFEST_DIR")` is the same convention
    // `templates.rs::load_default` already uses to locate `images/`
    // relative to this crate at compile time, and it resolves correctly
    // here too: `install.sh` always runs `cargo build --release` in place
    // inside the checked-out repo on the target host (see `install.sh`),
    // so the path baked in at compile time is exactly this host's own
    // checkout, independent of the service's runtime working directory.
    let extract_vmlinux = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../scripts/firecracker-menual/extract-vmlinux");
    let output = std::process::Command::new(&extract_vmlinux)
        .arg(&raw_kernel)
        .output()
        .map_err(|e| format!("run extract-vmlinux ({}): {e}", extract_vmlinux.display()))?;
    // `extract-vmlinux`'s own exit code doesn't reliably reflect whether it
    // actually found a vmlinux (verified directly: on a raw kernel it can't
    // recognize, it prints "Cannot find vmlinux." to stderr but still exits
    // 0 with empty stdout, since its final `echo` becomes the script's last
    // command) — the same caveat `rootfs.rs` already documents for
    // `debugfs`, so success is confirmed positively here too: a real
    // extraction always produces a non-empty stdout.
    if !output.status.success() || output.stdout.is_empty() {
        return Err(format!(
            "extract-vmlinux failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    std::fs::write(&kernel_dest, &output.stdout).map_err(|e| format!("write kernel: {e}"))?;

    if let Some(initrd_relative) = &spec.initrd {
        let raw_initrd = scratch.join("initramfs.raw");
        crate::rootfs::dump_from_image(guest_disk, "/initramfs", &raw_initrd)
            .map_err(|e| format!("dump initrd: {e}"))?;
        let initrd_dest = scratch.join(initrd_relative);
        std::fs::create_dir_all(initrd_dest.parent().unwrap()).ok();
        std::fs::rename(&raw_initrd, &initrd_dest).map_err(|e| format!("place initrd: {e}"))?;
    }

    let package_name = crate::image_install::package_name(alias);
    let staged = crate::image_install::staged_package_path(image_root, alias);
    let staging_temp = staged.with_file_name(format!(".{package_name}.building"));
    std::fs::create_dir_all(staged.parent().unwrap()).ok();

    let members: Vec<std::path::PathBuf> = if spec.initrd.is_some() {
        vec![
            spec.kernel.clone(),
            spec.initrd.clone().unwrap(),
            spec.rootfs.clone(),
        ]
    } else {
        vec![spec.kernel.clone(), spec.rootfs.clone()]
    };
    let mut tar = std::process::Command::new("tar")
        .arg("--sparse")
        .arg("-C")
        .arg(&scratch)
        .arg("-cf")
        .arg("-")
        .args(&members)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn tar: {e}"))?;
    let tar_stdout = tar.stdout.take().ok_or("tar stdout missing")?;
    let zstd = std::process::Command::new("zstd")
        .args(["-T0", "-19", "-f", "-o"])
        .arg(&staging_temp)
        .stdin(tar_stdout)
        .status()
        .map_err(|e| format!("run zstd: {e}"))?;
    let tar_status = tar.wait().map_err(|e| format!("tar wait: {e}"))?;
    // Both subprocesses must succeed: `!a.success() || !b.success()` is the
    // De Morgan form of "error unless both tar and zstd succeeded" — it
    // trips (and this function returns Err) if *either* one failed, not
    // only if both did. Do not simplify this to `&&`: that would silently
    // ignore a single failed subprocess (e.g. tar exiting non-zero while
    // zstd still emits a zero-length or truncated archive from a closed
    // pipe) and let a broken package get renamed into place as if it had
    // succeeded.
    if !tar_status.success() || !zstd.success() {
        let _ = std::fs::remove_file(&staging_temp);
        return Err(format!("packaging failed (tar {tar_status}, zstd {zstd})"));
    }

    std::fs::rename(&staging_temp, &staged).map_err(|e| format!("publish package: {e}"))?;
    let _ = std::fs::remove_dir_all(&scratch);
    Ok(())
}

/// `GET /api/images/bootstrap/{bootstrapId}`.
pub async fn get_bootstrap(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<BootstrapResponse>, AppError> {
    let id = parse_id(&id, request_id.0)?;
    state
        .bootstraps
        .get(id)
        .map(Json)
        .ok_or_else(|| AppError::not_found(request_id.0))
}

/// `DELETE /api/images/bootstrap/{bootstrapId}` — tears down the builder VM
/// without packaging anything. Mirrors `handlers::builds::cancel_build`
/// exactly (same stop-then-delete-if-needed sequence, same best-effort
/// swallowing of VM teardown errors).
pub async fn cancel_bootstrap(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let parsed_id = parse_id(&id, request_id.0)?;
    let session = state
        .bootstraps
        .get(parsed_id)
        .ok_or_else(|| AppError::not_found(request_id.0))?;

    teardown_builder_vm(&state, session.vm_id, request_id).await;

    state.bootstraps.remove(parsed_id);
    Ok(StatusCode::NO_CONTENT)
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
    use firecrab_api_types::{BootstrapStep, BootstrapStepOutcome};

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

    /// A minimal 64-byte ELF64 header (x86-64 executable, no sections) —
    /// enough for `readelf -h` to accept it, which is what
    /// `extract-vmlinux`'s own `check_vmlinux` uses to recognize an
    /// already-uncompressed vmlinux and `cat` it straight through
    /// (verified directly against the real script). Arbitrary non-ELF
    /// bytes won't do here: `extract-vmlinux` exits 0 with empty stdout
    /// when it can't recognize its input at all (its final `echo` is the
    /// last command run, not an explicit `exit 1`), which is exactly the
    /// unreliable-exit-code case `build_package_blocking` now guards
    /// against by also requiring non-empty stdout — so a test fixture that
    /// used unrecognizable bytes here would fail for a different reason
    /// than the one it's meant to test.
    const FAKE_ELF64_HEADER: &[u8] = &[
        0x7f, 0x45, 0x4c, 0x46, 0x02, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x02, 0x00, 0x3e, 0x00, 0x01, 0x00, 0x00, 0x00, 0x60, 0xa7, 0x49, 0x03, 0x00, 0x00,
        0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x88, 0x1d, 0x30, 0x03, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x38, 0x00, 0x03, 0x00, 0x40, 0x00,
        0x2f, 0x00, 0x2e, 0x00,
    ];

    /// Seeds a builder VM record in `vm_state`.
    ///
    /// Every packaging test drives this at `VmState::Running`, the state a
    /// real bootstrap's builder VM is always in when `package_bootstrap`
    /// runs. `test_support::record()` defaults to `Created`, which
    /// `delete_vm` accepts outright — so a `Created` fixture lets the whole
    /// stop-then-read ordering pass vacuously without ever exercising it,
    /// which is exactly how a live-disk read shipped through twelve task
    /// reviews. `stop_vm` needs nothing beyond this in-memory state to
    /// succeed (its SIGTERM step is skipped when `state.processes` has no
    /// entry for the VM, and `teardown_vm_network` is best-effort against
    /// the fixture's always-ok network helper), the same technique
    /// `handlers::builds`'s own finalize tests use.
    fn seed_builder_vm(state: &AppState, vm_state: VmState) -> crate::model::VmRecord {
        let vm = crate::model::VmRecord {
            state: vm_state,
            ..crate::handlers::vms::test_support::record("builder", Uuid::new_v4())
        };
        crate::handlers::vms::test_support::seed_vm(state, &vm);
        vm
    }

    /// Puts a real (tiny) ext4 image carrying `files` where
    /// `package_bootstrap` expects the builder VM's current-generation disk,
    /// and records that generation on the VM — the fixture stand-in for what
    /// a finished guest-side bootstrap script leaves behind.
    fn seed_builder_disk(state: &AppState, vm: &crate::model::VmRecord, files: &[(&str, &[u8])]) {
        let generation = Uuid::new_v4();
        let artifact_paths =
            crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id);
        artifact_paths.ensure_directories().unwrap();
        seed_builder_output_disk(&artifact_paths.rootfs(generation), files);
        let mut vms = state.vms.lock().unwrap();
        vms.get_mut(&vm.id).unwrap().disk_generation = Some(generation);
    }

    /// Registers a session already advanced to `status`, the way the earlier
    /// stage of a real run would have left it.
    fn seeded_session(
        state: &AppState,
        alias: &str,
        source_alias: &str,
        vm_id: Uuid,
        status: BootstrapStatus,
    ) -> Uuid {
        let id = state
            .bootstraps
            .try_begin(alias, source_alias, vm_id)
            .expect("no other bootstrap session is active");
        state
            .bootstraps
            .set_status_from(id, BootstrapStatus::Booting, status);
        id
    }

    /// Builds a small ext4 image at `disk_path` and seeds it with `files`
    /// (paths relative to the image root, e.g. `"rootfs.ext4"` -> content) —
    /// the fixture stand-in for MicroBoot's guest scripts wrapping their
    /// whole `$out` directory directly onto `/dev/vda` as its own
    /// filesystem, so seeded files land at the image's root rather than
    /// under any nested guest directory (see `build_package_blocking`'s own
    /// comment on this). No `mkdir` is needed first: `/` already exists on
    /// any freshly `mkfs.ext4`'d image.
    fn seed_builder_output_disk(disk_path: &std::path::Path, files: &[(&str, &[u8])]) {
        std::process::Command::new("mkfs.ext4")
            .args(["-q", "-F"])
            .arg(disk_path)
            .arg("16M")
            .status()
            .unwrap();
        for (name, content) in files {
            crate::rootfs::write_into_image(disk_path, &format!("/{name}"), content).unwrap();
        }
    }

    #[tokio::test]
    async fn package_bootstrap_writes_a_tar_zst_the_install_pipeline_can_read() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        // `Running`, like a real builder VM the bootstrap script just
        // finished on — `package_bootstrap` has to stop it before it may
        // read the disk, and delete it afterwards.
        let vm = seed_builder_vm(&state, VmState::Running);

        // The exact guest-side layout package_bootstrap expects to dump out.
        // `ubuntu-26.04` has `initrd: None` in `default_specs()`, so only
        // `rootfs.ext4` and `vmlinuz-raw` (Ubuntu's raw-kernel dump
        // filename per `bootstrap-ubuntu-in-guest.sh`) need seeding here.
        seed_builder_disk(
            &state,
            &vm,
            &[
                ("rootfs.ext4", b"fake ext4 rootfs bytes"),
                ("vmlinuz-raw", FAKE_ELF64_HEADER),
            ],
        );

        let bootstrap_id = seeded_session(
            &state,
            "ubuntu-26.04",
            "alpine-3.24",
            vm.id,
            BootstrapStatus::Packaging,
        );

        package_bootstrap(&state, bootstrap_id, vm.id).await;

        let snapshot = state.bootstraps.get(bootstrap_id).unwrap();
        assert_eq!(
            snapshot.status,
            BootstrapStatus::Succeeded,
            "log: {}",
            snapshot.log
        );
        let staged = crate::image_install::staged_package_path(
            state.templates.image_root_path(),
            "ubuntu-26.04",
        );
        assert!(staged.is_file());

        let listing = std::process::Command::new("tar")
            .arg("--use-compress-program=zstd")
            .arg("-tf")
            .arg(&staged)
            .output()
            .unwrap();
        assert!(listing.status.success());
        let members = String::from_utf8_lossy(&listing.stdout);
        assert!(members.contains("kernel/vmlinux-ubuntu-26.04-x86_64"));
        assert!(members.contains("rootfs/ubuntu-rootfs-26.04-amd64.ext4"));
        // The builder VM is gone: a successful bootstrap must not leave a
        // Firecracker process, TAP, IP lease and multi-GB disk behind —
        // `VmPurpose::Builder` hides it from `list_vms`, so nothing in the
        // product could ever reclaim it.
        assert!(
            state.vms.lock().unwrap().get(&vm.id).is_none(),
            "the builder VM record must be deleted after a successful package"
        );
    }

    /// The stop must happen *before* the disk is read, not merely somewhere
    /// in the function: `dump_from_image` only checks that it produced a
    /// non-empty file, so a read from a still-live, rw-mounted, unsynced
    /// ext4 would publish a torn rootfs as a perfectly valid package.
    ///
    /// Driving that ordering without a real Firecracker process: a VM left
    /// in `Created` is one `stop_vm` refuses (`can_transition` allows only
    /// `Running -> Stopping`). If anything were packaged despite that
    /// refusal, a staged archive would exist — asserting there is none is
    /// what proves nothing was read off the un-stopped disk.
    #[tokio::test]
    async fn package_bootstrap_publishes_nothing_when_the_builder_vm_cannot_be_stopped() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = seed_builder_vm(&state, VmState::Created);
        seed_builder_disk(
            &state,
            &vm,
            &[
                ("rootfs.ext4", b"fake ext4 rootfs bytes"),
                ("vmlinuz-raw", FAKE_ELF64_HEADER),
            ],
        );

        let bootstrap_id = seeded_session(
            &state,
            "ubuntu-26.04",
            "alpine-3.24",
            vm.id,
            BootstrapStatus::Packaging,
        );

        package_bootstrap(&state, bootstrap_id, vm.id).await;

        let snapshot = state.bootstraps.get(bootstrap_id).unwrap();
        assert_eq!(snapshot.status, BootstrapStatus::Failed);
        assert!(
            snapshot.log.contains("failed to stop builder VM"),
            "log: {}",
            snapshot.log
        );
        let staged = crate::image_install::staged_package_path(
            state.templates.image_root_path(),
            "ubuntu-26.04",
        );
        assert!(
            !staged.is_file(),
            "nothing may be packaged from a builder VM that could not be stopped"
        );
    }

    /// `extract-vmlinux`'s own exit code doesn't reliably reflect whether it
    /// found a vmlinux (verified directly against the real script: given
    /// input it can't recognize, it exits 0 with empty stdout — its final
    /// `echo "Cannot find vmlinux."` becomes the script's last command,
    /// there's no explicit terminal `exit 1`). Without also checking for
    /// non-empty stdout, `build_package_blocking` would silently package a
    /// 0-byte kernel as a "successful" bootstrap. This seeds a raw kernel
    /// `extract-vmlinux` genuinely cannot recognize (plain text, not an ELF
    /// or any known compressed kernel format) and asserts packaging fails
    /// loudly instead.
    #[tokio::test]
    async fn package_bootstrap_fails_when_extract_vmlinux_cannot_recognize_the_raw_kernel() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = seed_builder_vm(&state, VmState::Running);
        seed_builder_disk(
            &state,
            &vm,
            &[
                ("rootfs.ext4", b"fake ext4 rootfs bytes"),
                (
                    "vmlinuz-raw",
                    b"not an elf or a known compressed kernel format",
                ),
            ],
        );

        let bootstrap_id = seeded_session(
            &state,
            "ubuntu-26.04",
            "alpine-3.24",
            vm.id,
            BootstrapStatus::Packaging,
        );

        package_bootstrap(&state, bootstrap_id, vm.id).await;

        let snapshot = state.bootstraps.get(bootstrap_id).unwrap();
        assert_eq!(snapshot.status, BootstrapStatus::Failed);
        assert!(
            snapshot.log.contains("extract-vmlinux failed"),
            "log: {}",
            snapshot.log
        );
        let staged = crate::image_install::staged_package_path(
            state.templates.image_root_path(),
            "ubuntu-26.04",
        );
        assert!(
            !staged.is_file(),
            "a failed extraction must not publish a package"
        );
        // A packaging failure still tears the builder VM down — otherwise
        // every failed bootstrap would leak one, invisibly.
        assert!(
            state.vms.lock().unwrap().get(&vm.id).is_none(),
            "the builder VM record must be deleted even when packaging fails"
        );
    }

    /// Covers the `if let Some(initrd_relative) = &spec.initrd` branch in
    /// `build_package_blocking` that the `ubuntu-26.04` test above never
    /// exercises (Ubuntu's spec has `initrd: None`) — `alpine-3.24` does
    /// carry an initrd, and its raw-kernel dump filename
    /// (`vmlinuz-virt-raw`, per `bootstrap-alpine-in-guest.sh`) differs
    /// from every other alias's `vmlinuz-raw`, which `build_package_blocking`
    /// branches on explicitly.
    #[tokio::test]
    async fn package_bootstrap_includes_the_initrd_when_the_spec_has_one() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = seed_builder_vm(&state, VmState::Running);
        seed_builder_disk(
            &state,
            &vm,
            &[
                ("rootfs.ext4", b"fake alpine rootfs bytes"),
                ("vmlinuz-virt-raw", FAKE_ELF64_HEADER),
                ("initramfs", b"fake initramfs bytes"),
            ],
        );

        let bootstrap_id = seeded_session(
            &state,
            "alpine-3.24",
            "ubuntu-26.04",
            vm.id,
            BootstrapStatus::Packaging,
        );

        package_bootstrap(&state, bootstrap_id, vm.id).await;

        let snapshot = state.bootstraps.get(bootstrap_id).unwrap();
        assert_eq!(
            snapshot.status,
            BootstrapStatus::Succeeded,
            "log: {}",
            snapshot.log
        );
        let staged = crate::image_install::staged_package_path(
            state.templates.image_root_path(),
            "alpine-3.24",
        );
        let listing = std::process::Command::new("tar")
            .arg("--use-compress-program=zstd")
            .arg("-tf")
            .arg(&staged)
            .output()
            .unwrap();
        assert!(listing.status.success());
        let members = String::from_utf8_lossy(&listing.stdout);
        assert!(members.contains("kernel/vmlinux-alpine-virt-x86_64"));
        assert!(members.contains("kernel/initramfs-alpine-virt-x86_64"));
        assert!(members.contains("rootfs/alpine-rootfs-3.24.1-x86_64.ext4"));
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
    async fn start_bootstrap_succeeds_with_no_installed_templates_once_microboot_is_registered() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);
        // test_state's one registered template, "ubuntu-rootfs-26.04", isn't
        // any of the 3 bootstrap-target aliases and (before this task)
        // wouldn't have satisfied rocky-9's old matching-source rule either
        // — this fixture is deliberately unchanged from the test it
        // replaces, to prove the same "no real target template installed"
        // situation that used to 503 now succeeds. Register __microboot
        // directly (mirroring Task 1's own registration test) rather than
        // exercising a real network download here.
        // Must be the version this build pins: `ensure_registered`'s fast
        // path requires an exact match, so a stale tag here would silently
        // send the test down the slow path and pull ~22 MB off the network.
        state
            .templates
            .register_spec(crate::templates::TemplateSpec {
                alias: crate::microboot::MICROBOOT_ALIAS.to_owned(),
                version: crate::microboot::microboot_version_for_test().to_owned(),
                kernel: std::path::PathBuf::from("kernel"),
                initrd: None,
                rootfs: std::path::PathBuf::from("rootfs"),
                boot_args: "console=ttyS0 reboot=k".to_owned(),
            })
            .unwrap();

        let (status, Json(session)) = start_bootstrap(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path("rocky-9".to_owned()),
        )
        .await
        .unwrap();

        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(session.alias, "rocky-9");
        assert_eq!(session.source_alias, crate::microboot::MICROBOOT_ALIAS);

        // The builder VM's own spec, not just the session's status. The RAM
        // figure is load-bearing, not cosmetic: at the previous 1024 MiB the
        // guest's staging area (now entirely in RAM under MicroBoot) got
        // ubuntu's apt-get OOM-killed on a real run, and nothing else in the
        // suite would notice the constant being lowered again.
        let builder = state
            .vms
            .lock()
            .unwrap()
            .values()
            .find(|vm| vm.id == session.vm_id)
            .cloned()
            .expect("builder VM record");
        assert_eq!(builder.template, crate::microboot::MICROBOOT_ALIAS);
        assert_eq!(builder.ram, 8192);
    }

    #[tokio::test]
    async fn run_bootstrap_script_records_the_console_output_and_reaches_running_terminal_wait() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = seed_builder_vm(&state, VmState::Running);
        let console = register_fake_process(&state, vm.id);
        let bootstrap_id = seeded_session(
            &state,
            "ubuntu-26.04",
            "alpine-3.24",
            vm.id,
            BootstrapStatus::Running,
        );

        let vm_id = vm.id;
        let push_sentinel = async {
            wait_for_console_subscriber(&console).await;
            // The console-readiness probe comes first now: nothing is
            // written to a console no shell has claimed yet (agetty flushes
            // the input queue when it opens the tty), so the script is only
            // pushed once a probe has been answered. Pushed as its own chunk
            // so the probe wait consumes only this one and leaves the
            // completion sentinel below for the real wait.
            console.push_output(format!("{CONSOLE_PROBE_SENTINEL}:0\n").as_bytes());
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

    /// `run_bootstrap_script` advances the step timeline at both of its own
    /// transitions: once the console probe confirms a shell is live (opens
    /// `InstallingSystem`, closing `StartingBuilderVm`), and again once the
    /// script's completion sentinel arrives with exit code 0 (opens
    /// `Packaging`, closing `InstallingSystem`). Same `join!` structure and
    /// fixtures as
    /// `run_bootstrap_script_records_the_console_output_and_reaches_running_terminal_wait`
    /// above, for the identical reason given in that test's comment: a
    /// separate `tokio::spawn` for `run_bootstrap_script` would race the
    /// `package_bootstrap` successor task it spawns on its own success path.
    /// That means `package_bootstrap` has not run by the time this
    /// assertion runs, so `Packaging` is asserted still open (`Running`),
    /// not `Succeeded` — asserting on the final timeline after the `join!`
    /// rather than trying to catch an intermediate state mid-run.
    #[tokio::test]
    async fn running_the_script_advances_the_step_timeline_through_installing_and_packaging() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = seed_builder_vm(&state, VmState::Running);
        let console = register_fake_process(&state, vm.id);
        let bootstrap_id = seeded_session(
            &state,
            "ubuntu-26.04",
            "alpine-3.24",
            vm.id,
            BootstrapStatus::Running,
        );

        let vm_id = vm.id;
        let push_sentinel = async {
            wait_for_console_subscriber(&console).await;
            console.push_output(format!("{CONSOLE_PROBE_SENTINEL}:0\n").as_bytes());
            console.push_output(format!("{}:0\n", BOOTSTRAP_DONE_SENTINEL).as_bytes());
        };
        tokio::join!(
            run_bootstrap_script(&state, bootstrap_id, vm_id),
            push_sentinel
        );

        let session = state.bootstraps.get(bootstrap_id).unwrap();
        assert_eq!(session.status, BootstrapStatus::Packaging);
        assert_eq!(session.current_step, Some(BootstrapStep::Packaging));
        assert_eq!(session.step_timeline.len(), 3);
        assert_eq!(
            session.step_timeline[0].step,
            BootstrapStep::StartingBuilderVm
        );
        assert_eq!(
            session.step_timeline[0].outcome,
            BootstrapStepOutcome::Succeeded
        );
        assert_eq!(
            session.step_timeline[1].step,
            BootstrapStep::InstallingSystem
        );
        assert_eq!(
            session.step_timeline[1].outcome,
            BootstrapStepOutcome::Succeeded
        );
        assert_eq!(session.step_timeline[2].step, BootstrapStep::Packaging);
        assert_eq!(
            session.step_timeline[2].outcome,
            BootstrapStepOutcome::Running
        );
    }

    #[tokio::test]
    async fn get_bootstrap_returns_the_tracked_session() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let id = state
            .bootstraps
            .try_begin("ubuntu-26.04", "alpine-3.24", Uuid::new_v4())
            .expect("no other bootstrap session is active");

        let Json(found) = get_bootstrap(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(id.to_string()),
        )
        .await
        .unwrap();

        assert_eq!(found.bootstrap_id, id);
    }

    #[tokio::test]
    async fn get_bootstrap_404s_for_an_unknown_id() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let error = get_bootstrap(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(Uuid::new_v4().to_string()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }

    /// Seeded `Running` rather than at `record()`'s default `Created`, so
    /// this exercises `teardown_builder_vm`'s stop-then-delete branch (a VM
    /// that `delete_vm` would refuse outright) — the branch every bootstrap
    /// failure path now shares, not just cancel.
    #[tokio::test]
    async fn cancel_bootstrap_stops_and_deletes_a_running_builder_vm_and_drops_the_session() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = seed_builder_vm(&state, VmState::Running);
        let id = state
            .bootstraps
            .try_begin("ubuntu-26.04", "alpine-3.24", vm.id)
            .expect("no other bootstrap session is active");

        let status = cancel_bootstrap(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(id.to_string()),
        )
        .await
        .unwrap();

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(state.bootstraps.get(id).is_none());
        assert!(
            state.vms.lock().unwrap().get(&vm.id).is_none(),
            "cancelling must not leave the builder VM behind"
        );
    }

    /// A boot that never reaches `Running` used to `finish_err_from` and
    /// return with no VM cleanup at all, stranding a `VmPurpose::Builder` VM
    /// that `list_vms` hides from the dashboard entirely.
    #[tokio::test]
    async fn watch_bootstrap_boot_deletes_the_builder_vm_when_it_fails_to_boot() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = seed_builder_vm(&state, VmState::Error);
        let bootstrap_id = state
            .bootstraps
            .try_begin("ubuntu-26.04", "alpine-3.24", vm.id)
            .expect("no other bootstrap session is active");

        watch_bootstrap_boot(&state, bootstrap_id, vm.id).await;

        let snapshot = state.bootstraps.get(bootstrap_id).unwrap();
        assert_eq!(snapshot.status, BootstrapStatus::Failed);
        assert!(
            snapshot.log.contains("failed to boot"),
            "log: {}",
            snapshot.log
        );
        assert!(
            state.vms.lock().unwrap().get(&vm.id).is_none(),
            "a builder VM that failed to boot must not be left behind"
        );
    }

    /// `VmState::Running` is set by the guest's network-readiness sentinel,
    /// which can fire before agetty has autologin'd a shell onto ttyS0 — and
    /// agetty flushes the tty input queue when it opens it, silently eating
    /// anything written early. Without the probe, that lands the session in
    /// a ~30-minute wait ending in a meaningless timeout; with it, the
    /// session fails fast and says why, and the builder VM is reclaimed.
    ///
    /// `start_paused` lets the probe's bounded retries burn their timeouts
    /// in virtual time (tokio auto-advances the clock while every task is
    /// parked on a timer), so this costs no real wall-clock seconds.
    #[tokio::test(start_paused = true)]
    async fn run_bootstrap_script_fails_when_no_shell_answers_the_console_probe() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        // Deliberately `Created`, not `Running`, even though a real builder
        // VM would be `Running` here: this is the one test that needs a
        // registered `VmProcess` (for its console), and that fixture's `pid`
        // is 0 — so letting the teardown reach `stop_vm` would have it call
        // `kill(0, SIGTERM)`, which signals the *entire process group*, test
        // runner included. `run_bootstrap_script` never reads the VM's
        // lifecycle state, so `Created` exercises the identical code path
        // while keeping teardown on the delete-only branch.
        let vm = seed_builder_vm(&state, VmState::Created);
        // A console that is subscribed to but never answers — exactly what a
        // tty with no shell reading it looks like from the host.
        let _console = register_fake_process(&state, vm.id);
        let bootstrap_id = seeded_session(
            &state,
            "ubuntu-26.04",
            "alpine-3.24",
            vm.id,
            BootstrapStatus::Running,
        );

        run_bootstrap_script(&state, bootstrap_id, vm.id).await;

        let snapshot = state.bootstraps.get(bootstrap_id).unwrap();
        assert_eq!(snapshot.status, BootstrapStatus::Failed);
        assert!(
            snapshot.log.contains("never became responsive"),
            "log: {}",
            snapshot.log
        );
        assert!(
            state.vms.lock().unwrap().get(&vm.id).is_none(),
            "an unresponsive console must not strand the builder VM"
        );
    }

    /// The session log is otherwise written exactly twice (VM booted, script
    /// finished), so a multi-minute script run looks frozen in the
    /// dashboard. The heartbeat is what distinguishes "still working" from
    /// "hung" — driven here in virtual time rather than waiting a real
    /// minute per tick.
    #[tokio::test(start_paused = true)]
    async fn a_long_running_script_keeps_appending_progress_to_the_session_log() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = seed_builder_vm(&state, VmState::Running);
        let bootstrap_id = seeded_session(
            &state,
            "ubuntu-26.04",
            "alpine-3.24",
            vm.id,
            BootstrapStatus::Running,
        );

        let heartbeat = spawn_progress_heartbeat(&state, bootstrap_id);
        tokio::time::sleep(BOOTSTRAP_HEARTBEAT_INTERVAL * 3 + Duration::from_secs(1)).await;
        heartbeat.abort();

        let log = state.bootstraps.get(bootstrap_id).unwrap().log;
        assert_eq!(
            log.matches("still running the install script").count(),
            3,
            "expected one heartbeat per interval elapsed; log: {log}"
        );
    }

    /// The heartbeat must stop on its own once the session leaves `Running`
    /// — an abort may not have landed yet, and a "still running" line after
    /// the terminal one would misreport a finished session.
    #[tokio::test(start_paused = true)]
    async fn the_progress_heartbeat_stops_once_the_session_leaves_running() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = seed_builder_vm(&state, VmState::Running);
        let bootstrap_id = seeded_session(
            &state,
            "ubuntu-26.04",
            "alpine-3.24",
            vm.id,
            BootstrapStatus::Running,
        );

        let heartbeat = spawn_progress_heartbeat(&state, bootstrap_id);
        state.bootstraps.finish_ok(bootstrap_id);
        tokio::time::sleep(BOOTSTRAP_HEARTBEAT_INTERVAL * 3).await;

        assert!(heartbeat.is_finished());
        let log = state.bootstraps.get(bootstrap_id).unwrap().log;
        assert!(!log.contains("still running the install script"), "log: {log}");
    }
}

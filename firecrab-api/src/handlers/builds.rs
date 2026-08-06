//! Web-triggered image builds: boot a "builder" VM off an installed
//! template, let the dashboard install/remove packages on its console
//! (`handlers::packages::run_package_action`, reused as-is), then snapshot
//! the resulting disk as a new template version (`finalize`, Task 9).
//!
//! A build session's VM is a completely ordinary VM — same create/start/
//! console/delete code path as any dashboard-created instance — tagged
//! `VmPurpose::Builder` so `list_vms` hides it. This avoids reimplementing
//! any part of VM lifecycle, network setup, or console handling for builds.

use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use firecrab_api_types::{
    BuildResponse, BuildStatus, CreateVmRequest, EgressPolicy, FinalizeBuildRequest,
    PackageUpdateStatus,
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::model::{VmPurpose, VmState};
use crate::server::RequestId;
use crate::state::AppState;
use crate::templates::TemplateRegistry;

use super::packages::PACKAGE_UPDATE_TIMEOUT;
use super::vms::{create_vm, parse_id, start_vm_request};

/// How often every background poll in this module samples the shared VM map.
/// Matches the cadence the dashboard itself polls a build session at, so a
/// status change is never more than one tick away from being visible.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// How long a builder VM may take to reach `Running` before its session is
/// failed. Generous on purpose: `start_vm`'s pipeline copies a multi-GB
/// rootfs (queued behind `disk_prep_permits`) before the guest even boots,
/// so this is a "something is genuinely wrong" bound, not a latency budget.
const BUILDER_BOOT_TIMEOUT: Duration = Duration::from_secs(600);

/// How long a package action may stay `Running` on the builder VM before
/// this module stops waiting for its outcome. Deliberately longer than
/// [`PACKAGE_UPDATE_TIMEOUT`], which is what actually bounds the console
/// wait: `run_action` always records a terminal `packageUpdate` by then, so
/// waiting a little past it means a real outcome is observed instead of the
/// session flipping back to `Ready` while the guest is still installing —
/// exactly the race `finalize_build`'s `Installing` guard exists to close.
const PACKAGE_OUTCOME_TIMEOUT: Duration =
    PACKAGE_UPDATE_TIMEOUT.saturating_add(Duration::from_secs(60));

/// `POST /api/images/{alias}/build` — boots a builder VM off `alias`'s
/// currently installed version and registers a new build session. Returns
/// immediately once the VM's `create_vm`/`start_vm_request` calls have been
/// issued; the caller polls `GET /api/images/builds/{buildId}` (and the
/// existing `/ws/vms/{vmId}/console`) for progress.
pub async fn start_build(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(alias): Path<String>,
) -> Result<(StatusCode, Json<BuildResponse>), AppError> {
    let Some(source) = state.templates.resolve_alias(&alias) else {
        return Err(AppError::not_found(request_id.0));
    };

    let micro_network_id = builder_micro_network_id(&state, request_id.0).await?;
    let disk_gb = builder_disk_gb(source.rootfs.length());

    let create_request = CreateVmRequest {
        name: builder_vm_name(&alias),
        template: alias.clone(),
        ram: 1024,
        cpu: 1,
        disk_gb,
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

    // Response deliberately discarded: `start_build`'s own response reflects
    // the build session, not the VM's just-issued `starting` snapshot — the
    // dashboard polls `GET /api/images/builds/{buildId}` for that instead.
    let _vm_response = start_vm_request(
        State(state.clone()),
        Extension(request_id),
        Path(created.id.to_string()),
    )
    .await?;

    let build_id = state.builds.begin(&alias, created.id);

    // `start_vm_request` returns as soon as the VM is claimed `Starting` —
    // its real boot pipeline runs on a detached task. Nothing else ever
    // observes that VM reaching `Running`, so without this watcher the
    // session would sit at `Booting` forever and the dashboard's build
    // modal (which gates every control on `ready`) would stay inert.
    let state_for_watch = state.clone();
    tokio::spawn(async move {
        watch_builder_boot(&state_for_watch, build_id, created.id).await;
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(state.builds.get(build_id).expect("just inserted")),
    ))
}

/// Polls the builder VM's lifecycle state until it reaches `Running`
/// (session becomes `Ready`), lands on a terminal failure, or
/// [`BUILDER_BOOT_TIMEOUT`] elapses (session becomes `Failed`).
///
/// Every transition is compare-and-set against `Booting`, so a session that
/// something else already advanced (a cancel that dropped it, a request that
/// moved it on) is left alone rather than clobbered by a late tick.
pub(crate) async fn watch_builder_boot(state: &AppState, build_id: Uuid, vm_id: Uuid) {
    let deadline = tokio::time::Instant::now() + BUILDER_BOOT_TIMEOUT;
    loop {
        // A session that is gone (cancelled) or already past `Booting` has
        // no transition left for this watcher to make.
        match state.builds.get(build_id) {
            Some(session) if session.status == BuildStatus::Booting => {}
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
                state.builds.append_log(
                    build_id,
                    "builder VM is running — install or remove packages, then save",
                );
                state
                    .builds
                    .set_status_from(build_id, BuildStatus::Booting, BuildStatus::Ready);
                return;
            }
            // `run_start` lands a failed boot on `Error`; the exit monitor
            // lands a guest that died right after start on `Stopped`. Both
            // mean this session will never become usable.
            Some(state_now @ (VmState::Error | VmState::Stopped)) => {
                state.builds.finish_err_from(
                    build_id,
                    BuildStatus::Booting,
                    format!("builder VM {vm_id} failed to boot (state: {state_now:?})"),
                );
                return;
            }
            // The record vanished under us — only `delete_vm` does that, and
            // for a builder VM that means the session was torn down.
            None => return,
            Some(_) => {}
        }

        if tokio::time::Instant::now() >= deadline {
            state.builds.finish_err_from(
                build_id,
                BuildStatus::Booting,
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

/// Names the builder VM so it's recognizable if an operator inspects
/// `data/firecrab.db` directly; not shown anywhere in the dashboard since
/// `list_vms` filters `Builder` records out.
fn builder_vm_name(alias: &str) -> String {
    format!("builder-{alias}-{}", &Uuid::new_v4().to_string()[..8])
}

/// Builder VMs need headroom beyond the source rootfs to install new
/// packages into — a fixed 2 GiB margin over the template's own floor,
/// matching `handlers::images::min_disk_gb_for`'s ceiling logic.
fn builder_disk_gb(rootfs_bytes: u64) -> u16 {
    const GIB: u64 = 1024 * 1024 * 1024;
    let floor: u16 = rootfs_bytes.div_ceil(GIB).try_into().unwrap_or(u16::MAX);
    floor.saturating_add(2)
}

/// Picks the first MicroNetwork with internet egress enabled — a build
/// needs to reach the guest's package repositories. Fails clearly rather
/// than silently picking an isolated network a package install would hang
/// against.
async fn builder_micro_network_id(state: &AppState, request_id: Uuid) -> Result<Uuid, AppError> {
    let store = state.store.clone();
    let networks = tokio::task::spawn_blocking(move || store.list_micro_networks())
        .await
        .map_err(|_| AppError::internal(request_id))?
        .map_err(|_| AppError::internal(request_id))?;
    networks
        .into_iter()
        .find(|network| network.internet_enabled)
        .map(|network| network.id)
        .ok_or_else(|| {
            AppError::unavailable(
                "no MicroNetwork with internet access exists — create one before building images",
                request_id,
            )
        })
}

/// Flags the just-created VM as a builder so `list_vms` hides it, then
/// persists that change the same way `handlers::vms::persist_update` does.
async fn mark_as_builder(state: &AppState, vm_id: Uuid, request_id: Uuid) -> Result<(), AppError> {
    let record = {
        let mut vms = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let vm = vms
            .get_mut(&vm_id)
            .ok_or_else(|| AppError::internal(request_id))?;
        vm.purpose = VmPurpose::Builder;
        vm.clone()
    };
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || store.update(&record))
        .await
        .map_err(|_| AppError::internal(request_id))?
        .map_err(|_| AppError::internal(request_id))
}

/// `GET /api/images/builds` — every build session this process has tracked
/// (survives VM stop/delete since `BuildTracker` is independent of the VM
/// lifecycle; does not survive an API restart, matching `ImageInstallTracker`).
pub async fn list_builds(State(state): State<AppState>) -> Json<Vec<BuildResponse>> {
    Json(state.builds.list())
}

/// `GET /api/images/builds/{buildId}`.
pub async fn get_build(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(build_id): Path<String>,
) -> Result<Json<BuildResponse>, AppError> {
    let build_id = parse_id(&build_id, request_id.0)?;
    state
        .builds
        .get(build_id)
        .map(Json)
        .ok_or_else(|| AppError::not_found(request_id.0))
}

/// `POST /api/images/builds/{buildId}/packages` — runs one install/remove/
/// update action on the build session's VM by delegating straight to
/// `handlers::packages::run_package_action` (same validation, same
/// sentinel-wait mechanics) and mirrors its resulting `packageUpdate`
/// status into the build session's own log.
///
/// Returns as soon as the command has been dispatched, with the session in
/// `Installing` — a real `apt-get install` runs for minutes, far past
/// `enforce_limits`' request timeout, and axum drops a timed-out handler's
/// future, which would strand the session in `Installing` forever. The
/// outcome is recorded by a detached task instead (same shape as
/// `run_package_action` itself, and as every other long operation here),
/// and the dashboard's existing 1s poll of `GET /api/images/builds/{id}`
/// picks it up.
pub async fn build_packages(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(build_id): Path<String>,
    Json(body): Json<firecrab_api_types::PackageAction>,
) -> Result<Json<BuildResponse>, AppError> {
    let build_id = parse_id(&build_id, request_id.0)?;
    let session = state
        .builds
        .get(build_id)
        .ok_or_else(|| AppError::not_found(request_id.0))?;

    // Only a `Ready` session can take one. `Booting` has no console to write
    // to yet (and its boot watcher stops watching the moment the status
    // moves, which would strand the session); `Installing` would interleave a
    // second command into the same serial console; anything later is done.
    if session.status != BuildStatus::Ready {
        return Err(AppError::conflict(
            "build_not_ready",
            "this build is not ready for package changes right now",
            request_id.0,
        ));
    }

    state.builds.set_status(build_id, BuildStatus::Installing);
    state.builds.append_log(
        build_id,
        format!("{:?} {}", body.action, body.packages.join(" ")),
    );

    let _vm_response = super::packages::run_package_action(
        State(state.clone()),
        Extension(request_id),
        Path(session.vm_id.to_string()),
        Json(body),
    )
    .await
    .inspect_err(|_| {
        state
            .builds
            .set_status_from(build_id, BuildStatus::Installing, BuildStatus::Ready);
    })?;

    // Reaching this point means run_package_action successfully dispatched
    // the command to the builder VM's console, so this session now has a
    // package action to its name regardless of how the action ultimately
    // resolves — `finalize_build` gates on this flag to refuse registering a
    // template from a session that never customized anything.
    state.builds.mark_package_action_done(build_id);

    let state_for_task = state.clone();
    let vm_id = session.vm_id;
    tokio::spawn(async move {
        record_package_outcome(&state_for_task, build_id, vm_id).await;
    });

    Ok(Json(
        state.builds.get(build_id).expect("session still tracked"),
    ))
}

/// Waits for the console action `build_packages` just dispatched to reach a
/// terminal `packageUpdate`, mirrors it into the session log, and returns
/// the session to `Ready` so it can be finalized.
async fn record_package_outcome(state: &AppState, build_id: Uuid, vm_id: Uuid) {
    match wait_for_package_outcome(state, vm_id).await {
        Some(PackageUpdateStatus::Succeeded { output_tail }) => {
            state.builds.append_log(build_id, output_tail);
        }
        Some(PackageUpdateStatus::Failed {
            reason,
            output_tail,
        }) => {
            state
                .builds
                .append_log(build_id, format!("{reason}\n{output_tail}"));
        }
        // `run_action` always records a terminal status well inside
        // `PACKAGE_OUTCOME_TIMEOUT`, so this only happens if the VM record
        // itself went away (cancel/delete) — say so rather than silently
        // reporting the session ready again.
        _ => state.builds.append_log(
            build_id,
            "package action outcome was never observed on the builder VM",
        ),
    }
    // Compare-and-set: a cancel or a failure elsewhere may already have
    // moved this session on, and this task owns only the `Installing` exit.
    state
        .builds
        .set_status_from(build_id, BuildStatus::Installing, BuildStatus::Ready);
}

/// Polls `state.vms[vm_id].package_update` until it leaves `Running` or
/// [`PACKAGE_OUTCOME_TIMEOUT`] elapses — `run_package_action`'s console wait
/// runs on a detached task, so this is the only way to observe its result.
async fn wait_for_package_outcome(state: &AppState, vm_id: Uuid) -> Option<PackageUpdateStatus> {
    let deadline = tokio::time::Instant::now() + PACKAGE_OUTCOME_TIMEOUT;
    loop {
        let status = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&vm_id)
            .and_then(|vm| vm.package_update.clone());
        match status {
            Some(PackageUpdateStatus::Running) | None => {
                if tokio::time::Instant::now() >= deadline {
                    return None;
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            other => return other,
        }
    }
}

/// `POST /api/images/builds/{buildId}/finalize` — stops the builder VM,
/// pulls its rootfs disk out from under `delete_vm`'s artifact cleanup,
/// strips guest identity, registers it as a new template version, then
/// deletes the builder VM. `newAlias` in the request body determines
/// whether this is an in-place rebuild (omitted) or a derived template
/// (given) — decided here rather than at `start_build` time, since the
/// operator may only know which they want after seeing what changed.
///
/// Only the guards run inline; everything from `stop_vm` onward happens on a
/// detached task and the response carries the session's in-flight
/// `Finalizing` snapshot. Copying a multi-GB rootfs and running `e2fsck`
/// over it takes far longer than `enforce_limits`' request timeout, and
/// axum drops a timed-out handler's future — which would abandon the copied
/// disk unregistered, leave the session stuck at `Finalizing`, and strand a
/// stopped-but-undeleted builder VM. The dashboard polls
/// `GET /api/images/builds/{buildId}` for the terminal result instead.
pub async fn finalize_build(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(build_id): Path<String>,
    ValidatedJson(body): ValidatedJson<FinalizeBuildRequest>,
) -> Result<Json<BuildResponse>, AppError> {
    let parsed_build_id = parse_id(&build_id, request_id.0)?;
    let session = state
        .builds
        .get(parsed_build_id)
        .ok_or_else(|| AppError::not_found(request_id.0))?;

    // A finished session has no builder VM left to stop, copy from, or
    // delete — retrying one would fail at `stop_vm` and overwrite its real
    // terminal status (including a `Succeeded` that genuinely registered a
    // template) with a bogus `Failed`.
    if matches!(session.status, BuildStatus::Succeeded | BuildStatus::Failed) {
        return Err(AppError::conflict(
            "build_finished",
            "this build session has already finished; start a new build to make more changes",
            request_id.0,
        ));
    }

    if session.status == BuildStatus::Finalizing {
        return Err(AppError::conflict(
            "build_in_progress",
            "this build is already being saved as an image",
            request_id.0,
        ));
    }

    // `build_packages` flips `had_package_action` right after it dispatches
    // a command to the builder VM's console — before the outcome is known
    // — so a session can still have one running here even though that flag
    // already reads `true`. Finalizing mid-install would race
    // `record_package_outcome`'s poll for the very disk this handler is
    // about to copy out from under it, so refuse outright rather than risk
    // grabbing a half-written rootfs.
    if session.status == BuildStatus::Installing || package_action_running(&state, session.vm_id) {
        return Err(AppError::conflict(
            "build_in_progress",
            "a package action is still running on this build; wait for it to finish before finalizing",
            request_id.0,
        ));
    }

    if !session.had_package_action {
        return Err(AppError::conflict(
            "no_changes",
            "install, remove, or update at least one package before saving this build as an image",
            request_id.0,
        ));
    }

    let target_alias = body
        .new_alias
        .unwrap_or_else(|| session.source_alias.clone());
    if target_alias != session.source_alias && TemplateRegistry::known_spec(&target_alias).is_some()
    {
        return Err(AppError::conflict(
            "alias_reserved",
            "that alias name is reserved for a built-in template",
            request_id.0,
        ));
    }

    state
        .builds
        .set_status(parsed_build_id, BuildStatus::Finalizing);

    let state_for_task = state.clone();
    let session_for_task = session.clone();
    tokio::spawn(async move {
        run_finalize(
            &state_for_task,
            parsed_build_id,
            session_for_task,
            target_alias,
            request_id,
        )
        .await;
    });

    Ok(Json(
        state
            .builds
            .get(parsed_build_id)
            .expect("session still tracked"),
    ))
}

/// Whether the builder VM still has a package action in flight. Independent
/// second opinion to the session's own `Installing` status: the two live in
/// different places (`BuildTracker` vs. `VmRecord.package_update`) and can
/// drift if a package action is started against the VM directly.
fn package_action_running(state: &AppState, vm_id: Uuid) -> bool {
    state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&vm_id)
        .is_some_and(|vm| matches!(vm.package_update, Some(PackageUpdateStatus::Running)))
}

/// The whole of `finalize_build` past its guards, run detached: stop the
/// builder VM, snapshot and register its disk, then delete the VM. Reports
/// exclusively through the build session, which is what the caller polls.
async fn run_finalize(
    state: &AppState,
    build_id: Uuid,
    session: BuildResponse,
    target_alias: String,
    request_id: RequestId,
) {
    if super::vms::stop_vm(
        State(state.clone()),
        Extension(request_id),
        Path(session.vm_id.to_string()),
    )
    .await
    .is_err()
    {
        // Nothing has been registered yet at this point, so this really is
        // an end-to-end failure — land the session on a terminal state
        // rather than leaving it stuck at `Finalizing` forever. A retry
        // would otherwise hit `stop_vm`'s state-transition guard every time
        // (it only allows Running -> Stopping, and a first attempt may have
        // already moved the VM partway), with no in-band way out.
        state.builds.finish_err(
            build_id,
            format!(
                "failed to stop builder VM {} before finalizing",
                session.vm_id
            ),
        );
        return;
    }

    if let Err(reason) = finalize_and_register(state, &session, &target_alias, request_id.0).await {
        state.builds.finish_err(build_id, &reason);
        // Best-effort: the disk/register failure above is what this session
        // reports, so a second failure here (delete_vm) is swallowed rather
        // than masking it. If delete_vm itself fails, the builder VM record
        // survives — `VmPurpose::Builder` hides it from `list_vms`, so it
        // isn't operator-visible for cleanup; the session's own log (via
        // `finish_err`) is the only trace, an accepted gap.
        let _ = super::vms::delete_vm(
            State(state.clone()),
            Extension(request_id),
            Path(session.vm_id.to_string()),
        )
        .await;
        return;
    }

    if super::vms::delete_vm(
        State(state.clone()),
        Extension(request_id),
        Path(session.vm_id.to_string()),
    )
    .await
    .is_err()
    {
        // The template IS registered and usable by this point — the actual
        // goal of this request already succeeded — so the session still
        // reports success, not a build failure. VM cleanup here is genuinely
        // best-effort; log the vm_id since `VmPurpose::Builder` hides the VM
        // from `list_vms`, so this log line is the operator's only way to
        // find it for manual removal.
        state.builds.append_log(
            build_id,
            format!(
                "warning: builder VM {} could not be deleted after finalize succeeded; remove it manually",
                session.vm_id
            ),
        );
    }

    state.builds.finish_ok(build_id, &target_alias);
}

/// Copies the builder VM's current-generation rootfs disk out, strips guest
/// identity via `rootfs::finalize_template_disk`, and registers the result
/// as a new template version under `target_alias`. Split out from
/// `finalize_build` so this logic gets direct unit coverage without needing
/// a real Firecracker process to produce a genuinely `Running` builder VM
/// for `stop_vm` to act on first — the test fixture's Firecracker binary
/// never actually boots one.
pub(crate) async fn finalize_and_register(
    state: &AppState,
    session: &BuildResponse,
    target_alias: &str,
    request_id: Uuid,
) -> Result<(), String> {
    let vm_record = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&session.vm_id)
        .cloned()
        .ok_or_else(|| "builder VM record vanished before finalize".to_owned())?;

    let source_version = state
        .templates
        .resolve_alias(&session.source_alias)
        .ok_or_else(|| format!("source alias {} no longer resolves", session.source_alias))?;

    let Some(disk_generation) = vm_record.disk_generation else {
        return Err("builder VM has no disk generation to finalize".to_owned());
    };
    let artifact_paths = crate::artifacts::VmArtifactPaths::for_vm(
        &state.vms_dir_for(&vm_record.storage_root),
        session.vm_id,
    );
    let source_disk = artifact_paths.rootfs(disk_generation);

    // A flat top-level filename (rather than nesting under a `rootfs/`
    // subdirectory the way the built-in `default_specs` do) means this
    // never has to `create_dir_all` a path component that might already
    // exist as something else — image_root_path() itself is already a
    // verified, existing directory.
    let version_tag = format!("{target_alias}-{}", request_id.simple());
    let dest_relative = std::path::PathBuf::from(format!("{version_tag}.ext4"));
    let dest_path = state.templates.image_root_path().join(&dest_relative);

    let finalize_result = tokio::task::spawn_blocking({
        let source_disk = source_disk.clone();
        let dest_path = dest_path.clone();
        move || -> Result<(), String> {
            std::fs::copy(&source_disk, &dest_path).map_err(|error| {
                format!(
                    "copy {} -> {}: {error}",
                    source_disk.display(),
                    dest_path.display()
                )
            })?;
            crate::rootfs::finalize_template_disk(&dest_path)
                .map_err(|error| format!("finalize {}: {error}", dest_path.display()))
        }
    })
    .await
    .map_err(|error| format!("finalize task panicked: {error}"))?;

    if let Err(reason) = finalize_result {
        let _ = std::fs::remove_file(&dest_path);
        return Err(reason);
    }

    // Captured before registering: `register_spec` overwrites the alias pin,
    // after which there is no way left to find what it used to point at.
    let superseded = state.templates.resolve_alias(target_alias);

    let spec = crate::templates::TemplateSpec {
        alias: target_alias.to_owned(),
        version: version_tag.clone(),
        kernel: source_version.kernel.relative_path().to_path_buf(),
        initrd: source_version
            .initrd
            .as_ref()
            .map(|artifact| artifact.relative_path().to_path_buf()),
        rootfs: dest_relative,
        boot_args: source_version.boot_args.clone(),
    };
    let templates = state.templates.clone();
    let register_result = tokio::task::spawn_blocking(move || templates.register_spec(spec))
        .await
        .map_err(|error| format!("register task panicked: {error}"))?;

    if let Err(error) = register_result {
        let _ = std::fs::remove_file(&dest_path);
        return Err(error.to_string());
    }

    if let Some(superseded) = superseded
        && superseded.version != version_tag
    {
        prune_superseded_build(state, target_alias, &superseded);
    }

    Ok(())
}

/// Reclaims the rootfs an in-place rebuild just replaced.
///
/// Each finalize mints a fresh version tag and a fresh `.ext4`, and
/// `register_spec` keeps the old version addressable so VMs pinned to it
/// still boot — without this, every rebuild of the same alias would leave
/// one more unreachable multi-GB file behind. Deliberately narrow: only a
/// rootfs a previous *web build* produced is ever removed (those sit flat at
/// the image root, where nothing else writes; built-in and downloaded
/// artifacts always live under `kernel/` or `rootfs/`), and only when no VM
/// record still pins that version.
fn prune_superseded_build(
    state: &AppState,
    alias: &str,
    superseded: &crate::templates::TemplateVersion,
) {
    let is_build_artifact = superseded.rootfs.relative_path().components().count() == 1;
    if !is_build_artifact {
        return;
    }

    let still_pinned = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .values()
        .any(|vm| vm.template == alias && vm.template_version == superseded.version);
    if still_pinned {
        return;
    }

    let Some(orphans) = state
        .templates
        .prune_version(&superseded.name, &superseded.version)
    else {
        return;
    };
    for path in orphans {
        if let Err(error) = std::fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                alias,
                path = %path.display(),
                error = %error,
                "could not remove the rootfs superseded by this build"
            );
        }
    }
}

/// `DELETE /api/images/builds/{buildId}` — tears down the builder VM
/// without registering anything. Safe to call at any point in a session's
/// lifecycle (booting, ready, mid-install) since it goes through the same
/// `delete_vm` path a user-initiated VM delete would.
pub async fn cancel_build(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(build_id): Path<String>,
) -> Result<StatusCode, AppError> {
    let parsed_build_id = parse_id(&build_id, request_id.0)?;
    let session = state
        .builds
        .get(parsed_build_id)
        .ok_or_else(|| AppError::not_found(request_id.0))?;

    // A VM still `Starting`/`Running` needs `stop_vm` before `delete_vm`
    // accepts it — mirror the frontend's own stop-then-delete sequence for
    // a running instance (`Images.tsx`'s `removeVmsUsingImage`).
    let can_delete_now = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&session.vm_id)
        .map(|vm| vm.state.can_delete())
        .unwrap_or(true);
    if !can_delete_now {
        let _ = super::vms::stop_vm(
            State(state.clone()),
            Extension(request_id),
            Path(session.vm_id.to_string()),
        )
        .await;
    }

    let _ = super::vms::delete_vm(
        State(state.clone()),
        Extension(request_id),
        Path(session.vm_id.to_string()),
    )
    .await;

    state.builds.remove(parsed_build_id);
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::response::IntoResponse;
    use firecrab_api_types::BuildStatus;
    use tempfile::tempdir;
    use tokio::sync::watch;

    use super::*;
    use crate::console::ConsoleBroker;
    use crate::firecracker::VmProcess;
    use crate::handlers::vms::test_support::{record, seed_vm, test_state};
    use crate::model::{VmRecord, VmState};

    /// Registers a fake console+process for `id`, the same way
    /// `handlers::packages`'s own tests do — `run_package_action` requires a
    /// live `VmProcess` to write the sentinel-wait command to, and the test
    /// fixture never actually boots Firecracker.
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

    /// See `handlers::packages`'s identical helper: `run_package_action`
    /// returns as soon as it has *spawned* the console wait, which only then
    /// subscribes — output pushed before that subscription would be lost.
    async fn wait_for_console_subscriber(console: &ConsoleBroker) {
        for _ in 0..200 {
            if console.subscriber_count() > 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("run_action never subscribed to the console");
    }

    /// Seeds a builder VM plus its tracked session directly, instead of
    /// going through `start_build`.
    ///
    /// `start_build` leaves two detached tasks running against the same VM —
    /// `finish_start` (which fails against the fixture's non-existent
    /// Firecracker binary and lands the record on `Error`) and
    /// `watch_builder_boot` (which turns that into a `Failed` session) — so
    /// any test that wants to drive the VM/session states itself races them.
    /// Tests that specifically exercise `start_build`'s own wiring still call
    /// it; everything downstream of `Ready` starts from here.
    fn seeded_session(state: &AppState, vm_state: VmState) -> (VmRecord, Uuid) {
        let vm = VmRecord {
            state: vm_state,
            ..record("builder-vm", Uuid::new_v4())
        };
        seed_vm(state, &vm);
        let build_id = state.builds.begin("ubuntu-rootfs-26.04", vm.id);
        (vm, build_id)
    }

    /// Waits for a detached task to move a session to `expected`. Every long
    /// build operation now reports through the tracker rather than its HTTP
    /// response, so this is what stands in for awaiting the handler itself.
    async fn wait_for_build_status(
        state: &AppState,
        build_id: Uuid,
        expected: BuildStatus,
    ) -> BuildResponse {
        for _ in 0..400 {
            if let Some(session) = state.builds.get(build_id)
                && session.status == expected
            {
                return session;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!(
            "build session never reached {expected:?} (last: {:?})",
            state.builds.get(build_id).map(|session| session.status)
        );
    }

    /// Writes a real (tiny) ext4 file where `finalize_and_register` expects
    /// the builder VM's current-generation disk, since this fixture never
    /// boots Firecracker and so never runs the real disk-prep step.
    fn seed_builder_disk(state: &AppState, vm_id: Uuid) -> Uuid {
        let generation = Uuid::new_v4();
        let artifact_paths =
            crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for("default"), vm_id);
        artifact_paths.ensure_directories().unwrap();
        let status = std::process::Command::new("mkfs.ext4")
            .args(["-q", "-F"])
            .arg(artifact_paths.rootfs(generation))
            .arg("8M")
            .status()
            .unwrap();
        assert!(status.success(), "mkfs.ext4 failed");
        let mut vms = state.vms.lock().unwrap();
        vms.get_mut(&vm_id).unwrap().disk_generation = Some(generation);
        generation
    }

    #[tokio::test]
    async fn start_build_rejects_an_unknown_source_alias() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let error = start_build(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path("no-such-alias".to_owned()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn start_build_boots_a_builder_vm_hidden_from_list_vms() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        // test_state's default fixture template alias is "ubuntu-rootfs-26.04"
        // (see handlers::vms::test_support::test_state) — reuse it as the
        // build source instead of alpine/ubuntu/rocky, which aren't registered
        // in this lightweight fixture.
        crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);

        let (status, Json(build)) = start_build(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path("ubuntu-rootfs-26.04".to_owned()),
        )
        .await
        .unwrap();

        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(build.status, BuildStatus::Booting);

        let Json(listed) = crate::handlers::vms::list_vms(State(state)).await;
        assert!(!listed.iter().any(|vm| vm.id == build.vm_id));
    }

    /// Nothing else in the system observes a builder VM reaching `Running`,
    /// so without this watcher a session sits at `Booting` forever and the
    /// dashboard's build modal — which gates every control on `ready` —
    /// stays permanently inert.
    #[tokio::test]
    async fn watch_builder_boot_marks_the_session_ready_once_the_vm_runs() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let (vm, build_id) = seeded_session(&state, VmState::Starting);

        let state_for_watch = state.clone();
        let watcher = tokio::spawn(async move {
            watch_builder_boot(&state_for_watch, build_id, vm.id).await;
        });

        // Still `Starting`: the watcher must keep waiting, not guess.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            state.builds.get(build_id).unwrap().status,
            BuildStatus::Booting
        );

        state.vms.lock().unwrap().get_mut(&vm.id).unwrap().state = VmState::Running;

        let session = wait_for_build_status(&state, build_id, BuildStatus::Ready).await;
        assert!(session.log.contains("builder VM is running"));
        watcher.await.unwrap();
    }

    #[tokio::test]
    async fn watch_builder_boot_fails_the_session_when_the_vm_lands_on_error() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let (vm, build_id) = seeded_session(&state, VmState::Error);

        watch_builder_boot(&state, build_id, vm.id).await;

        let session = state.builds.get(build_id).unwrap();
        assert_eq!(session.status, BuildStatus::Failed);
        assert!(session.log.contains("failed to boot"));
        assert!(session.ended_at_ms.is_some());
    }

    /// End-to-end proof the watcher is actually spawned by `start_build`:
    /// the fixture's Firecracker binary doesn't exist, so the builder VM's
    /// real start pipeline fails and the session must follow it to `Failed`
    /// instead of sitting at `Booting`.
    #[tokio::test]
    async fn start_build_fails_the_session_when_the_builder_vm_never_boots() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);

        let (_status, Json(build)) = start_build(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path("ubuntu-rootfs-26.04".to_owned()),
        )
        .await
        .unwrap();
        assert_eq!(build.status, BuildStatus::Booting);

        wait_for_build_status(&state, build.build_id, BuildStatus::Failed).await;
    }

    #[tokio::test]
    async fn start_build_reports_unavailable_when_no_internet_micro_network_exists() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        // Deliberately no `seed_internet_micro_network` call here — this
        // exercises `builder_micro_network_id`'s "no internet-enabled
        // MicroNetwork exists" branch, which every other test in this
        // module sidesteps by seeding one.

        let error = start_build(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path("ubuntu-rootfs-26.04".to_owned()),
        )
        .await
        .unwrap_err();

        assert_eq!(
            error.into_response().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn build_packages_requires_the_builder_vm_to_be_running() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        // Session says `Ready`, VM says otherwise — `run_package_action`'s own
        // state guard must surface that as a normal conflict, not a panic.
        let (_vm, build_id) = seeded_session(&state, VmState::Stopped);
        state.builds.set_status(build_id, BuildStatus::Ready);

        let error = build_packages(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(build_id.to_string()),
            Json(firecrab_api_types::PackageAction {
                action: firecrab_api_types::PackageActionKind::Install,
                packages: vec!["curl".to_owned()],
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    }

    /// A `Booting` session has no console to write to yet, and moving its
    /// status is what tells `watch_builder_boot` to stop watching — so a
    /// package action before `Ready` would strand the session there.
    #[tokio::test]
    async fn build_packages_is_refused_before_the_session_is_ready() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let (_vm, build_id) = seeded_session(&state, VmState::Starting);

        let error = build_packages(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(build_id.to_string()),
            Json(firecrab_api_types::PackageAction {
                action: firecrab_api_types::PackageActionKind::Install,
                packages: vec!["curl".to_owned()],
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
        assert_eq!(
            state.builds.get(build_id).unwrap().status,
            BuildStatus::Booting,
            "a refused action must leave the session watchable"
        );
    }

    #[tokio::test]
    async fn build_packages_rejects_an_unknown_build_id() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let error = build_packages(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(Uuid::new_v4().to_string()),
            Json(firecrab_api_types::PackageAction {
                action: firecrab_api_types::PackageActionKind::Update,
                packages: Vec::new(),
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }

    /// `build_packages` must return as soon as the command is dispatched:
    /// a real `apt-get install` runs for minutes, far past the 10s request
    /// timeout in `server.rs`, and axum drops a timed-out handler's future —
    /// which would strand the session in `Installing` forever and
    /// permanently block finalize. The outcome arrives via a later poll.
    #[tokio::test]
    async fn build_packages_returns_installing_before_the_action_finishes() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = VmRecord {
            state: VmState::Running,
            ..record("builder-vm", Uuid::new_v4())
        };
        seed_vm(&state, &vm);
        let console = register_fake_process(&state, vm.id);
        let build_id = state.builds.begin("ubuntu-rootfs-26.04", vm.id);
        state.builds.set_status(build_id, BuildStatus::Ready);

        // No sentinel has been pushed yet — a handler that waited for the
        // outcome inline could not possibly return here.
        let Json(build) = tokio::time::timeout(
            Duration::from_secs(5),
            build_packages(
                State(state.clone()),
                Extension(RequestId(Uuid::new_v4())),
                Path(build_id.to_string()),
                Json(firecrab_api_types::PackageAction {
                    action: firecrab_api_types::PackageActionKind::Update,
                    packages: Vec::new(),
                }),
            ),
        )
        .await
        .expect("build_packages must not block on the package action")
        .expect("build_packages returned an error");

        assert_eq!(build.status, BuildStatus::Installing);
        assert!(build.had_package_action);

        wait_for_console_subscriber(&console).await;
        console.push_output(b"FIRECRAB_PKG_UPDATE_DONE:0\n");

        let settled = wait_for_build_status(&state, build_id, BuildStatus::Ready).await;
        assert!(settled.log.contains("FIRECRAB_PKG_UPDATE_DONE:0"));
    }

    #[tokio::test]
    async fn build_packages_records_a_failed_outcome_and_still_marks_the_action_done() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = VmRecord {
            state: VmState::Running,
            template: "alpine-3.24".to_owned(),
            ..record("builder-vm", Uuid::new_v4())
        };
        seed_vm(&state, &vm);
        let console = register_fake_process(&state, vm.id);
        let build_id = state.builds.begin("alpine-3.24", vm.id);
        state.builds.set_status(build_id, BuildStatus::Ready);

        let _accepted = build_packages(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(build_id.to_string()),
            Json(firecrab_api_types::PackageAction {
                action: firecrab_api_types::PackageActionKind::Install,
                packages: vec!["curl".to_owned()],
            }),
        )
        .await
        .expect("build_packages returned an error");

        wait_for_console_subscriber(&console).await;
        console.push_output(b"FIRECRAB_PKG_UPDATE_DONE:1\n");

        // Even though the package action itself failed, the session must
        // come back to a normal, resumable state rather than stay stuck in
        // `Installing` — and the session did have a package action run
        // against it, so `had_package_action` still flips to true.
        let build = wait_for_build_status(&state, build_id, BuildStatus::Ready).await;
        assert!(build.had_package_action);
        assert!(build.log.contains("exited with code 1"));
    }

    /// The session's `Installing` window must actually cover the guest's
    /// package run, since that is the only thing stopping `finalize_build`
    /// from copying a rootfs that is still being written to.
    #[tokio::test]
    async fn finalize_build_is_refused_while_a_dispatched_package_action_is_still_running() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = VmRecord {
            state: VmState::Running,
            ..record("builder-vm", Uuid::new_v4())
        };
        seed_vm(&state, &vm);
        let console = register_fake_process(&state, vm.id);
        let build_id = state.builds.begin("ubuntu-rootfs-26.04", vm.id);
        state.builds.set_status(build_id, BuildStatus::Ready);

        let _accepted = build_packages(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(build_id.to_string()),
            Json(firecrab_api_types::PackageAction {
                action: firecrab_api_types::PackageActionKind::Update,
                packages: Vec::new(),
            }),
        )
        .await
        .unwrap();
        wait_for_console_subscriber(&console).await;

        let error = finalize_build(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(build_id.to_string()),
            ValidatedJson(FinalizeBuildRequest { new_alias: None }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);

        console.push_output(b"FIRECRAB_PKG_UPDATE_DONE:0\n");
        wait_for_build_status(&state, build_id, BuildStatus::Ready).await;
    }

    /// Second, independent guard: `BuildStatus` and `VmRecord.package_update`
    /// live in different places and can drift (a package action started
    /// against the builder VM directly never touches the session), so a VM
    /// still reporting `Running` must block finalize on its own.
    #[tokio::test]
    async fn finalize_build_is_refused_while_the_vm_reports_a_running_package_update() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let (vm, build_id) = seeded_session(&state, VmState::Running);
        state.builds.mark_package_action_done(build_id);
        state.builds.set_status(build_id, BuildStatus::Ready);
        state
            .vms
            .lock()
            .unwrap()
            .get_mut(&vm.id)
            .unwrap()
            .package_update = Some(PackageUpdateStatus::Running);

        let error = finalize_build(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(build_id.to_string()),
            ValidatedJson(FinalizeBuildRequest { new_alias: None }),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn finalize_build_rejects_a_session_with_no_successful_package_action() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let (_vm, build_id) = seeded_session(&state, VmState::Running);
        state.builds.set_status(build_id, BuildStatus::Ready);

        let error = finalize_build(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(build_id.to_string()),
            ValidatedJson(FinalizeBuildRequest { new_alias: None }),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    }

    /// Re-finalizing a finished session used to run `stop_vm` against an
    /// already-deleted VM, overwriting a real `Succeeded` (template
    /// registered and all) with a bogus `Failed`.
    #[tokio::test]
    async fn finalize_build_refuses_a_session_that_already_finished() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let (_vm, build_id) = seeded_session(&state, VmState::Running);
        state.builds.mark_package_action_done(build_id);
        state.builds.finish_ok(build_id, "my-nginx-base");

        let error = finalize_build(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(build_id.to_string()),
            ValidatedJson(FinalizeBuildRequest { new_alias: None }),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
        let session = state.builds.get(build_id).unwrap();
        assert_eq!(session.status, BuildStatus::Succeeded);
        assert_eq!(session.target_alias.as_deref(), Some("my-nginx-base"));
    }

    #[tokio::test]
    async fn finalize_build_rejects_an_unknown_build_id() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let error = finalize_build(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(Uuid::new_v4().to_string()),
            ValidatedJson(FinalizeBuildRequest { new_alias: None }),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn finalize_build_rejects_a_reserved_alias_name() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let (_vm, build_id) = seeded_session(&state, VmState::Running);
        state.builds.mark_package_action_done(build_id);
        state.builds.set_status(build_id, BuildStatus::Ready);

        // "alpine-3.24" is one of TemplateRegistry::known_specs's built-in
        // aliases — reserved even though this registry never actually
        // installed it, so a derived build can't shadow it.
        let error = finalize_build(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(build_id.to_string()),
            ValidatedJson(FinalizeBuildRequest {
                new_alias: Some("alpine-3.24".to_owned()),
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    }

    /// If `stop_vm` itself fails, nothing has been registered yet — this is
    /// a genuine end-to-end failure, so the session must land on
    /// `BuildStatus::Failed` rather than sticking at `Finalizing` forever
    /// (a retry would otherwise hit `stop_vm`'s Running -> Stopping guard
    /// every time, with no in-band way out).
    #[tokio::test]
    async fn finalize_build_marks_the_session_failed_when_stop_vm_fails() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        // Left at the default `Created` state (never started) — `stop_vm`
        // only allows Running -> Stopping, so this reproduces the same
        // guard failure a half-transitioned VM from a prior finalize
        // attempt would hit.
        let (_vm, build_id) = seeded_session(&state, VmState::Created);
        state.builds.mark_package_action_done(build_id);
        state.builds.set_status(build_id, BuildStatus::Ready);

        let Json(accepted) = finalize_build(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(build_id.to_string()),
            ValidatedJson(FinalizeBuildRequest { new_alias: None }),
        )
        .await
        .unwrap();
        assert_eq!(accepted.status, BuildStatus::Finalizing);

        let session = wait_for_build_status(&state, build_id, BuildStatus::Failed).await;
        assert!(session.log.contains("failed to stop builder VM"));
    }

    /// `build_packages` flips `had_package_action` as soon as it
    /// successfully *dispatches* a command to the builder VM's console —
    /// before any result is observed — so a session can be mid-install
    /// (`BuildStatus::Installing`) with the flag already `true`.
    /// `finalize_build` must refuse this case even though the
    /// `had_package_action` check alone would let it through, since the
    /// disk it's about to copy could still be mid-write on the builder VM.
    #[tokio::test]
    async fn finalize_build_rejects_a_build_still_installing_packages() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let (_vm, build_id) = seeded_session(&state, VmState::Running);
        state.builds.mark_package_action_done(build_id);
        state.builds.set_status(build_id, BuildStatus::Installing);

        let error = finalize_build(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(build_id.to_string()),
            ValidatedJson(FinalizeBuildRequest { new_alias: None }),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    }

    /// Full success-path coverage for the disk-copy/strip/register logic
    /// lives here against `finalize_and_register` directly rather than
    /// through `finalize_build`: `finalize_build` first calls
    /// `handlers::vms::stop_vm`, which requires the VM to actually be
    /// `Running` (`VmState::can_transition`) — something this test
    /// fixture's Firecracker binary can never produce, since it never
    /// really boots a process. Testing the extracted helper directly gives
    /// this logic real coverage without fighting that constraint.
    #[tokio::test]
    async fn finalize_and_register_registers_a_new_template_version_when_disk_is_ready() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        // Simulate a builder VM whose disk-prep step already ran (as it
        // would have, early in a real `start_vm`, well before `Running`) —
        // this fixture never actually boots Firecracker, so seed a real
        // ext4 file at the disk generation path `finalize_and_register`
        // expects instead.
        let (vm, build_id) = seeded_session(&state, VmState::Running);
        seed_builder_disk(&state, vm.id);

        let session = state.builds.get(build_id).unwrap();
        finalize_and_register(&state, &session, "my-nginx-base", Uuid::new_v4())
            .await
            .unwrap();

        let registered = state.templates.resolve_alias("my-nginx-base");
        assert!(registered.is_some());
        assert_eq!(registered.unwrap().name, "my-nginx-base");
    }

    /// Every finalize mints a new version tag and a new flat `.ext4` at the
    /// image root, and `register_spec` keeps the superseded version
    /// addressable — so without pruning, each in-place rebuild would leave
    /// another unreachable multi-GB rootfs behind forever.
    #[tokio::test]
    async fn finalize_and_register_removes_the_rootfs_a_rebuild_supersedes() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let (vm, build_id) = seeded_session(&state, VmState::Running);
        let session = state.builds.get(build_id).unwrap();

        seed_builder_disk(&state, vm.id);
        finalize_and_register(&state, &session, "my-nginx-base", Uuid::new_v4())
            .await
            .unwrap();
        let first = state.templates.resolve_alias("my-nginx-base").unwrap();
        let first_rootfs = state.templates.artifact_path(&first.rootfs);
        assert!(first_rootfs.is_file());

        // Rebuild the same alias in place from the same session.
        seed_builder_disk(&state, vm.id);
        finalize_and_register(&state, &session, "my-nginx-base", Uuid::new_v4())
            .await
            .unwrap();

        let second = state.templates.resolve_alias("my-nginx-base").unwrap();
        assert_ne!(second.version, first.version);
        assert!(state.templates.artifact_path(&second.rootfs).is_file());
        assert!(
            !first_rootfs.is_file(),
            "the superseded build's rootfs must not linger at {}",
            first_rootfs.display()
        );
        assert!(
            state
                .templates
                .resolve_version("my-nginx-base", &first.version)
                .is_none()
        );
    }

    /// The prune above must never touch a rootfs a VM still boots from, nor
    /// a built-in/downloaded artifact (which live under `kernel/`/`rootfs/`,
    /// unlike a build's own flat file).
    #[tokio::test]
    async fn finalize_and_register_keeps_a_superseded_rootfs_a_vm_still_pins() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let (vm, build_id) = seeded_session(&state, VmState::Running);
        let session = state.builds.get(build_id).unwrap();

        seed_builder_disk(&state, vm.id);
        finalize_and_register(&state, &session, "my-nginx-base", Uuid::new_v4())
            .await
            .unwrap();
        let first = state.templates.resolve_alias("my-nginx-base").unwrap();
        let first_rootfs = state.templates.artifact_path(&first.rootfs);

        // A VM created from that first build version.
        let pinned = VmRecord {
            template: "my-nginx-base".to_owned(),
            template_version: first.version.clone(),
            ..record("pinned-vm", Uuid::new_v4())
        };
        seed_vm(&state, &pinned);

        seed_builder_disk(&state, vm.id);
        finalize_and_register(&state, &session, "my-nginx-base", Uuid::new_v4())
            .await
            .unwrap();

        assert!(
            first_rootfs.is_file(),
            "a rootfs a VM still pins must survive the rebuild"
        );
        assert!(
            state
                .templates
                .resolve_version("my-nginx-base", &first.version)
                .is_some()
        );
    }

    /// An in-place rebuild of a *built-in* alias must leave the distro's own
    /// rootfs alone — it lives under `rootfs/`, is reusable for a reinstall,
    /// and nothing about superseding an alias pin makes it deletable.
    #[tokio::test]
    async fn finalize_and_register_never_deletes_a_built_in_rootfs() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let (vm, build_id) = seeded_session(&state, VmState::Running);
        let session = state.builds.get(build_id).unwrap();
        let source = state
            .templates
            .resolve_alias("ubuntu-rootfs-26.04")
            .unwrap();
        let source_rootfs = state.templates.artifact_path(&source.rootfs);

        seed_builder_disk(&state, vm.id);
        finalize_and_register(&state, &session, "ubuntu-rootfs-26.04", Uuid::new_v4())
            .await
            .unwrap();

        assert!(
            source_rootfs.is_file(),
            "the source template's own rootfs must survive an in-place rebuild"
        );
    }

    /// End-to-end coverage of `finalize_build`'s own orchestration (guard
    /// checks already passed, `stop_vm`, `finalize_and_register`,
    /// `delete_vm`, `finish_ok`) — not just the extracted helper. `stop_vm`
    /// only requires `VmState::Running` in the in-memory record to proceed
    /// (its own SIGTERM step is skipped when `state.processes` has no entry
    /// for the VM, and `teardown_vm_network` is best-effort against the
    /// fixture's always-ok network helper), so setting `Running` directly —
    /// the same technique `build_packages`'s own Task 8 tests already use —
    /// is enough to drive this without a real Firecracker process.
    #[tokio::test]
    async fn finalize_build_stops_the_builder_vm_registers_a_template_and_deletes_it() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let (vm, build_id) = seeded_session(&state, VmState::Running);
        seed_builder_disk(&state, vm.id);
        state.builds.mark_package_action_done(build_id);
        state.builds.set_status(build_id, BuildStatus::Ready);

        // The response is the in-flight snapshot; the terminal state arrives
        // on the session, which is what the dashboard polls.
        let Json(accepted) = finalize_build(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(build_id.to_string()),
            ValidatedJson(FinalizeBuildRequest {
                new_alias: Some("my-nginx-base".to_owned()),
            }),
        )
        .await
        .unwrap();
        assert_eq!(accepted.status, BuildStatus::Finalizing);

        let finalized = wait_for_build_status(&state, build_id, BuildStatus::Succeeded).await;
        assert_eq!(finalized.target_alias.as_deref(), Some("my-nginx-base"));
        assert!(state.templates.resolve_alias("my-nginx-base").is_some());
        // delete_vm ran: the builder VM record is gone.
        assert!(state.vms.lock().unwrap().get(&vm.id).is_none());
    }

    /// By the time the *final* `delete_vm` call runs, `finalize_and_register`
    /// has already succeeded — the new template is registered and usable —
    /// so a failure here must not be reported as a build failure. The
    /// session should still reach `Succeeded` with `target_alias` set; VM
    /// cleanup is best-effort, surfaced only as a log line (since
    /// `VmPurpose::Builder` hides the VM from `list_vms`, that log line is
    /// the operator's only way to find its `vm_id` for manual removal).
    ///
    /// Built directly via `record()`/`seed_vm()` rather than `start_build`:
    /// `start_build` spawns a detached `finish_start` task that itself
    /// touches this same VM's artifact directory (its own disk-prep step),
    /// racing the permission change below and non-deterministically
    /// resetting it back before `delete_vm` ever runs.
    #[tokio::test]
    async fn finalize_build_still_succeeds_when_the_final_delete_vm_fails() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let (vm, build_id) = seeded_session(&state, VmState::Running);
        state.builds.mark_package_action_done(build_id);
        state.builds.set_status(build_id, BuildStatus::Ready);
        seed_builder_disk(&state, vm.id);
        let artifact_paths =
            crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for("default"), vm.id);

        // Same trick as `handlers::vms::tests::delete_restores_the_record_when_artifact_removal_fails`,
        // adapted: `finalize_and_register` only ever reads from
        // `artifact_paths.disks` (never touches the VM's top-level
        // directory), so stripping write permission from the top-level
        // directory *after* the disk is staged breaks only the later
        // `remove_dir_all` inside `delete_vm` — it needs write on this
        // directory to unlink its `d/`/`r/` subdirectory entries, but not
        // to read the disk file already inside `d/`.
        let mut permissions = std::fs::metadata(&artifact_paths.dir)
            .unwrap()
            .permissions();
        permissions.set_mode(0o500);
        std::fs::set_permissions(&artifact_paths.dir, permissions).unwrap();

        let _accepted = finalize_build(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(build_id.to_string()),
            ValidatedJson(FinalizeBuildRequest {
                new_alias: Some("my-nginx-base".to_owned()),
            }),
        )
        .await
        .expect("finalize_build must accept the request");

        let finalized = wait_for_build_status(&state, build_id, BuildStatus::Succeeded).await;

        // Restore permissions so the tempdir can clean itself up.
        let mut permissions = std::fs::metadata(&artifact_paths.dir)
            .unwrap()
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&artifact_paths.dir, permissions).unwrap();

        assert_eq!(finalized.target_alias.as_deref(), Some("my-nginx-base"));
        assert!(state.templates.resolve_alias("my-nginx-base").is_some());
        assert!(finalized.log.contains("could not be deleted"));
        assert!(finalized.log.contains(&vm.id.to_string()));
        // delete_vm's failure path re-inserts the record rather than
        // silently dropping it.
        assert!(state.vms.lock().unwrap().get(&vm.id).is_some());
    }

    /// When `finalize_and_register` fails (here: no disk generation was
    /// ever recorded, so there's nothing to copy), `finalize_build` must
    /// still record the failure and clean up the builder VM rather than
    /// leaving it orphaned and hidden from `list_vms` forever.
    #[tokio::test]
    async fn finalize_build_cleans_up_the_builder_vm_when_the_disk_copy_fails() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        // Running, but deliberately no disk_generation — the disk-prep step
        // that would normally set it never ran in this fixture.
        let (vm, build_id) = seeded_session(&state, VmState::Running);
        state.builds.mark_package_action_done(build_id);
        state.builds.set_status(build_id, BuildStatus::Ready);

        let _accepted = finalize_build(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(build_id.to_string()),
            ValidatedJson(FinalizeBuildRequest { new_alias: None }),
        )
        .await
        .expect("finalize_build must accept the request");

        let session = wait_for_build_status(&state, build_id, BuildStatus::Failed).await;
        assert!(session.log.contains("no disk generation"));
        // delete_vm still ran despite the finalize failure.
        assert!(state.vms.lock().unwrap().get(&vm.id).is_none());
    }

    #[tokio::test]
    async fn cancel_build_deletes_the_builder_vm_and_drops_the_session() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);
        let (_status, Json(build)) = start_build(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path("ubuntu-rootfs-26.04".to_owned()),
        )
        .await
        .unwrap();

        let status = cancel_build(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(build.build_id.to_string()),
        )
        .await
        .unwrap();

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(state.builds.get(build.build_id).is_none());
    }

    #[tokio::test]
    async fn cancel_build_rejects_an_unknown_build_id() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let error = cancel_build(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(Uuid::new_v4().to_string()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }
}

//! Web-triggered image builds: boot a "builder" VM off an installed
//! template, let the dashboard install/remove packages on its console
//! (`handlers::packages::run_package_action`, reused as-is), then snapshot
//! the resulting disk as a new template version (`finalize`, Task 9).
//!
//! A build session's VM is a completely ordinary VM — same create/start/
//! console/delete code path as any dashboard-created instance — tagged
//! `VmPurpose::Builder` so `list_vms` hides it. This avoids reimplementing
//! any part of VM lifecycle, network setup, or console handling for builds.

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use firecrab_api_types::{
    BuildResponse, BuildStatus, CreateVmRequest, EgressPolicy, FinalizeBuildRequest,
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::model::VmPurpose;
use crate::server::RequestId;
use crate::state::AppState;
use crate::templates::TemplateRegistry;

use super::vms::{create_vm, parse_id, start_vm_request};

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
    let _ = start_vm_request(
        State(state.clone()),
        Extension(request_id),
        Path(created.id.to_string()),
    )
    .await?;

    let build_id = state.builds.begin(&alias, created.id);
    Ok((
        StatusCode::ACCEPTED,
        Json(state.builds.get(build_id).expect("just inserted")),
    ))
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

    state.builds.set_status(build_id, BuildStatus::Installing);
    state.builds.append_log(
        build_id,
        format!("{:?} {}", body.action, body.packages.join(" ")),
    );

    let vm_response = super::packages::run_package_action(
        State(state.clone()),
        Extension(request_id),
        Path(session.vm_id.to_string()),
        Json(body),
    )
    .await
    .inspect_err(|_| state.builds.set_status(build_id, BuildStatus::Ready))?;

    // run_package_action detaches the actual console wait onto a spawned
    // task and returns immediately with `Running` — poll it here the same
    // way `packages.rs`'s own tests do, so build_packages's caller gets a
    // definite outcome instead of another poll loop layered on top.
    //
    // Reaching this point means run_package_action successfully dispatched
    // the command to the builder VM's console, so this session now has a
    // package action to its name regardless of how the action ultimately
    // resolves — `finalize_build` (Task 9) gates on this flag to refuse
    // registering a template from a session that never customized anything.
    state.builds.mark_package_action_done(build_id);

    let outcome = wait_for_package_outcome(&state, session.vm_id).await;
    match outcome {
        Some(firecrab_api_types::PackageUpdateStatus::Succeeded { output_tail }) => {
            state.builds.append_log(build_id, output_tail);
            state.builds.set_status(build_id, BuildStatus::Ready);
        }
        Some(firecrab_api_types::PackageUpdateStatus::Failed {
            reason,
            output_tail,
        }) => {
            state
                .builds
                .append_log(build_id, format!("{reason}\n{output_tail}"));
            state.builds.set_status(build_id, BuildStatus::Ready);
        }
        _ => state.builds.set_status(build_id, BuildStatus::Ready),
    }

    let _ = vm_response;
    Ok(Json(
        state.builds.get(build_id).expect("session still tracked"),
    ))
}

/// Polls `state.vms[vm_id].package_update` until it leaves `Running` or a
/// bounded number of attempts elapse — `run_package_action`'s console wait
/// runs on a detached task, so this is the only way to observe its result
/// from a caller that itself must return a single HTTP response.
async fn wait_for_package_outcome(
    state: &AppState,
    vm_id: Uuid,
) -> Option<firecrab_api_types::PackageUpdateStatus> {
    for _ in 0..600 {
        let status = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&vm_id)
            .and_then(|vm| vm.package_update.clone());
        match status {
            Some(firecrab_api_types::PackageUpdateStatus::Running) | None => {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            other => return other,
        }
    }
    None
}

/// `POST /api/images/builds/{buildId}/finalize` — stops the builder VM,
/// pulls its rootfs disk out from under `delete_vm`'s artifact cleanup,
/// strips guest identity, registers it as a new template version, then
/// deletes the builder VM. `newAlias` in the request body determines
/// whether this is an in-place rebuild (omitted) or a derived template
/// (given) — decided here rather than at `start_build` time, since the
/// operator may only know which they want after seeing what changed.
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

    // `build_packages` flips `had_package_action` right after it dispatches
    // a command to the builder VM's console — before the outcome is known
    // — so a session can still have one running here even though that flag
    // already reads `true`. Finalizing mid-install would race
    // `wait_for_package_outcome`'s poll for the very disk this handler is
    // about to copy out from under it, so refuse outright rather than risk
    // grabbing a half-written rootfs.
    if session.status == BuildStatus::Installing {
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

    let _ = super::vms::stop_vm(
        State(state.clone()),
        Extension(request_id),
        Path(session.vm_id.to_string()),
    )
    .await?;

    if let Err(reason) = finalize_and_register(&state, &session, &target_alias, request_id.0).await
    {
        state.builds.finish_err(parsed_build_id, &reason);
        // Best-effort: the disk/register failure above is the error this
        // call reports, so a second failure here (delete_vm) is swallowed
        // rather than masking it. If delete_vm itself fails, the builder VM
        // record survives — `VmPurpose::Builder` hides it from `list_vms`,
        // so it isn't operator-visible for cleanup; the session's own log
        // (via `finish_err`) is the only trace, which is an accepted gap for
        // this task rather than something Task 9 attempts to close.
        let _ = super::vms::delete_vm(
            State(state.clone()),
            Extension(request_id),
            Path(session.vm_id.to_string()),
        )
        .await;
        return Err(AppError::internal(request_id.0));
    }

    super::vms::delete_vm(
        State(state.clone()),
        Extension(request_id),
        Path(session.vm_id.to_string()),
    )
    .await?;

    state.builds.finish_ok(parsed_build_id, &target_alias);
    Ok(Json(
        state
            .builds
            .get(parsed_build_id)
            .expect("session still tracked"),
    ))
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

    let spec = crate::templates::TemplateSpec {
        alias: target_alias.to_owned(),
        version: version_tag,
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

    Ok(())
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
        crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);
        let (_status, Json(build)) = start_build(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path("ubuntu-rootfs-26.04".to_owned()),
        )
        .await
        .unwrap();

        // The fixture Firecracker binary doesn't exist, so the builder VM never
        // reaches Running — build_packages must surface that as a normal
        // conflict, not panic.
        let error = build_packages(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(build.build_id.to_string()),
            Json(firecrab_api_types::PackageAction {
                action: firecrab_api_types::PackageActionKind::Install,
                packages: vec!["curl".to_owned()],
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
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

    #[tokio::test]
    async fn build_packages_records_a_succeeded_outcome_and_marks_the_action_done() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = VmRecord {
            state: VmState::Running,
            ..record("builder-vm", Uuid::new_v4())
        };
        seed_vm(&state, &vm);
        let console = register_fake_process(&state, vm.id);
        let build_id = state.builds.begin("ubuntu-rootfs-26.04", vm.id);

        let handle = tokio::spawn(build_packages(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(build_id.to_string()),
            Json(firecrab_api_types::PackageAction {
                action: firecrab_api_types::PackageActionKind::Update,
                packages: Vec::new(),
            }),
        ));

        wait_for_console_subscriber(&console).await;
        console.push_output(b"FIRECRAB_PKG_UPDATE_DONE:0\n");

        let Json(build) = handle
            .await
            .expect("build_packages task panicked")
            .expect("build_packages returned an error");

        assert_eq!(build.status, BuildStatus::Ready);
        assert!(build.had_package_action);
        assert!(build.log.contains("FIRECRAB_PKG_UPDATE_DONE:0"));
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

        let handle = tokio::spawn(build_packages(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(build_id.to_string()),
            Json(firecrab_api_types::PackageAction {
                action: firecrab_api_types::PackageActionKind::Install,
                packages: vec!["curl".to_owned()],
            }),
        ));

        wait_for_console_subscriber(&console).await;
        console.push_output(b"FIRECRAB_PKG_UPDATE_DONE:1\n");

        let Json(build) = handle
            .await
            .expect("build_packages task panicked")
            .expect("build_packages returned an error");

        // Even though the package action itself failed, build_packages must
        // leave the session in a normal, resumable state rather than stuck
        // in `Installing` — and the session did have a package action run
        // against it, so `had_package_action` still flips to true.
        assert_eq!(build.status, BuildStatus::Ready);
        assert!(build.had_package_action);
        assert!(build.log.contains("exited with code 1"));
    }

    #[tokio::test]
    async fn finalize_build_rejects_a_session_with_no_successful_package_action() {
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

        let error = finalize_build(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(build.build_id.to_string()),
            ValidatedJson(FinalizeBuildRequest { new_alias: None }),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
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
        crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);
        let (_status, Json(build)) = start_build(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path("ubuntu-rootfs-26.04".to_owned()),
        )
        .await
        .unwrap();
        state.builds.mark_package_action_done(build.build_id);

        // "alpine-3.24" is one of TemplateRegistry::known_specs's built-in
        // aliases — reserved even though this registry never actually
        // installed it, so a derived build can't shadow it.
        let error = finalize_build(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(build.build_id.to_string()),
            ValidatedJson(FinalizeBuildRequest {
                new_alias: Some("alpine-3.24".to_owned()),
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    }

    /// `build_packages` (Task 8) flips `had_package_action` as soon as it
    /// successfully *dispatches* a command to the builder VM's console —
    /// before `wait_for_package_outcome` observes any result — so a session
    /// can be mid-install (`BuildStatus::Installing`) with the flag already
    /// `true`. `finalize_build` must refuse this case even though the
    /// `had_package_action` check alone would let it through, since the
    /// disk it's about to copy could still be mid-write on the builder VM.
    #[tokio::test]
    async fn finalize_build_rejects_a_build_still_installing_packages() {
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

        state.builds.mark_package_action_done(build.build_id);
        state
            .builds
            .set_status(build.build_id, BuildStatus::Installing);

        let error = finalize_build(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(build.build_id.to_string()),
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
        crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);

        let (_status, Json(build)) = start_build(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path("ubuntu-rootfs-26.04".to_owned()),
        )
        .await
        .unwrap();

        // Simulate a builder VM whose disk-prep step already ran (as it
        // would have, early in a real `start_vm`, well before `Running`) —
        // this fixture never actually boots Firecracker, so seed a real
        // ext4 file at the disk generation path `finalize_and_register`
        // expects instead.
        let generation = Uuid::new_v4();
        let artifact_paths =
            crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for("default"), build.vm_id);
        artifact_paths.ensure_directories().unwrap();
        let disk_path = artifact_paths.rootfs(generation);
        std::process::Command::new("mkfs.ext4")
            .args(["-q", "-F"])
            .arg(&disk_path)
            .arg("8M")
            .status()
            .unwrap();
        {
            let mut vms = state.vms.lock().unwrap();
            let vm = vms.get_mut(&build.vm_id).unwrap();
            vm.disk_generation = Some(generation);
        }

        let session = state.builds.get(build.build_id).unwrap();
        finalize_and_register(&state, &session, "my-nginx-base", Uuid::new_v4())
            .await
            .unwrap();

        let registered = state.templates.resolve_alias("my-nginx-base");
        assert!(registered.is_some());
        assert_eq!(registered.unwrap().name, "my-nginx-base");
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
        crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);

        let (_status, Json(build)) = start_build(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path("ubuntu-rootfs-26.04".to_owned()),
        )
        .await
        .unwrap();

        let generation = Uuid::new_v4();
        let artifact_paths =
            crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for("default"), build.vm_id);
        artifact_paths.ensure_directories().unwrap();
        let disk_path = artifact_paths.rootfs(generation);
        std::process::Command::new("mkfs.ext4")
            .args(["-q", "-F"])
            .arg(&disk_path)
            .arg("8M")
            .status()
            .unwrap();
        {
            let mut vms = state.vms.lock().unwrap();
            let vm = vms.get_mut(&build.vm_id).unwrap();
            vm.disk_generation = Some(generation);
            vm.state = VmState::Running;
        }
        state.builds.mark_package_action_done(build.build_id);

        let Json(finalized) = finalize_build(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(build.build_id.to_string()),
            ValidatedJson(FinalizeBuildRequest {
                new_alias: Some("my-nginx-base".to_owned()),
            }),
        )
        .await
        .unwrap();

        assert_eq!(finalized.status, BuildStatus::Succeeded);
        assert_eq!(finalized.target_alias.as_deref(), Some("my-nginx-base"));
        assert!(state.templates.resolve_alias("my-nginx-base").is_some());
        // delete_vm ran: the builder VM record is gone.
        assert!(state.vms.lock().unwrap().get(&build.vm_id).is_none());
    }

    /// When `finalize_and_register` fails (here: no disk generation was
    /// ever recorded, so there's nothing to copy), `finalize_build` must
    /// still record the failure and clean up the builder VM rather than
    /// leaving it orphaned and hidden from `list_vms` forever.
    #[tokio::test]
    async fn finalize_build_cleans_up_the_builder_vm_when_the_disk_copy_fails() {
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

        // Running, but deliberately no disk_generation — the disk-prep step
        // that would normally set it never ran in this fixture.
        {
            let mut vms = state.vms.lock().unwrap();
            let vm = vms.get_mut(&build.vm_id).unwrap();
            vm.state = VmState::Running;
        }
        state.builds.mark_package_action_done(build.build_id);

        let error = finalize_build(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(build.build_id.to_string()),
            ValidatedJson(FinalizeBuildRequest { new_alias: None }),
        )
        .await
        .unwrap_err();

        assert_eq!(
            error.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        let session = state.builds.get(build.build_id).unwrap();
        assert_eq!(session.status, BuildStatus::Failed);
        assert!(session.log.contains("no disk generation"));
        // delete_vm still ran despite the finalize failure.
        assert!(state.vms.lock().unwrap().get(&build.vm_id).is_none());
    }
}

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
use firecrab_api_types::{BuildResponse, BuildStatus, CreateVmRequest, EgressPolicy};
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::model::VmPurpose;
use crate::server::RequestId;
use crate::state::AppState;

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
}

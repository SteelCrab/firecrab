//! Durable, idempotent preparation of an Ubuntu coding VM.
use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use firecrab_api_types::{
    CreateMicroNetworkRequest, CreateVmRequest, CreateWorkspaceRequest, EgressPolicy,
    ImageInstallStatus, WorkspaceSetup, WorkspaceSetupPhase as Phase,
    WorkspaceSetupState as Status,
};
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::handlers::{images, micro_networks, vms};
use crate::model::{VmPurpose, VmState};
use crate::server::RequestId;
use crate::state::AppState;

pub const IMAGE: &str = "ubuntu-26.04";

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub struct Runtime {
    admission: AsyncMutex<()>,
    networks: AsyncMutex<()>,
    workers: Mutex<HashSet<Uuid>>,
    slots: Semaphore,
}
impl Default for Runtime {
    fn default() -> Self {
        Self {
            admission: AsyncMutex::new(()),
            networks: AsyncMutex::new(()),
            workers: Mutex::new(HashSet::new()),
            slots: Semaphore::new(2),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SavedSetup {
    pub request: CreateWorkspaceRequest,
    pub progress: WorkspaceSetup,
    pub reserved_network: Uuid,
}

fn failed(request: RequestId) -> AppError {
    AppError::internal(request.0)
}
fn conflict(code: &'static str, message: &'static str, request: RequestId) -> AppError {
    AppError::conflict(code, message, request.0)
}
async fn read(state: &AppState, id: Uuid) -> Result<SavedSetup, AppError> {
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || store.workspace_setup(id))
        .await
        .map_err(|_| failed(RequestId(id)))?
        .map_err(|_| failed(RequestId(id)))?
        .ok_or_else(|| AppError::not_found(id))
}
async fn save(state: &AppState, job: &SavedSetup) -> Result<(), AppError> {
    let store = state.store.clone();
    let job = job.clone();
    let id = job.progress.id;
    tokio::task::spawn_blocking(move || store.save_workspace_setup(&job))
        .await
        .map_err(|_| failed(RequestId(id)))?
        .map_err(|_| failed(RequestId(id)))
}
async fn queue_capacity(state: &AppState, request: RequestId) -> Result<(), AppError> {
    let store = state.store.clone();
    let jobs = tokio::task::spawn_blocking(move || store.workspace_setups())
        .await
        .map_err(|_| failed(request))?
        .map_err(|_| failed(request))?;
    if jobs
        .iter()
        .filter(|job| matches!(job.progress.state, Status::Queued | Status::Running))
        .count()
        >= 8
    {
        return Err(conflict(
            "workspace_queue_full",
            "Wait for a workspace to finish preparing before creating another",
            request,
        ));
    }
    Ok(())
}
pub async fn list(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
) -> Result<Json<Vec<WorkspaceSetup>>, AppError> {
    let store = state.store.clone();
    let jobs = tokio::task::spawn_blocking(move || store.workspace_setups())
        .await
        .map_err(|_| failed(request))?
        .map_err(|_| failed(request))?;
    Ok(Json(
        jobs.into_iter().take(50).map(|job| job.progress).collect(),
    ))
}
pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkspaceSetup>, AppError> {
    Ok(Json(read(&state, id).await?.progress))
}

fn validate(request: &CreateWorkspaceRequest) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    if request.id.is_nil() {
        fields.insert("id".into(), "must not be nil".into());
    }
    if request.name.len() > 64
        || !request
            .name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    {
        fields.insert(
            "name".into(),
            "use up to 64 letters, numbers, '.', '_' or '-'".into(),
        );
    }
    if !(1..=32).contains(&request.cpu) {
        fields.insert("cpu".into(), "must be between 1 and 32".into());
    }
    if !(1024..=32768).contains(&request.ram) || !request.ram.is_power_of_two() {
        fields.insert(
            "ram".into(),
            "choose 1024, 2048, 4096, 8192, 16384 or 32768 MiB".into(),
        );
    }
    if !(8..=500).contains(&request.disk_gb) {
        fields.insert("diskGb".into(), "must be between 8 and 500 GiB".into());
    }
    fields
}

/// Identical POST retries inspect the same reservation; only explicit failed-job retries resume it.
pub async fn create(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    ValidatedJson(input): ValidatedJson<CreateWorkspaceRequest>,
) -> Result<(StatusCode, Json<WorkspaceSetup>), AppError> {
    let fields = validate(&input);
    if !fields.is_empty() {
        return Err(AppError::validation(fields, request.0));
    }
    let _admission = state.workspace_setups.admission.lock().await;
    let store = state.store.clone();
    let id = input.id;
    let previous = tokio::task::spawn_blocking(move || store.workspace_setup(id))
        .await
        .map_err(|_| failed(request))?
        .map_err(|_| failed(request))?;
    let job = if let Some(mut previous) = previous {
        if previous.request != input {
            return Err(conflict(
                "workspace_request_changed",
                "Use the original settings when retrying this workspace",
                request,
            ));
        }
        if previous.progress.state == Status::Failed {
            queue_capacity(&state, request).await?;
            previous.progress.state = Status::Queued;
            previous.progress.error = None;
            save(&state, &previous).await?;
        }
        previous
    } else {
        queue_capacity(&state, request).await?;
        let name = if input.name.is_empty() {
            format!("workspace-{}", &input.id.simple().to_string()[..8])
        } else {
            input.name.clone()
        };
        let job = SavedSetup {
            reserved_network: Uuid::new_v4(),
            progress: WorkspaceSetup {
                id: input.id,
                vm_id: Uuid::new_v4(),
                name,
                cpu: input.cpu,
                ram: input.ram,
                disk_gb: input.disk_gb,
                micro_network_id: input.micro_network_id,
                storage_root: input.storage_root.clone(),
                state: Status::Queued,
                phase: Phase::Image,
                error: None,
            },
            request: input,
        };
        save(&state, &job).await?;
        job
    };
    if job.progress.state != Status::Ready {
        spawn(state.clone(), job.progress.id);
    }
    Ok((StatusCode::ACCEPTED, Json(job.progress)))
}

fn spawn(state: AppState, id: Uuid) {
    if !state
        .workspace_setups
        .workers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id)
    {
        return;
    }
    tokio::spawn(async move {
        let result = std::panic::AssertUnwindSafe(run(&state, id))
            .catch_unwind()
            .await;
        let error = match result {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error.redacted().description()),
            Err(_) => Some("Workspace preparation was interrupted. Retry to continue.".into()),
        };
        // A failed job must leave the worker set atomically with respect to explicit retries.
        let _admission = state.workspace_setups.admission.lock().await;
        if let Some(error) = error {
            tracing::warn!(workspace_id=%id,%error,"workspace preparation failed");
            if let Ok(mut job) = read(&state, id).await {
                job.progress.state = Status::Failed;
                job.progress.error = Some(error);
                let _ = save(&state, &job).await;
            }
        }
        state
            .workspace_setups
            .workers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
    });
}
pub async fn recover(state: &AppState) {
    let store = state.store.clone();
    match tokio::task::spawn_blocking(move || store.workspace_setups()).await {
        Ok(Ok(jobs)) => {
            for job in jobs {
                if matches!(job.progress.state, Status::Queued | Status::Running) {
                    spawn(state.clone(), job.progress.id);
                }
            }
        }
        _ => tracing::error!("could not recover workspace preparation jobs"),
    }
}
async fn phase(state: &AppState, job: &mut SavedSetup, phase: Phase) -> Result<(), AppError> {
    job.progress.phase = phase;
    save(state, job).await
}

async fn image_job(state: &AppState, request: RequestId, package: bool) -> Result<(), AppError> {
    let tracker = if package {
        &state.image_packages
    } else {
        &state.image_installs
    };
    if !tracker.is_running(IMAGE) {
        let result = if package {
            images::start_image_package(
                State(state.clone()),
                Path(IMAGE.into()),
                Extension(request),
            )
            .await
            .map(|_| ())
        } else {
            images::start_image_install(
                State(state.clone()),
                Path(IMAGE.into()),
                Extension(request),
            )
            .await
            .map(|_| ())
        };
        if let Err(error) = result {
            if !tracker.is_running(IMAGE) && state.templates.resolve_alias(IMAGE).is_none() {
                return Err(error);
            }
        }
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1800);
    loop {
        if state.templates.resolve_alias(IMAGE).is_some() {
            return Ok(());
        }
        match tracker.snapshot(IMAGE).status {
            ImageInstallStatus::Succeeded => return Ok(()),
            ImageInstallStatus::Running => {}
            _ => {
                return Err(conflict(
                    "workspace_image_failed",
                    "Ubuntu preparation failed; inspect the image installation log and retry",
                    request,
                ));
            }
        }
        if tokio::time::Instant::now() > deadline {
            return Err(conflict(
                "workspace_image_timeout",
                "Ubuntu is still being prepared; retry to check the existing installation",
                request,
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn network(
    state: &AppState,
    job: &mut SavedSetup,
    request: RequestId,
) -> Result<Uuid, AppError> {
    let _guard = state.workspace_setups.networks.lock().await;
    let store = state.store.clone();
    let mut networks = tokio::task::spawn_blocking(move || store.list_micro_networks())
        .await
        .map_err(|_| failed(request))?
        .map_err(|_| failed(request))?;
    if let Some(id) = job.progress.micro_network_id {
        return networks
            .iter()
            .find(|n| n.id == id && n.internet_enabled)
            .map(|n| n.id)
            .ok_or_else(|| {
                conflict(
                    "workspace_network_unavailable",
                    "Select an internet-enabled MicroNetwork and create a new workspace",
                    request,
                )
            });
    }
    networks.sort_by_key(|n| (n.id != job.reserved_network, n.name != "default", n.id));
    if let Some(existing) = networks.iter().find(|n| n.internet_enabled) {
        job.progress.micro_network_id = Some(existing.id);
        save(state, job).await?;
        return Ok(existing.id);
    }
    for prefix in ["172.30", "10.203", "192.168"] {
        for subnet in 240..248 {
            let input = CreateMicroNetworkRequest {
                name: format!(
                    "workspace-{}",
                    &job.reserved_network.simple().to_string()[..8]
                ),
                subnet_cidr: format!("{prefix}.{subnet}.0/24"),
                internet_enabled: true,
                uplink: None,
                ipv6_cidr: None,
                ipv6_address_mode: None,
                dns_mode: Default::default(),
            };
            match micro_networks::create_micro_network_with_id(
                state.clone(),
                request,
                input,
                job.reserved_network,
            )
            .await
            {
                Ok((_, Json(created))) => {
                    job.progress.micro_network_id = Some(created.id);
                    save(state, job).await?;
                    return Ok(created.id);
                }
                Err(error)
                    if error
                        .field_names()
                        .iter()
                        .any(|field| field == "subnetCidr") =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
    }
    Err(conflict(
        "workspace_network_conflict",
        "No automatic network range is available; choose a MicroNetwork in advanced settings",
        request,
    ))
}

async fn run(state: &AppState, id: Uuid) -> Result<(), AppError> {
    let _slot = state
        .workspace_setups
        .slots
        .acquire()
        .await
        .map_err(|_| failed(RequestId(id)))?;
    let mut job = read(state, id).await?;
    if job.progress.state == Status::Ready {
        return Ok(());
    }
    let request = RequestId(id);
    job.progress.state = Status::Running;
    job.progress.error = None;
    phase(state, &mut job, Phase::Image).await?;
    if state.templates.resolve_alias(IMAGE).is_none() {
        if !state.image_installs.is_running(IMAGE) {
            if !crate::image_install::staged_package_exists(
                state.templates.image_root_path(),
                IMAGE,
            ) {
                image_job(state, request, true).await?;
            }
        }
        image_job(state, request, false).await?;
    }
    let template = state
        .templates
        .resolve_alias(IMAGE)
        .ok_or_else(|| failed(request))?;
    job.progress.disk_gb = job.request.disk_gb.max(
        u16::try_from(template.rootfs.length().div_ceil(1024 * 1024 * 1024))
            .map_err(|_| failed(request))?,
    );
    phase(state, &mut job, Phase::Network).await?;
    let network = network(state, &mut job, request).await?;
    phase(state, &mut job, Phase::Create).await?;
    let existing = state
        .vms
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&job.progress.vm_id)
        .cloned();
    if let Some(vm) = existing {
        if vm.template != IMAGE
            || vm.micro_network_id != network
            || vm.purpose != VmPurpose::Instance
            || vm.name != job.progress.name
            || vm.cpu != job.progress.cpu
            || vm.ram != job.progress.ram
            || vm.disk_gb != job.progress.disk_gb
        {
            return Err(conflict(
                "workspace_vm_changed",
                "The reserved VM settings changed; inspect the existing VM before continuing",
                request,
            ));
        }
        vms::resume_cluster_create(state, vm.id, request).await?;
    } else {
        let _ = vms::create_vm_with_id(
            state.clone(),
            request,
            CreateVmRequest {
                name: job.progress.name.clone(),
                template: IMAGE.into(),
                cpu: job.progress.cpu,
                ram: job.progress.ram,
                disk_gb: job.progress.disk_gb,
                egress_policy: EgressPolicy::Internet,
                micro_network_id: network,
                storage_root: job.progress.storage_root.clone(),
                security_group_id: None,
                block_storages: vec![],
                shell_ids: vec![],
                port_forwards: vec![],
                env: BTreeMap::new(),
            },
            job.progress.vm_id,
        )
        .await?;
    }
    phase(state, &mut job, Phase::Start).await?;
    let vm_state = state
        .vms
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&job.progress.vm_id)
        .map(|vm| vm.state)
        .ok_or_else(|| failed(request))?;
    match vm_state {
        VmState::Running => {}
        VmState::Created | VmState::Stopped | VmState::Error => {
            let _ = vms::start_vm(
                State(state.clone()),
                Extension(request),
                Path(job.progress.vm_id.to_string()),
            )
            .await?;
        }
        _ => {
            return Err(conflict(
                "workspace_vm_busy",
                "The VM lifecycle changed; wait for it to settle before retrying",
                request,
            ));
        }
    }
    job.progress.state = Status::Ready;
    phase(state, &mut job, Phase::Complete).await
}

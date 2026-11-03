//! Operator-requested Canvas setup using stable project and volume identities.
use super::{bind_workspace, denied, reconcile};
use crate::{
    ai_sandbox::access::Caller,
    error::AppError,
    extract::ValidatedJson,
    handlers,
    model::{VmPurpose, VmState},
    persistence::PersistenceError,
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    routing::put,
};
use firecrab_api_types::{
    BindProjectWorkspace, CreateMicroBlockStorageRequest, SandboxPrincipal, SandboxProjectContext,
    SandboxProjectSpec,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

static PROVISION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Request {
    vm_id: Uuid,
    name: String,
    volume_size_gib: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Record {
    pub id: Uuid,
    pub project_id: Uuid,
    pub vm_id: Uuid,
    pub volume_id: Uuid,
    owner: SandboxPrincipal,
    request: Request,
    storage_root: String,
    phase: String,
}

pub(super) fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/ai-sandbox/canvas/provisions/{operation}",
        put(provision),
    )
}

fn conflict(id: RequestId, message: &'static str) -> AppError {
    AppError::conflict("canvas_provision_conflict", message, id.0)
}

async fn provision(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(operation): Path<Uuid>,
    ValidatedJson(request): ValidatedJson<Request>,
) -> Result<Json<Record>, AppError> {
    caller.operator().map_err(|_| denied(id.0))?;
    if operation.is_nil()
        || request.vm_id.is_nil()
        || request.name.trim() != request.name
        || request.name.is_empty()
        || request.name.len() > 200
        || request.name.chars().any(char::is_control)
        || request.volume_size_gib == 0
    {
        return Err(conflict(
            id,
            "Choose a valid project name, VM and workspace volume size",
        ));
    }
    // The worker owns its locks after HTTP disconnect; the same identity resumes it.
    tokio::spawn(run(state, id, caller, operation, request))
        .await
        .map_err(|_| AppError::internal(id.0))?
        .map(Json)
}

async fn run(
    state: AppState,
    id: RequestId,
    caller: Caller,
    operation: Uuid,
    request: Request,
) -> Result<Record, AppError> {
    let _guard = PROVISION.lock().await;
    let mut record = match state
        .store
        .canvas_provision(operation)
        .map_err(|_| AppError::internal(id.0))?
    {
        Some(record) => {
            if record.id != operation
                || record.owner != caller.principal
                || record.request != request
                || record.vm_id != request.vm_id
            {
                return Err(conflict(
                    id,
                    "The operation identity belongs to another request",
                ));
            }
            record
        }
        None => {
            let vm = state
                .vms
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&request.vm_id)
                .cloned()
                .ok_or_else(|| AppError::not_found(id.0))?;
            if vm.purpose != VmPurpose::Instance
                || vm.state != VmState::Created
                || state
                    .store
                    .sandbox_projects()
                    .map_err(|_| AppError::internal(id.0))?
                    .iter()
                    .any(|project| project.spec.vm_id == Some(vm.id))
                || state
                    .store
                    .block_storages()
                    .map_err(|_| AppError::internal(id.0))?
                    .iter()
                    .any(|volume| volume.attached_vm_id == Some(vm.id))
            {
                return Err(conflict(
                    id,
                    "Select a newly created VM with no project or attached volume",
                ));
            }
            let record = Record {
                id: operation,
                project_id: Uuid::new_v4(),
                vm_id: vm.id,
                volume_id: Uuid::new_v4(),
                owner: caller.principal,
                request,
                storage_root: vm.storage_root,
                phase: "reserved".into(),
            };
            state
                .store
                .reserve_canvas_provision(&record)
                .map_err(|_| conflict(id, "This VM or operation is already reserved"))?;
            record
        }
    };
    if record.phase == "ready" {
        return Ok(record);
    }
    let spec = SandboxProjectSpec {
        name: record.request.name.clone(),
        source_path: None,
        vm_id: Some(record.vm_id),
        selection: None,
        instructions: String::new(),
        context: SandboxProjectContext::default(),
        environment_id: None,
    };
    let existing = state
        .store
        .sandbox_project(record.project_id)
        .map_err(|_| AppError::internal(id.0))?;
    if let Some(project) = existing {
        if project.owner != record.owner || project.spec != spec {
            return Err(conflict(id, "The reserved project settings changed"));
        }
    } else if record.phase == "reserved" {
        handlers::ai_sandbox::save_project(
            State(state.clone()),
            Extension(id),
            caller,
            Path(record.project_id),
            ValidatedJson(spec),
        )
        .await?;
    } else {
        return Err(conflict(id, "The reserved project is missing"));
    }
    if record.phase == "reserved" {
        record.phase = "project".into();
        state
            .store
            .save_canvas_provision(&record)
            .map_err(|_| AppError::internal(id.0))?;
    }
    let expected_name = format!("canvas-{}", record.id);
    let volume = match state.store.block_storage(record.volume_id) {
        Ok(volume) => volume,
        Err(PersistenceError::MissingBlockStorage { .. }) if record.phase == "project" => {
            handlers::block_storage::create_with_id(
                state.clone(),
                id,
                CreateMicroBlockStorageRequest {
                    name: expected_name.clone(),
                    size_gib: record.request.volume_size_gib,
                    storage_root: Some(record.storage_root.clone()),
                },
                record.volume_id,
            )
            .await?
            .1
            .0
        }
        Err(_) => {
            return Err(conflict(
                id,
                "The reserved workspace volume is missing or unavailable",
            ));
        }
    };
    if volume.name != expected_name
        || volume.size_gib != record.request.volume_size_gib
        || volume.storage_root != record.storage_root
        || !matches!(volume.state.as_str(), "available" | "attached")
        || volume.attached_vm_id.is_some_and(|vm| vm != record.vm_id)
    {
        return Err(conflict(
            id,
            "The reserved workspace volume changed or is still being created",
        ));
    }
    if record.phase == "project" {
        record.phase = "volume".into();
        state
            .store
            .save_canvas_provision(&record)
            .map_err(|_| AppError::internal(id.0))?;
    }
    if record.phase == "volume" {
        bind_workspace(
            State(state.clone()),
            Extension(id),
            caller,
            Path(record.project_id),
            ValidatedJson(BindProjectWorkspace {
                volume_id: record.volume_id,
                expected_generation: None,
            }),
        )
        .await?;
        record.phase = "bound".into();
        state
            .store
            .save_canvas_provision(&record)
            .map_err(|_| AppError::internal(id.0))?;
    }
    let binding = state
        .store
        .project_workspace(record.project_id)
        .map_err(|_| AppError::internal(id.0))?
        .ok_or_else(|| conflict(id, "Workspace binding is missing"))?;
    if binding.vm_id != record.vm_id
        || binding.volume_id != record.volume_id
        || binding.provisioning
    {
        return Err(conflict(id, "Workspace binding changed"));
    }
    let vm = state
        .vms
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&record.vm_id)
        .cloned()
        .ok_or_else(|| conflict(id, "Workspace VM is missing"))?;
    if record.phase == "bound" {
        if vm.state != VmState::Created {
            return Err(conflict(id, "Workspace VM is no longer newly created"));
        }
        record.phase = "starting".into();
        state
            .store
            .save_canvas_provision(&record)
            .map_err(|_| AppError::internal(id.0))?;
    }
    if record.phase == "starting" {
        match vm.state {
            VmState::Created => {
                handlers::vms::start_vm_request(
                    State(state.clone()),
                    Extension(id),
                    Path(record.vm_id.to_string()),
                )
                .await?;
                return Ok(record);
            }
            VmState::Starting => return Ok(record),
            VmState::Running => {
                reconcile(
                    State(state.clone()),
                    Extension(id),
                    caller,
                    Path(record.project_id),
                )
                .await?;
                record.phase = "ready".into();
                state
                    .store
                    .save_canvas_provision(&record)
                    .map_err(|_| AppError::internal(id.0))?;
            }
            _ => {
                return Err(conflict(
                    id,
                    "The workspace VM did not start; inspect its state before retrying",
                ));
            }
        }
    }
    if record.phase != "ready" {
        return Err(conflict(id, "Unknown Canvas provision phase"));
    }
    Ok(record)
}

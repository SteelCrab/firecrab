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
    BindProjectWorkspace, CreateMicroBlockStorageRequest, CreateVmRequest, SandboxPrincipal,
    SandboxProjectContext, SandboxProjectSpec,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

static PROVISION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static VM_CREATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CreationRequest {
    vm: CreateVmRequest,
    name: String,
    volume_size_gib: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreationRecord {
    pub id: Uuid,
    owner: SandboxPrincipal,
    request: CreationRequest,
    template_name: String,
    template_pin: firecrab_stack::TemplatePin,
    storage_root: String,
    phase: String,
}

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
    Router::new()
        .route(
            "/api/ai-sandbox/canvas/provisions/{operation}",
            put(provision),
        )
        .route(
            "/api/ai-sandbox/canvas/creations/{operation}",
            put(create).delete(cancel_creation),
        )
}

fn conflict(id: RequestId, message: &'static str) -> AppError {
    AppError::conflict("canvas_provision_conflict", message, id.0)
}

/// Persist the full non-secret creation request before creating any VM resource.
async fn create(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(operation): Path<Uuid>,
    ValidatedJson(request): ValidatedJson<CreationRequest>,
) -> Result<Json<Record>, AppError> {
    caller.operator().map_err(|_| denied(id.0))?;
    if operation.is_nil()
        || request.name.trim() != request.name
        || request.name.is_empty()
        || request.name.len() > 200
        || request.name.chars().any(char::is_control)
        || request.volume_size_gib == 0
        || !request.vm.block_storages.is_empty()
        || !request.vm.shell_ids.is_empty()
        || !request.vm.port_forwards.is_empty()
        || !request.vm.env.is_empty()
        || request.vm.template == crate::microboot::MICROBOOT_ALIAS
    {
        return Err(conflict(
            id,
            "Canvas creation requires a valid project and a VM without advanced resources",
        ));
    }
    // A timed-out HTTP request leaves its worker running. Do not queue another
    // detached worker on every retry while that worker still owns creation.
    let guard = VM_CREATION.try_lock().map_err(|_| {
        AppError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "canvas_creation_busy",
            "Canvas creation is still in progress",
            id.0,
        )
    })?;
    tokio::spawn(async move {
        let _guard = guard;
        run_creation(state, id, caller, operation, request).await
    })
    .await
    .map_err(|_| AppError::internal(id.0))?
    .map(Json)
}

async fn run_creation(
    state: AppState,
    id: RequestId,
    caller: Caller,
    operation: Uuid,
    request: CreationRequest,
) -> Result<Record, AppError> {
    if state
        .store
        .canvas_creation_retired(operation)
        .map_err(|_| AppError::internal(id.0))?
    {
        return Err(conflict(
            id,
            "This Canvas VM creation was cancelled or its VM was deleted",
        ));
    }
    let mut creation = match state
        .store
        .canvas_vm_creation(operation)
        .map_err(|_| AppError::internal(id.0))?
    {
        Some(saved) => {
            if saved.owner != caller.principal || saved.request != request {
                return Err(conflict(
                    id,
                    "The VM creation identity belongs to a different request",
                ));
            }
            saved
        }
        None => {
            let template = state
                .templates
                .resolve_alias(&request.vm.template)
                .ok_or_else(|| conflict(id, "The selected VM image is unavailable"))?;
            let fields = handlers::vms::validate_create_for(&request.vm, Some(&template), &state);
            if !fields.is_empty() {
                return Err(AppError::validation(fields, id.0));
            }
            let root = request
                .vm
                .storage_root
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| state.storage.default_id())
                .to_owned();
            let saved = CreationRecord {
                id: operation,
                owner: caller.principal,
                request,
                template_name: template.name.clone(),
                template_pin: template.stack_pin(),
                storage_root: root,
                phase: "reserved".into(),
            };
            state
                .store
                .reserve_canvas_vm_creation(&saved)
                .map_err(|_| conflict(id, "This VM creation identity is unavailable"))?;
            saved
        }
    };
    if creation.phase == "reserved" {
        let template = state
            .templates
            .resolve_version(&creation.template_name, &creation.template_pin.version)
            .filter(|template| template.stack_pin() == creation.template_pin)
            .ok_or_else(|| {
                conflict(
                    id,
                    "The originally selected VM image is unavailable or changed",
                )
            })?;
        let vm = state
            .vms
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&operation)
            .cloned();
        if let Some(vm) = vm {
            let expected = &creation.request.vm;
            if vm.purpose != VmPurpose::Instance
                || vm.state != VmState::Created
                || vm.name != expected.name
                || vm.template != creation.template_name
                || vm.template_version != creation.template_pin.version
                || vm.template_kernel_sha256 != creation.template_pin.kernel_sha256
                || vm.template_rootfs_sha256 != creation.template_pin.rootfs_sha256
                || vm.template_boot_args_sha256 != creation.template_pin.boot_args_sha256
                || vm.cpu != expected.cpu
                || vm.ram != expected.ram
                || vm.disk_gb != expected.disk_gb
                || vm.egress_policy != expected.egress_policy
                || vm.micro_network_id != expected.micro_network_id
                || vm.security_group_id != expected.security_group_id
                || vm.storage_root != creation.storage_root
                || !vm.env.is_empty()
                || vm.last_runtime_id.is_some()
            {
                return Err(conflict(
                    id,
                    "The reserved VM differs from its original creation request",
                ));
            }
            // Key creation precedes lease allocation in VM creation. If a lease
            // already exists, both steps completed; allocation itself is not idempotent.
            if state
                .store
                .active_lease(operation)
                .map_err(|_| AppError::internal(id.0))?
                .is_none()
            {
                handlers::vms::resume_cluster_create(&state, operation, id).await?;
            }
        } else {
            if state
                .store
                .canvas_vm_row_exists(operation)
                .map_err(|_| AppError::internal(id.0))?
            {
                return Err(conflict(
                    id,
                    "The reserved VM needs controller restart and reconciliation",
                ));
            }
            let mut vm_request = creation.request.vm.clone();
            vm_request.storage_root = Some(creation.storage_root.clone());
            handlers::vms::create_vm_from_template(
                state.clone(),
                id,
                vm_request,
                operation,
                Some(template),
            )
            .await?;
        }
        creation.phase = "created".into();
        state
            .store
            .save_canvas_vm_creation(&creation)
            .map_err(|_| AppError::internal(id.0))?;
    }
    if creation.phase != "created" {
        return Err(conflict(id, "Unknown Canvas VM creation phase"));
    }
    if !state
        .vms
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(&operation)
    {
        return Err(conflict(
            id,
            "The previously created VM is missing; it will not be recreated",
        ));
    }
    run(
        state,
        id,
        caller,
        operation,
        Request {
            vm_id: operation,
            name: creation.request.name,
            volume_size_gib: creation.request.volume_size_gib,
        },
    )
    .await
}

/// Cancel only while no VM or project has been created. A tombstone fences late requests.
async fn cancel_creation(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(operation): Path<Uuid>,
) -> Result<axum::http::StatusCode, AppError> {
    caller.operator().map_err(|_| denied(id.0))?;
    if operation.is_nil() {
        return Err(conflict(id, "Invalid creation identity"));
    }
    let _guard = VM_CREATION.lock().await;
    if let Some(record) = state
        .store
        .canvas_vm_creation(operation)
        .map_err(|_| AppError::internal(id.0))?
        && record.owner != caller.principal
    {
        return Err(denied(id.0));
    }
    if !state
        .store
        .retire_canvas_vm_creation(operation)
        .map_err(|_| AppError::internal(id.0))?
    {
        return Err(conflict(
            id,
            "A VM or project already exists; resume setup instead",
        ));
    }
    Ok(axum::http::StatusCode::NO_CONTENT)
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

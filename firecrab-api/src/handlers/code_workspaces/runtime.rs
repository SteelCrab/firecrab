use super::{conflict, database, operator};
use crate::{
    ai_sandbox::access::Caller,
    code_runtime,
    error::AppError,
    extract::ValidatedJson,
    persistence::code_runtime::{Admission, Record},
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use firecrab_api_types::*;
use uuid::Uuid;

pub fn routes() -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/api/code/workspaces/{id}/runtime",
            get(get_runtime).post(provision),
        )
        .route("/api/code/workspaces/{id}/runtime/start", post(start))
        .route("/api/code/workspaces/{id}/runtime/stop", post(stop))
        .route("/api/code/workspaces/{id}/runtime/detach", post(detach))
        .layer(axum::middleware::from_fn(
            crate::handlers::secrets::no_store,
        ))
}

fn view(state: &AppState, mut record: Record) -> CodeWorkspaceRuntime {
    let vm = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&record.runtime.vm_id)
        .cloned();
    record.runtime.vm_state = vm.as_ref().map(|vm| vm.state);
    if record.runtime.phase == CodeRuntimePhase::Ready
        && vm.as_ref().is_none_or(|vm| {
            vm.state != VmState::Running || vm.last_runtime_id != record.runtime.runtime_id
        })
    {
        record.runtime.phase = CodeRuntimePhase::ReconcileRequired;
    }
    record.runtime
}

fn validate_revision(revision: u64, id: RequestId) -> Result<(), AppError> {
    if revision == 0 || revision > i64::MAX as u64 {
        return Err(AppError::validation(
            [(
                "expectedRevision".into(),
                "use a positive saved workspace revision".into(),
            )]
            .into(),
            id.0,
        ));
    }
    Ok(())
}

async fn admit(
    state: &AppState,
    id: RequestId,
    key: Uuid,
    revision: u64,
    placement: Option<CodeRuntimePlacement>,
    action: CodeRuntimeAction,
) -> Result<(StatusCode, Json<CodeWorkspaceRuntime>), AppError> {
    validate_revision(revision, id)?;
    let store = state.store.clone();
    let record = match database(id, move || {
        store.admit_code_runtime(key, revision, placement, action)
    })
    .await?
    {
        Admission::Accepted(record) => *record,
        Admission::Missing => return Err(AppError::not_found(id.0)),
        Admission::Conflict => return Err(conflict(id)),
        Admission::Busy => {
            return Err(AppError::conflict(
                "code_runtime_busy",
                "Wait for the current lifecycle operation before requesting another action",
                id.0,
            ));
        }
        Admission::PlacementChanged => {
            return Err(AppError::conflict(
                "code_placement_bound",
                "Detach the stopped runtime before selecting different placement",
                id.0,
            ));
        }
    };
    code_runtime::dispatch(state.clone(), key, record.runtime.operation_id);
    Ok((StatusCode::ACCEPTED, Json(view(state, record))))
}

pub async fn get_runtime(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<String>,
) -> Result<Json<CodeWorkspaceRuntime>, AppError> {
    operator(caller, id)?;
    let key = crate::handlers::vms::parse_id(&key, id.0)?;
    let store = state.store.clone();
    let record = database(id, move || store.code_runtime(key))
        .await?
        .ok_or_else(|| AppError::not_found(id.0))?;
    Ok(Json(view(&state, record)))
}

pub async fn provision(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<String>,
    ValidatedJson(input): ValidatedJson<CodeProvisionRequest>,
) -> Result<(StatusCode, Json<CodeWorkspaceRuntime>), AppError> {
    operator(caller, id)?;
    validate_revision(input.expected_revision, id)?;
    let key = crate::handlers::vms::parse_id(&key, id.0)?;
    let store = state.store.clone();
    let workspace = database(id, move || store.code_workspace(key))
        .await?
        .ok_or_else(|| AppError::not_found(id.0))?;
    if workspace.revision != input.expected_revision {
        return Err(conflict(id));
    }
    let template = state
        .templates
        .resolve_alias(&input.template)
        .ok_or_else(|| {
            AppError::validation(
                [(
                    "template".into(),
                    "select an installed development image".into(),
                )]
                .into(),
                id.0,
            )
        })?;
    if input.template == crate::microboot::MICROBOOT_ALIAS {
        return Err(AppError::validation(
            [(
                "template".into(),
                "select an Ubuntu or Alpine development image".into(),
            )]
            .into(),
            id.0,
        ));
    }
    let placement = CodeRuntimePlacement {
        template: SandboxTemplatePin {
            name: template.name.clone(),
            version: template.version.clone(),
            kernel_sha256: template.kernel.sha256().into(),
            rootfs_sha256: template.rootfs.sha256().into(),
            boot_args_sha256: template.boot_args_sha256(),
            initrd_sha256: template.initrd.as_ref().map(|a| a.sha256().into()),
        },
        micro_network_id: input.micro_network_id,
        storage_root: input
            .storage_root
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| state.storage.default_id().into()),
    };
    let trial = Record {
        spec: workspace.spec,
        vm_created: false,
        runtime: CodeWorkspaceRuntime {
            workspace_id: key,
            vm_id: Uuid::nil(),
            configuration_revision: input.expected_revision,
            operation_id: Uuid::new_v4(),
            action: CodeRuntimeAction::Start,
            phase: CodeRuntimePhase::Queued,
            placement: placement.clone(),
            project_path: "/home/fc-code/project".into(),
            runtime_id: None,
            initial_commit: None,
            vm_state: None,
            error: None,
        },
    };
    let mut fields = crate::handlers::vms::validate_create_for(
        &crate::code_runtime::native_request(&trial),
        Some(&template),
        &state,
    );
    let network = state
        .store
        .micro_network(placement.micro_network_id)
        .map_err(|_| AppError::internal(id.0))?;
    if network.is_none_or(|network| !network.internet_enabled) {
        fields.insert(
            "microNetworkId".into(),
            "select an existing internet-enabled MicroNetwork".into(),
        );
    }
    if !fields.is_empty() {
        return Err(AppError::validation(fields, id.0));
    }
    admit(
        &state,
        id,
        key,
        input.expected_revision,
        Some(placement),
        CodeRuntimeAction::Start,
    )
    .await
}

pub async fn start(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<String>,
    ValidatedJson(input): ValidatedJson<CodeRuntimeRequest>,
) -> Result<(StatusCode, Json<CodeWorkspaceRuntime>), AppError> {
    operator(caller, id)?;
    let key = crate::handlers::vms::parse_id(&key, id.0)?;
    admit(
        &state,
        id,
        key,
        input.expected_revision,
        None,
        CodeRuntimeAction::Start,
    )
    .await
}

pub async fn stop(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<String>,
    ValidatedJson(input): ValidatedJson<CodeRuntimeRequest>,
) -> Result<(StatusCode, Json<CodeWorkspaceRuntime>), AppError> {
    operator(caller, id)?;
    let key = crate::handlers::vms::parse_id(&key, id.0)?;
    admit(
        &state,
        id,
        key,
        input.expected_revision,
        None,
        CodeRuntimeAction::Stop,
    )
    .await
}

/// Release the link only. A stopped VM and its disk remain visible in the normal VM inventory.
pub async fn detach(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<String>,
    ValidatedJson(input): ValidatedJson<CodeRuntimeRequest>,
) -> Result<Json<CodeWorkspaceRuntime>, AppError> {
    operator(caller, id)?;
    validate_revision(input.expected_revision, id)?;
    let key = crate::handlers::vms::parse_id(&key, id.0)?;
    let _claim = code_runtime::lock(&state, key).map_err(|_| {
        AppError::conflict(
            "code_runtime_busy",
            "Wait for the current operation before detaching",
            id.0,
        )
    })?;
    let store = state.store.clone();
    let record = database(id, move || store.code_runtime(key))
        .await?
        .ok_or_else(|| AppError::not_found(id.0))?;
    if state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&record.runtime.vm_id)
        .is_some_and(|vm| {
            !matches!(
                vm.state,
                VmState::Created | VmState::Stopped | VmState::Error
            )
        })
    {
        return Err(AppError::conflict(
            "code_runtime_running",
            "Stop the runtime before detaching its retained VM",
            id.0,
        ));
    }
    let store = state.store.clone();
    if !database(id, move || {
        store.detach_code_runtime(key, input.expected_revision)
    })
    .await?
    {
        return Err(conflict(id));
    }
    Ok(Json(view(&state, record)))
}

//! Project-scoped environment lifecycle with controller-only resource mutations.
use crate::{
    ai_sandbox::{access::Caller, environment},
    error::AppError,
    extract::ValidatedJson,
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use firecrab_api_types::*;
use std::collections::BTreeMap;
use uuid::Uuid;

/// Keep environment route wiring independent of task/preview endpoint modules.
pub fn routes() -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/api/ai-sandbox/projects/{project}/environment-images",
            get(images),
        )
        .route("/api/ai-sandbox/projects/{project}/environments", get(list))
        .route(
            "/api/ai-sandbox/projects/{project}/environments/{environment}",
            get(self::get).put(put).delete(delete),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/environments/{environment}/reconcile",
            post(reconcile),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/environments/{environment}/stop",
            post(stop),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/environments/{environment}/resume",
            post(resume),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/environments/{environment}/export",
            post(export),
        )
}

fn error(reason: String, request: Uuid) -> AppError {
    AppError::validation(BTreeMap::from([("environment".into(), reason)]), request)
}
fn id(value: &str, request: Uuid) -> Result<Uuid, AppError> {
    super::vms::parse_id(value, request)
}
fn project(
    state: &AppState,
    caller: Caller,
    project: Uuid,
    permission: SandboxPermission,
    request: Uuid,
) -> Result<(), AppError> {
    caller
        .project(state, project, permission)
        .map_err(|reason| error(reason, request))?;
    if state
        .store
        .sandbox_project(project)
        .map_err(|_| AppError::internal(request))?
        .is_none()
    {
        return Err(AppError::not_found(request));
    }
    Ok(())
}
fn owned(
    state: &AppState,
    project: Uuid,
    environment: Uuid,
    request: Uuid,
) -> Result<SandboxEnvironment, AppError> {
    let saved = state
        .store
        .sandbox_environment(environment)
        .map_err(|_| AppError::internal(request))?
        .ok_or_else(|| AppError::not_found(request))?;
    if saved.project_id != project {
        return Err(AppError::not_found(request));
    }
    Ok(saved)
}
fn mutation(
    state: &AppState,
    caller: Caller,
    project_id: Uuid,
    environment_id: Uuid,
    request: Uuid,
) -> Result<SandboxEnvironment, AppError> {
    caller.operator().map_err(|reason| error(reason, request))?;
    project(
        state,
        caller,
        project_id,
        SandboxPermission::Execute,
        request,
    )?;
    owned(state, project_id, environment_id, request)
}

pub async fn list(
    State(state): State<AppState>,
    caller: Caller,
    Extension(request): Extension<RequestId>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<SandboxEnvironment>>, AppError> {
    let project_id = id(&project_id, request.0)?;
    project(
        &state,
        caller,
        project_id,
        SandboxPermission::Read,
        request.0,
    )?;
    state
        .store
        .sandbox_environments(project_id)
        .map(Json)
        .map_err(|_| AppError::internal(request.0))
}

/// Return immutable content identities so a client never reconstructs a template pin.
pub async fn images(
    State(state): State<AppState>,
    caller: Caller,
    Extension(request): Extension<RequestId>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<SandboxTemplatePin>>, AppError> {
    project(
        &state,
        caller,
        id(&project_id, request.0)?,
        SandboxPermission::Read,
        request.0,
    )?;
    Ok(Json(
        state
            .templates
            .list_aliases()
            .iter()
            .map(|template| SandboxTemplatePin {
                name: template.name.clone(),
                version: template.version.clone(),
                kernel_sha256: template.kernel.sha256().into(),
                rootfs_sha256: template.rootfs.sha256().into(),
                initrd_sha256: template.initrd.as_ref().map(|a| a.sha256().into()),
                boot_args_sha256: template.boot_args_sha256(),
            })
            .collect(),
    ))
}

pub async fn get(
    State(state): State<AppState>,
    caller: Caller,
    Extension(request): Extension<RequestId>,
    Path((project_id, environment_id)): Path<(String, String)>,
) -> Result<Json<SandboxEnvironment>, AppError> {
    let project_id = id(&project_id, request.0)?;
    project(
        &state,
        caller,
        project_id,
        SandboxPermission::Read,
        request.0,
    )?;
    owned(
        &state,
        project_id,
        id(&environment_id, request.0)?,
        request.0,
    )
    .map(Json)
}

/// Repeated PUT preserves the generation and resource IDs. A changed manifest is a conflict.
pub async fn put(
    State(state): State<AppState>,
    caller: Caller,
    Extension(request): Extension<RequestId>,
    Path((project_id, environment_id)): Path<(String, String)>,
    ValidatedJson(spec): ValidatedJson<SandboxEnvironmentSpec>,
) -> Result<(StatusCode, Json<SandboxEnvironment>), AppError> {
    caller
        .operator()
        .map_err(|reason| error(reason, request.0))?;
    let project_id = id(&project_id, request.0)?;
    let environment_id = id(&environment_id, request.0)?;
    project(
        &state,
        caller,
        project_id,
        SandboxPermission::Write,
        request.0,
    )?;
    let _guard = state.lock_vm(environment_id).await;
    if let Some(saved) = state
        .store
        .sandbox_environment(environment_id)
        .map_err(|_| AppError::internal(request.0))?
    {
        if saved.project_id != project_id || saved.spec != spec {
            return Err(AppError::conflict(
                "sandbox_environment_immutable",
                "Environment identity and manifest are immutable; use a new environment UUID",
                request.0,
            ));
        }
        return Ok((StatusCode::OK, Json(saved)));
    }
    let saved = environment::reservation(environment_id, project_id, caller, spec)
        .map_err(|reason| error(reason, request.0))?;
    state
        .store
        .reserve_sandbox_environment(&saved)
        .map_err(|_| AppError::internal(request.0))?;
    let runtime = state.clone();
    tokio::spawn(async move {
        let _ = environment::reconcile(&runtime, environment_id).await;
    });
    Ok((StatusCode::ACCEPTED, Json(saved)))
}

pub async fn reconcile(
    State(state): State<AppState>,
    caller: Caller,
    Extension(request): Extension<RequestId>,
    Path((project_id, environment_id)): Path<(String, String)>,
) -> Result<Json<SandboxEnvironment>, AppError> {
    let environment_id = id(&environment_id, request.0)?;
    mutation(
        &state,
        caller,
        id(&project_id, request.0)?,
        environment_id,
        request.0,
    )?;
    tokio::spawn(async move { environment::reconcile(&state, environment_id).await })
        .await
        .map_err(|_| AppError::internal(request.0))?
        .map(Json)
        .map_err(|reason| error(reason, request.0))
}

async fn running(
    state: AppState,
    caller: Caller,
    request: RequestId,
    project_id: String,
    environment_id: String,
    running: bool,
) -> Result<Json<SandboxEnvironment>, AppError> {
    let environment_id = id(&environment_id, request.0)?;
    mutation(
        &state,
        caller,
        id(&project_id, request.0)?,
        environment_id,
        request.0,
    )?;
    tokio::spawn(async move { environment::set_running(&state, environment_id, running).await })
        .await
        .map_err(|_| AppError::internal(request.0))?
        .map(Json)
        .map_err(|reason| error(reason, request.0))
}
pub async fn stop(
    State(state): State<AppState>,
    caller: Caller,
    Extension(request): Extension<RequestId>,
    Path((project_id, environment_id)): Path<(String, String)>,
) -> Result<Json<SandboxEnvironment>, AppError> {
    running(state, caller, request, project_id, environment_id, false).await
}
pub async fn resume(
    State(state): State<AppState>,
    caller: Caller,
    Extension(request): Extension<RequestId>,
    Path((project_id, environment_id)): Path<(String, String)>,
) -> Result<Json<SandboxEnvironment>, AppError> {
    running(state, caller, request, project_id, environment_id, true).await
}
pub async fn export(
    State(state): State<AppState>,
    caller: Caller,
    Extension(request): Extension<RequestId>,
    Path((project_id, environment_id)): Path<(String, String)>,
) -> Result<Json<SandboxEnvironmentExport>, AppError> {
    let environment_id = id(&environment_id, request.0)?;
    mutation(
        &state,
        caller,
        id(&project_id, request.0)?,
        environment_id,
        request.0,
    )?;
    environment::export(&state, environment_id)
        .await
        .map(Json)
        .map_err(|reason| error(reason, request.0))
}
pub async fn delete(
    State(state): State<AppState>,
    caller: Caller,
    Extension(request): Extension<RequestId>,
    Path((project_id, environment_id)): Path<(String, String)>,
    ValidatedJson(body): ValidatedJson<SandboxEnvironmentDelete>,
) -> Result<Json<SandboxEnvironment>, AppError> {
    let environment_id = id(&environment_id, request.0)?;
    mutation(
        &state,
        caller,
        id(&project_id, request.0)?,
        environment_id,
        request.0,
    )?;
    tokio::spawn(async move {
        environment::cleanup(&state, environment_id, body.authorize_workspace_deletion).await
    })
    .await
    .map_err(|_| AppError::internal(request.0))?
    .map(Json)
    .map_err(|reason| error(reason, request.0))
}

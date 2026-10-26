//! Versioned guest controls always resolve an already-admitted project/task/runtime.
use crate::{
    ai_sandbox::{SavedTask, access::Caller, guest},
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
use uuid::Uuid;

pub fn routes() -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/api/ai-sandbox/projects/{project}/tasks/{task}/guest/health",
            get(health),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/tasks/{task}/guest/processes",
            get(processes),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/tasks/{task}/guest/signal",
            post(signal),
        )
}

fn task(
    state: &AppState,
    caller: Caller,
    project: &str,
    task: &str,
    request: Uuid,
    permission: SandboxPermission,
) -> Result<SavedTask, AppError> {
    let project = super::vms::parse_id(project, request)?;
    let task = super::vms::parse_id(task, request)?;
    caller.project(state, project, permission).map_err(|_| {
        AppError::new(
            StatusCode::FORBIDDEN,
            "sandbox_permission_denied",
            "Sandbox project permission denied",
            request,
        )
    })?;
    let saved = state
        .store
        .sandbox_task(task)
        .map_err(|_| AppError::internal(request))?
        .ok_or_else(|| AppError::not_found(request))?;
    if saved.task.project_id != project {
        return Err(AppError::not_found(request));
    }
    caller.task(state, &saved, permission).map_err(|_| {
        AppError::new(
            StatusCode::FORBIDDEN,
            "sandbox_permission_denied",
            "Sandbox task resource permission denied",
            request,
        )
    })?;
    crate::ai_sandbox::access::attempt(state, &saved).map_err(|_| {
        AppError::conflict(
            "sandbox_guest_generation_changed",
            "Admitted guest runtime or resource authority changed; reconcile the attempt",
            request,
        )
    })?;
    Ok(saved)
}

pub async fn health(
    State(state): State<AppState>,
    caller: Caller,
    Extension(request): Extension<RequestId>,
    Path((project_id, task_id)): Path<(String, String)>,
) -> Result<Json<SandboxGuestCapabilities>, AppError> {
    let saved = task(
        &state,
        caller,
        &project_id,
        &task_id,
        request.0,
        SandboxPermission::Read,
    )?;
    let runtime = saved.guest.ok_or_else(|| {
        AppError::conflict(
            "sandbox_guest_unavailable",
            "This task has no admitted guest runtime",
            request.0,
        )
    })?;
    let directory = saved.workspace.ok_or_else(|| {
        AppError::conflict(
            "sandbox_guest_unavailable",
            "This task has no retained attempt workspace",
            request.0,
        )
    })?;
    guest::capabilities(&state, &runtime, &directory)
        .await
        .map(Json)
        .map_err(|_| {
            AppError::new(
                StatusCode::NOT_IMPLEMENTED,
                "sandbox_guest_protocol_unsupported",
                "The admitted guest does not provide a compatible authenticated V1 channel",
                request.0,
            )
        })
}

pub async fn processes(
    State(state): State<AppState>,
    caller: Caller,
    Extension(request): Extension<RequestId>,
    Path((project_id, task_id)): Path<(String, String)>,
) -> Result<Json<Vec<SandboxGuestProcess>>, AppError> {
    let saved = task(
        &state,
        caller,
        &project_id,
        &task_id,
        request.0,
        SandboxPermission::Read,
    )?;
    guest::processes(&state, &saved)
        .await
        .map(Json)
        .map_err(|_| {
            AppError::conflict(
                "sandbox_guest_inspection_failed",
                "Cannot inspect the admitted guest process generation",
                request.0,
            )
        })
}

pub async fn signal(
    State(state): State<AppState>,
    caller: Caller,
    Extension(request): Extension<RequestId>,
    Path((project_id, task_id)): Path<(String, String)>,
    ValidatedJson(body): ValidatedJson<SandboxGuestSignalRequest>,
) -> Result<StatusCode, AppError> {
    let saved = task(
        &state,
        caller,
        &project_id,
        &task_id,
        request.0,
        SandboxPermission::Execute,
    )?;
    guest::signal(&state, &saved, &body).await.map_err(|_| {
        AppError::conflict(
            "sandbox_guest_signal_refused",
            "Guest boot/PID/start-time or ownership changed; signal was refused",
            request.0,
        )
    })?;
    Ok(StatusCode::NO_CONTENT)
}

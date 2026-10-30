//! Operator-only configuration CRUD. Saving a workspace never starts tools or contacts providers.
use crate::{
    ai_sandbox::access::Caller,
    error::AppError,
    extract::ValidatedJson,
    persistence::{
        PersistenceError,
        code_workspaces::{DeleteResult, WriteResult},
    },
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json,
    extract::{Path, Query, State, rejection::QueryRejection},
    http::StatusCode,
};
use firecrab_api_types::{CodeWorkspace, CodeWorkspaceDelete, CodeWorkspaceWrite};
mod verification;
pub use verification::verify;

fn operator(caller: Caller, id: RequestId) -> Result<(), AppError> {
    caller.operator().map_err(|_| {
        AppError::new(
            StatusCode::FORBIDDEN,
            "code_operator_required",
            "Only the controller operator can manage CODE workspaces",
            id.0,
        )
    })
}

async fn database<T: Send + 'static>(
    id: RequestId,
    operation: impl FnOnce() -> Result<T, PersistenceError> + Send + 'static,
) -> Result<T, AppError> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| AppError::internal(id.0))?
        .map_err(|_| AppError::internal(id.0))
}

fn conflict(id: RequestId) -> AppError {
    AppError::conflict(
        "code_workspace_revision_conflict",
        "Workspace changed; read its current revision before retrying",
        id.0,
    )
}

pub async fn list(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
) -> Result<Json<Vec<CodeWorkspace>>, AppError> {
    operator(caller, id)?;
    database(id, move || state.store.code_workspaces())
        .await
        .map(Json)
}

pub async fn get(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<String>,
) -> Result<Json<CodeWorkspace>, AppError> {
    operator(caller, id)?;
    let key = crate::handlers::vms::parse_id(&key, id.0)?;
    database(id, move || state.store.code_workspace(key))
        .await?
        .map(Json)
        .ok_or_else(|| AppError::not_found(id.0))
}

pub async fn save(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<String>,
    ValidatedJson(input): ValidatedJson<CodeWorkspaceWrite>,
) -> Result<Json<CodeWorkspace>, AppError> {
    operator(caller, id)?;
    let key = crate::handlers::vms::parse_id(&key, id.0)?;
    let now = chrono::Utc::now().timestamp_millis();
    match database(id, move || {
        state.store.write_code_workspace(key, input, now)
    })
    .await?
    {
        WriteResult::Written(workspace) => Ok(Json(*workspace)),
        WriteResult::Conflict => Err(conflict(id)),
        WriteResult::NameTaken => Err(AppError::conflict(
            "code_workspace_name_taken",
            "Workspace name is already in use",
            id.0,
        )),
        WriteResult::Invalid(fields) => Err(AppError::validation(fields, id.0)),
    }
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<String>,
    query: Result<Query<CodeWorkspaceDelete>, QueryRejection>,
) -> Result<StatusCode, AppError> {
    operator(caller, id)?;
    let key = crate::handlers::vms::parse_id(&key, id.0)?;
    let Query(input) = query.map_err(|_| {
        AppError::validation(
            [(
                "expectedRevision".into(),
                "supply only the saved revision as a positive integer".into(),
            )]
            .into(),
            id.0,
        )
    })?;
    if input.expected_revision == 0 || input.expected_revision > i64::MAX as u64 {
        return Err(AppError::validation(
            [(
                "expectedRevision".into(),
                "use a positive saved workspace revision".into(),
            )]
            .into(),
            id.0,
        ));
    }
    match database(id, move || {
        state
            .store
            .delete_code_workspace(key, input.expected_revision)
    })
    .await?
    {
        DeleteResult::Deleted => Ok(StatusCode::NO_CONTENT),
        DeleteResult::Missing => Err(AppError::not_found(id.0)),
        DeleteResult::Conflict => Err(conflict(id)),
    }
}

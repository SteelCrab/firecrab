//! Git source configuration and detect-only check endpoints.

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use firecrab_stack::{ConfigureGitSource, GitCheck, GitSourceGeneration, GitSourceStatus};

use super::stacks::{database, validate_name};
use crate::{error::AppError, extract::ValidatedJson, server::RequestId, state::AppState};

/// Configure a source with optimistic generation checks. Does not contact the repository.
pub async fn configure(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(name): Path<String>,
    ValidatedJson(request): ValidatedJson<ConfigureGitSource>,
) -> Result<Json<GitSourceStatus>, AppError> {
    validate_name(&name, id.0)?;
    request.source.validate().map_err(|message| {
        AppError::validation(
            std::collections::BTreeMap::from([("source".into(), message.into())]),
            id.0,
        )
    })?;
    database(id.0, move || {
        state
            .store
            .configure_stack_source(&name, &request, crate::stack_source::now())
    })
    .await
    .map(Json)
}

/// Read configuration and the latest durable observation. Never reads token values.
pub async fn get(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(name): Path<String>,
) -> Result<Json<GitSourceStatus>, AppError> {
    validate_name(&name, id.0)?;
    database(id.0, move || state.store.stack_source(&name))
        .await
        .map(Json)
}

/// Return the latest fifty checks, including earlier configuration generations.
pub async fn history(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(name): Path<String>,
) -> Result<Json<Vec<GitCheck>>, AppError> {
    validate_name(&name, id.0)?;
    database(id.0, move || state.store.stack_check_history(&name))
        .await
        .map(Json)
}

/// Explicit read-only check, also permitted while periodic polling is paused.
pub async fn check(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(name): Path<String>,
    ValidatedJson(request): ValidatedJson<GitSourceGeneration>,
) -> Result<(StatusCode, Json<GitSourceStatus>), AppError> {
    validate_name(&name, id.0)?;
    // Acceptance and worker creation survive a dropped HTTP acknowledgement.
    let status = tokio::spawn(crate::stack_source::start_check(
        state,
        name,
        request.generation,
        false,
    ))
    .await
    .map_err(|_| AppError::internal(id.0))??
    .ok_or_else(|| AppError::internal(id.0))?;
    Ok((StatusCode::ACCEPTED, Json(status)))
}

#[cfg(test)]
mod tests;

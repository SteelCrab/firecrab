//! Private service-account binding endpoints with retryable setup.
use super::{
    micro_services::database,
    pipe_databases::{bounded_write, load},
};
use crate::{
    error::AppError,
    extract::ValidatedJson,
    pipe_databases::bindings::{self, BindingInput, BindingRecord, BindingSummary},
    server::RequestId,
    state::AppState,
};
use axum::{
    Json,
    extract::{Extension, Path, State},
};

/// List public service bindings for one saved database.
pub async fn list(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<Vec<BindingSummary>>, AppError> {
    let record = load(state.clone(), rid.0, id).await?;
    let bindings = database(rid.0, move || state.store.database_bindings()).await?;
    Ok(Json(
        bindings
            .into_iter()
            .filter(|b| b.database_id == record.id)
            .map(|b| b.summary())
            .collect(),
    ))
}
/// Persist binding intent before account setup and allow identical retries.
pub async fn create(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(input): ValidatedJson<BindingInput>,
) -> Result<Json<BindingSummary>, AppError> {
    let record = load(state.clone(), rid.0, id).await?;
    let _claim = state.service_mutations.lock().await;
    super::pipe_databases::available(&state, record.id, rid.0)?;
    crate::service_runtime::ensure_editable(&state, input.service_id, rid.0).await?;
    let service = state
        .store
        .micro_service(input.service_id)
        .map_err(|_| AppError::internal(rid.0))?
        .ok_or_else(|| AppError::not_found(rid.0))?;
    if service.spec.env.contains_key(&input.env_name)
        || service.spec.secrets.contains_key(&input.env_name)
    {
        return Err(crate::pipe_databases::validation(
            "Remove the environment variable with the same name before binding the database",
            rid.0,
        ));
    }
    let bindings = state
        .store
        .database_bindings()
        .map_err(|_| AppError::internal(rid.0))?;
    let mut binding = if let Some(existing) = bindings.iter().find(|b| b.id == input.request_id) {
        if existing.database_id != record.id
            || serde_json::to_value(&existing.input).ok() != serde_json::to_value(&input).ok()
        {
            return Err(AppError::new(
                axum::http::StatusCode::CONFLICT,
                "request_conflict",
                "The request ID belongs to another binding",
                rid.0,
            ));
        }
        if existing.ready {
            return Ok(Json(existing.summary()));
        }
        existing.clone()
    } else {
        if bindings
            .iter()
            .any(|b| b.input.service_id == input.service_id && b.input.env_name == input.env_name)
        {
            return Err(crate::pipe_databases::validation(
                "This environment variable already has a database binding",
                rid.0,
            ));
        }
        bounded_write(rid.0, BindingRecord::new(&record, input)).await?
    };
    state
        .store
        .save_database_binding(&binding)
        .map_err(|_| AppError::internal(rid.0))?;
    bounded_write(rid.0, bindings::provision(&record, &binding)).await?;
    binding.ready = true;
    state
        .store
        .save_database_binding(&binding)
        .map_err(|_| AppError::internal(rid.0))?;
    Ok(Json(binding.summary()))
}
/// Require a stopped service before revoking its generated database account.
pub async fn delete(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let id = super::vms::parse_id(&id, rid.0)?;
    let binding = state
        .store
        .database_bindings()
        .map_err(|_| AppError::internal(rid.0))?
        .into_iter()
        .find(|b| b.id == id)
        .ok_or_else(|| AppError::not_found(rid.0))?;
    let _claim = state.service_mutations.lock().await;
    crate::service_runtime::ensure_editable(&state, binding.input.service_id, rid.0).await?;
    super::pipe_databases::available(&state, binding.database_id, rid.0)?;
    let record = load(state.clone(), rid.0, binding.database_id.to_string()).await?;
    bounded_write(rid.0, bindings::revoke(&record, &binding)).await?;
    state
        .store
        .delete_database_binding(id)
        .map_err(|_| AppError::internal(rid.0))?;
    Ok(Json(serde_json::json!({"deleted":true})))
}

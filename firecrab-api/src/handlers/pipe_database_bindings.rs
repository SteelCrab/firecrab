//! Private service-account binding endpoints with retryable setup.
use super::{micro_services::database, pipe_databases::load};
use crate::{
    error::AppError, pipe_databases::bindings::BindingSummary, server::RequestId, state::AppState,
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
pub use super::pipe_database_binding_mutations::{create, delete};

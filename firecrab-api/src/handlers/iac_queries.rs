//! Bounded observations of unfinished native work.
use super::stacks::database;
use crate::{error::AppError, server::RequestId, state::AppState};
use axum::{Extension, Json, extract::State};

/// Stable pagination prevents an active cleanup from being hidden by completed history.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeQuery {
    before: Option<uuid::Uuid>,
    limit: Option<u16>,
}

pub async fn native_operations(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    axum::extract::Query(query): axum::extract::Query<NativeQuery>,
) -> Result<Json<firecrab_stack::NativeOperationsPage>, AppError> {
    let limit = query.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) || query.before.is_some_and(|id| id.is_nil()) {
        return Err(AppError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "invalid_native_query",
            "use a limit from 1 to 100 and a non-nil operation cursor",
            request.0,
        ));
    }
    database(request.0, move || {
        state.store.active_native_operations(query.before, limit)
    })
    .await
    .map(Json)
}

//! PIPE database registration and read-only inspection.

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use std::future::Future;
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::handlers::micro_services::database;
use crate::handlers::vms::parse_id;
use crate::pipe_databases::{
    self, DatabaseConnection, DatabaseInput, DatabaseRecord, DatabaseSummary, OPERATION_TIMEOUT,
    Preview, PreviewRequest, Tables,
};
use crate::server::RequestId;
use crate::state::AppState;

/// Do not forward driver errors: servers can include credentials or query text.
async fn bounded<T>(
    request_id: Uuid,
    operation: impl Future<Output = Result<T, sqlx::Error>>,
) -> Result<T, AppError> {
    match tokio::time::timeout(OPERATION_TIMEOUT, operation).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(sqlx::Error::RowNotFound)) => Err(AppError::not_found(request_id)),
        Ok(Err(sqlx::Error::Protocol(message))) if message == "too_many_columns" => {
            Err(pipe_databases::validation(
                "Preview supports tables with at most 128 columns",
                request_id,
            ))
        }
        Ok(Err(_)) => Err(AppError::new(
            StatusCode::BAD_GATEWAY,
            "database_connection_failed",
            "Cannot read the database; check the URI, credentials, TLS certificate, network access and SELECT permissions",
            request_id,
        )),
        Err(_) => Err(AppError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "database_timeout",
            "Database operation timed out; check the host and firewall rules",
            request_id,
        )),
    }
}

/// List saved connection metadata without returning credentials or CA bundles.
pub async fn list(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
) -> Result<Json<Vec<DatabaseSummary>>, AppError> {
    let records = database(request_id.0, move || state.store.list_pipe_databases()).await?;
    records
        .into_iter()
        .map(|record| {
            record
                .input
                .settings(record.id)
                .map(|settings| settings.summary)
                .map_err(|_| AppError::internal(request_id.0))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Json)
}

/// Validate and authenticate before persisting a connection.
pub async fn create(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    ValidatedJson(input): ValidatedJson<DatabaseInput>,
) -> Result<(StatusCode, Json<DatabaseSummary>), AppError> {
    let id = Uuid::new_v4();
    let settings = input
        .settings(id)
        .map_err(|message| pipe_databases::validation(message, request_id.0))?;
    bounded(request_id.0, async {
        DatabaseConnection::connect(&input, &settings)
            .await?
            .ping()
            .await
    })
    .await?;
    database(request_id.0, move || {
        state.store.save_pipe_database(id, &input)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(settings.summary)))
}

/// Authenticate an unsaved connection without writing a persistence record.
pub async fn test(
    Extension(request_id): Extension<RequestId>,
    ValidatedJson(input): ValidatedJson<DatabaseInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let settings = input
        .settings(Uuid::nil())
        .map_err(|message| pipe_databases::validation(message, request_id.0))?;
    bounded(request_id.0, async {
        DatabaseConnection::connect(&input, &settings)
            .await?
            .ping()
            .await
    })
    .await?;
    Ok(Json(
        serde_json::json!({ "connected": true, "engine": settings.summary.engine }),
    ))
}

/// Validate a route identifier and load its saved input, or return HTTP 404.
async fn load(state: AppState, request_id: Uuid, id: String) -> Result<DatabaseRecord, AppError> {
    let id = parse_id(&id, request_id)?;
    database(request_id, move || state.store.pipe_database(id))
        .await?
        .ok_or_else(|| AppError::not_found(request_id))
}

/// Open a saved connection and list its accessible base tables.
pub async fn tables(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<Tables>, AppError> {
    let record = load(state, request_id.0, id).await?;
    let settings = record
        .input
        .settings(record.id)
        .map_err(|_| AppError::internal(request_id.0))?;
    bounded(request_id.0, async {
        DatabaseConnection::connect(&record.input, &settings)
            .await?
            .tables()
            .await
    })
    .await
    .map(Json)
}

/// Validate pagination and return one bounded page from a selected table.
pub async fn preview(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(request): ValidatedJson<PreviewRequest>,
) -> Result<Json<Preview>, AppError> {
    request
        .validate()
        .map_err(|message| pipe_databases::validation(message, request_id.0))?;
    let record = load(state, request_id.0, id).await?;
    let settings = record
        .input
        .settings(record.id)
        .map_err(|_| AppError::internal(request_id.0))?;
    bounded(request_id.0, async {
        DatabaseConnection::connect(&record.input, &settings)
            .await?
            .preview(&request)
            .await
    })
    .await
    .map(Json)
}

/// Delete saved connection input without connecting to or changing the source DB.
pub async fn delete(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let id = parse_id(&id, request_id.0)?;
    if !database(request_id.0, move || state.store.delete_pipe_database(id)).await? {
        return Err(AppError::not_found(request_id.0));
    }
    Ok(Json(serde_json::json!({ "deleted": true })))
}

#[cfg(test)]
#[path = "pipe_databases_tests.rs"]
mod tests;

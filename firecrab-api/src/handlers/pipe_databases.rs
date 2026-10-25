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
use crate::pipe_databases::write;
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
    if input.write_enabled {
        bounded_write(request_id.0, async {
            let mut connection = DatabaseConnection::connect_write(&input, &settings).await?;
            write::initialize_saved(
                &mut connection,
                &DatabaseRecord {
                    id,
                    input: input.clone(),
                },
                &state.store,
            )
            .await
        })
        .await?;
    } else {
        bounded(request_id.0, async {
            DatabaseConnection::connect(&input, &settings)
                .await?
                .ping()
                .await
        })
        .await?;
    }
    database(request_id.0, move || {
        state.store.save_pipe_database(id, &input)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(settings.summary)))
}

/// Replace credentials without returning their previous values.
pub async fn update(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(mut input): ValidatedJson<DatabaseInput>,
) -> Result<Json<DatabaseSummary>, AppError> {
    let _claim = state.service_mutations.lock().await;
    let record = load(state.clone(), rid.0, id).await?;
    if input
        .ca_certificate
        .as_ref()
        .is_none_or(|c| c.trim().is_empty())
    {
        input.ca_certificate = record.input.ca_certificate.clone();
    }
    if state
        .store
        .database_target_jobs(record.id)
        .map_err(|_| AppError::internal(rid.0))?
        .iter()
        .any(|j| matches!(j.status.as_str(), "queued" | "running" | "needs_recovery"))
    {
        return Err(AppError::conflict(
            "database_job_in_use",
            "Finish database jobs before updating credentials",
            rid.0,
        ));
    }
    let settings = input
        .settings(record.id)
        .map_err(|m| pipe_databases::validation(m, rid.0))?;
    let previous = record
        .input
        .settings(record.id)
        .map_err(|_| AppError::internal(rid.0))?;
    let bindings = state
        .store
        .database_bindings()
        .map_err(|_| AppError::internal(rid.0))?
        .into_iter()
        .filter(|b| b.database_id == record.id)
        .collect::<Vec<_>>();
    let triggers = state
        .store
        .database_triggers()
        .map_err(|_| AppError::internal(rid.0))?
        .into_iter()
        .filter(|t| t.database_id == record.id)
        .collect::<Vec<_>>();
    let referenced = !bindings.is_empty() || !triggers.is_empty();
    if referenced
        && (settings.summary.host != previous.summary.host
            || settings.summary.port != previous.summary.port
            || settings.summary.database != previous.summary.database
            || settings.summary.engine != previous.summary.engine
            || input.write_enabled != record.input.write_enabled)
    {
        return Err(pipe_databases::validation(
            "Remove service bindings and triggers before changing the database target or write access",
            rid.0,
        ));
    }
    if !bindings.is_empty()
        && (input.ca_certificate != record.input.ca_certificate
            || settings.summary.tls_mode != previous.summary.tls_mode)
    {
        return Err(pipe_databases::validation(
            "Remove bindings before changing their TLS trust; deploy new bindings with the updated certificate",
            rid.0,
        ));
    }
    if input.write_enabled {
        bounded_write(rid.0, async {
            let mut c = DatabaseConnection::connect_write(&input, &settings).await?;
            write::initialize_saved(
                &mut c,
                &DatabaseRecord {
                    id: record.id,
                    input: input.clone(),
                },
                &state.store,
            )
            .await
        })
        .await?;
    } else {
        bounded(rid.0, async {
            DatabaseConnection::connect(&input, &settings)
                .await?
                .ping()
                .await
        })
        .await?;
    }
    database(rid.0, move || {
        state.store.update_pipe_database(record.id, &input)
    })
    .await?;
    Ok(Json(settings.summary))
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
pub(super) async fn load(
    state: AppState,
    request_id: Uuid,
    id: String,
) -> Result<DatabaseRecord, AppError> {
    let id = parse_id(&id, request_id)?;
    database(request_id, move || state.store.pipe_database(id))
        .await?
        .ok_or_else(|| AppError::not_found(request_id))
}

pub(crate) fn available(state: &AppState, id: Uuid, rid: Uuid) -> Result<(), AppError> {
    if state
        .store
        .database_maintenance_active(id)
        .map_err(|_| AppError::internal(rid))?
    {
        return Err(AppError::conflict(
            "database_maintenance",
            "Database maintenance is active; wait for the job or restore its safety backup",
            rid,
        ));
    }
    Ok(())
}

pub(super) async fn bounded_write<T>(
    request_id: Uuid,
    operation: impl Future<Output = Result<T, write::Error>> + Send,
) -> Result<T, AppError> {
    let (status, code, message) = match tokio::time::timeout(OPERATION_TIMEOUT, operation).await {
        Ok(Ok(value)) => return Ok(value),
        Ok(Err(write::Error::Invalid(message))) => {
            (StatusCode::BAD_REQUEST, "database_validation", message)
        }
        Ok(Err(write::Error::NotFound)) => (
            StatusCode::NOT_FOUND,
            "not_found",
            "The table or row no longer exists",
        ),
        Ok(Err(write::Error::Conflict)) => (
            StatusCode::CONFLICT,
            "row_conflict",
            "The row changed; refresh it before saving",
        ),
        Ok(Err(write::Error::RequestConflict)) => (
            StatusCode::CONFLICT,
            "request_conflict",
            "The request ID was already used for another operation",
        ),
        Ok(Err(write::Error::Database(sqlx::Error::Database(error))))
            if error.is_unique_violation()
                || error.is_foreign_key_violation()
                || error.is_check_violation() =>
        {
            (
                StatusCode::CONFLICT,
                "database_constraint",
                "The change violates a unique key, relation or validation rule",
            )
        }
        Ok(Err(write::Error::Database(sqlx::Error::Database(error))))
            if error
                .code()
                .is_some_and(|code| code.starts_with("22") || code == "23502")
                || error
                    .try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()
                    .is_some_and(|error| {
                        matches!(error.number(), 1048 | 1264 | 1292 | 1366 | 1406)
                    }) =>
        {
            (
                StatusCode::BAD_REQUEST,
                "database_field_value",
                "A field has an invalid value, length or required NULL; correct it before saving",
            )
        }
        Ok(Err(_)) => (
            StatusCode::BAD_GATEWAY,
            "database_operation_failed",
            "Cannot complete the database operation; check DB permissions, field types and connectivity",
        ),
        Err(_) => (
            StatusCode::GATEWAY_TIMEOUT,
            "database_timeout",
            "The database operation timed out; retry with the same request ID",
        ),
    };
    Err(AppError::new(status, code, message, request_id))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessInput {
    write_enabled: bool,
}

pub async fn access(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(input): ValidatedJson<AccessInput>,
) -> Result<Json<DatabaseSummary>, AppError> {
    let _claim = state.service_mutations.lock().await;
    let mut record = load(state.clone(), request_id.0, id).await?;
    available(&state, record.id, request_id.0)?;
    if !input.write_enabled
        && (state
            .store
            .database_bindings()
            .map_err(|_| AppError::internal(request_id.0))?
            .iter()
            .any(|b| {
                b.database_id == record.id
                    && b.input.access == crate::pipe_databases::bindings::Access::Write
            })
            || state
                .store
                .database_triggers()
                .map_err(|_| AppError::internal(request_id.0))?
                .iter()
                .any(|t| t.database_id == record.id))
    {
        return Err(pipe_databases::validation(
            "Remove write bindings and finish retained triggers before disabling writes",
            request_id.0,
        ));
    }
    record.input.write_enabled = input.write_enabled;
    let settings = record
        .input
        .settings(record.id)
        .map_err(|message| pipe_databases::validation(message, request_id.0))?;
    if input.write_enabled {
        bounded_write(request_id.0, async {
            let mut connection =
                DatabaseConnection::connect_write(&record.input, &settings).await?;
            write::initialize_saved(&mut connection, &record, &state.store).await
        })
        .await?;
    }
    database(request_id.0, move || {
        state
            .store
            .set_database_writes(record.id, input.write_enabled)
    })
    .await?;
    Ok(Json(settings.summary))
}

pub async fn rows(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(input): ValidatedJson<write::PageRequest>,
) -> Result<Json<write::Page>, AppError> {
    let record = load(state.clone(), request_id.0, id).await?;
    available(&state, record.id, request_id.0)?;
    let settings = record
        .input
        .settings(record.id)
        .map_err(|_| AppError::internal(request_id.0))?;
    bounded_write(request_id.0, async {
        let mut connection = DatabaseConnection::connect(&record.input, &settings).await?;
        connection.editable_page(&input).await
    })
    .await
    .map(Json)
}

pub async fn mutate(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(input): ValidatedJson<write::Mutation>,
) -> Result<Json<write::Receipt>, AppError> {
    let _claim = state.service_mutations.lock().await;
    let record = load(state.clone(), request_id.0, id).await?;
    available(&state, record.id, request_id.0)?;
    if !record.input.write_enabled {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "database_read_only",
            "Enable writes for this database first",
            request_id.0,
        ));
    }
    let settings = record
        .input
        .settings(record.id)
        .map_err(|_| AppError::internal(request_id.0))?;
    bounded_write(request_id.0, async {
        let mut connection = DatabaseConnection::connect_write(&record.input, &settings).await?;
        write::mutate(&mut connection, record.id, &input).await
    })
    .await
    .map(Json)
}

/// Open a saved connection and list its accessible base tables.
pub async fn tables(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<Tables>, AppError> {
    let record = load(state.clone(), request_id.0, id).await?;
    available(&state, record.id, request_id.0)?;
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
    let record = load(state.clone(), request_id.0, id).await?;
    available(&state, record.id, request_id.0)?;
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
    let _claim = state.service_mutations.lock().await;
    if state
        .store
        .database_jobs(id)
        .map_err(|_| AppError::internal(request_id.0))?
        .iter()
        .any(|j| {
            matches!(j.status.as_str(), "queued" | "running" | "needs_recovery")
                || (j.kind == crate::pipe_databases::maintenance::Kind::Backup
                    && j.status == "succeeded")
        })
    {
        return Err(pipe_databases::validation(
            "Finish database jobs and remove retained backups before removing this connection",
            request_id.0,
        ));
    }

    if state
        .store
        .database_bindings()
        .map_err(|_| AppError::internal(request_id.0))?
        .iter()
        .any(|b| b.database_id == id)
        || state
            .store
            .database_triggers()
            .map_err(|_| AppError::internal(request_id.0))?
            .iter()
            .any(|t| t.database_id == id)
    {
        return Err(pipe_databases::validation(
            "Remove service bindings and finish retained triggers before removing this connection",
            request_id.0,
        ));
    }
    if !database(request_id.0, move || state.store.delete_pipe_database(id)).await? {
        return Err(AppError::not_found(request_id.0));
    }
    Ok(Json(serde_json::json!({ "deleted": true })))
}

#[cfg(test)]
#[path = "pipe_databases_tests.rs"]
mod tests;

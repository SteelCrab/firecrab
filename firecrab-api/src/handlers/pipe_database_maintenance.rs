//! Redacted maintenance endpoints and verified archive downloads.
use super::pipe_databases::{bounded_write, load};
use crate::{
    error::AppError,
    extract::ValidatedJson,
    pipe_databases::{
        maintenance::{self, Job, Kind},
        write::Error,
    },
    server::RequestId,
    state::AppState,
};
use axum::{
    Json,
    body::Body,
    extract::{Extension, Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use uuid::Uuid;

/// Report installed backup clients without returning private connection data.
pub async fn capabilities() -> Json<serde_json::Value> {
    Json(maintenance::capabilities().await)
}
/// List redacted maintenance and backup jobs for one connection.
pub async fn list(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let record = load(state.clone(), rid.0, id).await?;
    Ok(Json(
        state
            .store
            .database_jobs(record.id)
            .map_err(|_| AppError::internal(rid.0))?
            .iter()
            .take(100)
            .map(Job::summary)
            .collect(),
    ))
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Request ID for one retryable logical backup.
pub struct BackupInput {
    /// Client UUID reused for retries of the same operation.
    request_id: Uuid,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Versioned SQL and destructive-maintenance acknowledgement.
pub struct MigrationInput {
    /// Client UUID reused for retries of the same operation.
    request_id: Uuid,
    /// Immutable schema version, or original row fingerprint.
    version: String,
    /// Operator migration SQL, kept in private persistence only.
    sql: String,
    /// Exact database name typed before destructive maintenance.
    confirm_database: String,
    /// Operator acknowledgement that external writers are stopped.
    external_writers_stopped: bool,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Verified snapshot selection and restore acknowledgement.
pub struct RestoreInput {
    /// Client UUID reused for retries of the same operation.
    request_id: Uuid,
    /// Successful snapshot selected for a restore.
    backup_id: Uuid,
    /// Exact database name typed before destructive maintenance.
    confirm_database: String,
    /// Operator acknowledgement that external writers are stopped.
    external_writers_stopped: bool,
}

/// Compare immutable request fields before accepting an idempotent retry.
fn matches_input(a: &Job, b: &Job) -> bool {
    a.database_id == b.database_id
        && a.kind == b.kind
        && a.version == b.version
        && a.sql == b.sql
        && a.backup_id == b.backup_id
        && a.target_hash == b.target_hash
}
/// Persist an immutable job intent while excluding competing target maintenance.
fn save(
    state: &AppState,
    rid: Uuid,
    job: Job,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    if let Some(previous) = state
        .store
        .database_job(job.id)
        .map_err(|_| AppError::internal(rid))?
    {
        if !matches_input(&previous, &job) {
            return Err(AppError::conflict(
                "request_conflict",
                "The request ID belongs to another database operation",
                rid,
            ));
        }
        return Ok((StatusCode::OK, Json(previous.summary())));
    }
    let jobs = state
        .store
        .database_target_jobs(job.database_id)
        .map_err(|_| AppError::internal(rid))?;
    if jobs.iter().any(|j| {
        matches!(j.status.as_str(), "queued" | "running")
            || (j.status == "needs_recovery" && job.kind != Kind::Restore)
    }) {
        return Err(AppError::conflict(
            "database_job_in_use",
            "Finish the active database job or restore its safety backup",
            rid,
        ));
    }
    state
        .store
        .save_database_job(&job)
        .map_err(|_| AppError::internal(rid))?;
    Ok((StatusCode::ACCEPTED, Json(job.summary())))
}
/// Map setup errors to redacted HTTP errors.
fn validation(error: Error, rid: Uuid) -> AppError {
    match error {
        Error::Invalid(message) => crate::pipe_databases::validation(message, rid),
        _ => AppError::internal(rid),
    }
}

/// Queue a retryable native logical snapshot.
pub async fn backup(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(input): ValidatedJson<BackupInput>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    let _claim = state.service_mutations.lock().await;
    let record = load(state.clone(), rid.0, id).await?;
    save(
        &state,
        rid.0,
        Job::new(&record, input.request_id, Kind::Backup).map_err(|e| validation(e, rid.0))?,
    )
}
/// Validate SQL and acknowledgements before queuing a schema version.
pub async fn migrate(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(input): ValidatedJson<MigrationInput>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    let _claim = state.service_mutations.lock().await;
    let record = load(state.clone(), rid.0, id).await?;
    let settings = record
        .input
        .settings(record.id)
        .map_err(|_| AppError::internal(rid.0))?;
    if !input.external_writers_stopped || input.confirm_database != settings.summary.database {
        return Err(crate::pipe_databases::validation(
            "Confirm the database name and stop external writers before maintenance",
            rid.0,
        ));
    }
    maintenance::validate_version(&input.version).map_err(|e| validation(e, rid.0))?;
    maintenance::migration_statements(
        settings.summary.engine,
        &settings.summary.database,
        &input.sql,
    )
    .map_err(|e| validation(e, rid.0))?;
    bounded_write(rid.0, maintenance::require_maintenance(&state, &record)).await?;
    let mut job =
        Job::new(&record, input.request_id, Kind::Migration).map_err(|e| validation(e, rid.0))?;
    job.version = Some(input.version);
    job.sql = Some(input.sql);
    save(&state, rid.0, job)
}
/// Queue replacement only from a verified snapshot of the same connection.
pub async fn restore(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(input): ValidatedJson<RestoreInput>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    let _claim = state.service_mutations.lock().await;
    let record = load(state.clone(), rid.0, id).await?;
    let settings = record
        .input
        .settings(record.id)
        .map_err(|_| AppError::internal(rid.0))?;
    if !input.external_writers_stopped || input.confirm_database != settings.summary.database {
        return Err(crate::pipe_databases::validation(
            "Confirm the database name and stop external writers before restore",
            rid.0,
        ));
    }
    bounded_write(rid.0, maintenance::require_maintenance(&state, &record)).await?;
    let snapshot = state
        .store
        .database_job(input.backup_id)
        .map_err(|_| AppError::internal(rid.0))?
        .ok_or_else(|| AppError::not_found(rid.0))?;
    if snapshot.database_id != record.id
        || snapshot.kind != Kind::Backup
        || snapshot.status != "succeeded"
        || snapshot.target_hash
            != maintenance::target_hash(&record).map_err(|e| validation(e, rid.0))?
    {
        return Err(crate::pipe_databases::validation(
            "Choose a successful backup of this connection",
            rid.0,
        ));
    }
    let mut job =
        Job::new(&record, input.request_id, Kind::Restore).map_err(|e| validation(e, rid.0))?;
    job.backup_id = Some(input.backup_id);
    save(&state, rid.0, job)
}
/// Stream a completed private archive to the trusted operator.
pub async fn download(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    use tokio::io::AsyncReadExt;
    let id = super::vms::parse_id(&id, rid.0)?;
    let job = state
        .store
        .database_job(id)
        .map_err(|_| AppError::internal(rid.0))?
        .ok_or_else(|| AppError::not_found(rid.0))?;
    if job.kind != Kind::Backup || job.status != "succeeded" {
        return Err(AppError::not_found(rid.0));
    }
    let path = maintenance::backup_path(&state, id).map_err(|e| validation(e, rid.0))?;
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| AppError::not_found(rid.0))?;
    let stream = futures_util::stream::unfold(file, |mut file| async move {
        let mut bytes = vec![0_u8; 65536];
        match file.read(&mut bytes).await {
            Ok(0) => None,
            Ok(n) => {
                bytes.truncate(n);
                Some((Ok::<_, std::io::Error>(bytes), file))
            }
            Err(e) => Some((Err(e), file)),
        }
    });
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"firecrab-{id}.dump\""),
            ),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}
/// Expire a local archive unless an active recovery job references it.
pub async fn delete_backup(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let id = super::vms::parse_id(&id, rid.0)?;
    let _claim = state.service_mutations.lock().await;
    let mut job = state
        .store
        .database_job(id)
        .map_err(|_| AppError::internal(rid.0))?
        .ok_or_else(|| AppError::not_found(rid.0))?;
    if job.kind != Kind::Backup || !matches!(job.status.as_str(), "succeeded" | "failed") {
        return Err(crate::pipe_databases::validation(
            "Only a finished backup can be removed",
            rid.0,
        ));
    }
    let jobs = state
        .store
        .database_target_jobs(job.database_id)
        .map_err(|_| AppError::internal(rid.0))?;
    if jobs.iter().any(|j| {
        matches!(j.status.as_str(), "queued" | "running" | "needs_recovery")
            && (j.backup_id == Some(id) || j.safety_backup_id == Some(id))
    }) {
        return Err(crate::pipe_databases::validation(
            "This backup is required by an active recovery job",
            rid.0,
        ));
    }
    let path = maintenance::backup_path(&state, id).map_err(|e| validation(e, rid.0))?;
    if let Some(dir) = path.parent() {
        match tokio::fs::remove_dir_all(dir).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(AppError::internal(rid.0)),
        }
    }
    job.status = "expired".into();
    state
        .store
        .save_database_job(&job)
        .map_err(|_| AppError::internal(rid.0))?;
    Ok(Json(serde_json::json!({"deleted":true})))
}

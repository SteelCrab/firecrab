//! PipeStorage connection management and S3 object routes.
use axum::{
    Extension, Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::Response,
};
use firecrab_api_types::{
    MicroServiceResponse, MicroServiceSpec, PipeStorageResponse, StorageCredentialRef,
    StorageObjectPage, StorageServiceRef,
};
use futures_util::StreamExt;
use serde::Deserialize;
use uuid::Uuid;

// Keep permits until streamed response bodies close, including client cancellation.
static TRANSFERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

use crate::{
    error::AppError,
    extract::ValidatedJson,
    handlers::{micro_services::database, vms::parse_id},
    pipe_auth::AwsCredentials,
    pipe_storage::{MAX_OBJECT_BYTES, S3Client, StorageError, StorageInput, StorageRecord},
    server::RequestId,
    state::AppState,
};

pub(crate) fn failure(error: StorageError, id: Uuid) -> AppError {
    AppError::new(error.status, error.code, error.message, id)
}

async fn load(state: &AppState, id: Uuid, request_id: Uuid) -> Result<StorageRecord, AppError> {
    let store = state.store.clone();
    database(request_id, move || store.pipe_storage(id))
        .await?
        .ok_or_else(|| AppError::not_found(request_id))
}

async fn client(
    state: &AppState,
    record: StorageRecord,
    request_id: Uuid,
) -> Result<S3Client, AppError> {
    let store = state.store.clone();
    let copy = record.clone();
    let credentials = tokio::task::spawn_blocking(move || copy.signing_credentials(&store))
        .await
        .map_err(|_| AppError::internal(request_id))?
        .map_err(|e| failure(e, request_id))?;
    let client = S3Client::new(record, credentials).map_err(|e| failure(e, request_id))?;
    #[cfg(test)]
    let client = match &state.s3_test_origin {
        Some(origin) => client.with_test_origin(origin),
        None => client,
    };
    Ok(client)
}

fn summary(record: StorageRecord, services: &[MicroServiceResponse]) -> PipeStorageResponse {
    let references = services
        .iter()
        .filter(|s| s.spec.storage_connection_id == Some(record.id))
        .map(|s| StorageServiceRef {
            id: s.id,
            name: s.spec.name.clone(),
        })
        .collect();
    PipeStorageResponse {
        id: record.id,
        spec: record.spec,
        services: references,
    }
}

pub async fn list(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
) -> Result<Json<Vec<PipeStorageResponse>>, AppError> {
    let (records, services) = database(id.0, move || {
        Ok((
            state.store.list_pipe_storage()?,
            state.store.list_micro_services()?,
        ))
    })
    .await?;
    Ok(Json(
        records.into_iter().map(|r| summary(r, &services)).collect(),
    ))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<PipeStorageResponse>, AppError> {
    let record = load(&state, parse_id(&id, request_id.0)?, request_id.0).await?;
    let services = database(request_id.0, move || state.store.list_micro_services()).await?;
    Ok(Json(summary(record, &services)))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    ValidatedJson(input): ValidatedJson<StorageInput>,
) -> Result<(StatusCode, Json<PipeStorageResponse>), AppError> {
    let record = input
        .record(Uuid::new_v4(), None)
        .map_err(|e| failure(e, id.0))?;
    let _claim = state.service_mutations.lock().await;
    client(&state, record.clone(), id.0)
        .await?
        .verify()
        .await
        .map_err(|e| failure(e, id.0))?;
    let saved = record.clone();
    let store = state.store.clone();
    database(id.0, move || store.save_pipe_storage(&saved)).await?;
    Ok((StatusCode::CREATED, Json(summary(record, &[]))))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(input): ValidatedJson<StorageInput>,
) -> Result<Json<PipeStorageResponse>, AppError> {
    let id = parse_id(&id, request_id.0)?;
    let _claim = state.service_mutations.lock().await;
    ensure_unbound(&state, id, request_id.0).await?;
    let previous = load(&state, id, request_id.0).await?;
    let record = input
        .record(id, Some(&previous))
        .map_err(|e| failure(e, request_id.0))?;
    client(&state, record.clone(), request_id.0)
        .await?
        .verify()
        .await
        .map_err(|e| failure(e, request_id.0))?;
    let saved = record.clone();
    let store = state.store.clone();
    database(request_id.0, move || store.save_pipe_storage(&saved)).await?;
    Ok(Json(summary(record, &[])))
}

async fn ensure_unbound(state: &AppState, id: Uuid, request_id: Uuid) -> Result<(), AppError> {
    let store = state.store.clone();
    if database(request_id, move || store.list_micro_services())
        .await?
        .iter()
        .any(|service| service.spec.storage_connection_id == Some(id))
    {
        return Err(AppError::conflict(
            "storage_in_use",
            "Stop and unbind every referencing MicroService before editing or disconnecting this connection",
            request_id,
        ));
    }
    Ok(())
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let id = parse_id(&id, request_id.0)?;
    let _claim = state.service_mutations.lock().await;
    ensure_unbound(&state, id, request_id.0).await?;
    let store = state.store.clone();
    if !database(request_id.0, move || store.delete_pipe_storage(id)).await? {
        return Err(AppError::not_found(request_id.0));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn verify(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let record = load(&state, parse_id(&id, request_id.0)?, request_id.0).await?;
    client(&state, record, request_id.0)
        .await?
        .verify()
        .await
        .map_err(|e| failure(e, request_id.0))?;
    // IAM cannot be inferred from ListBucket. Probing write/delete would mutate user data.
    Ok(Json(
        serde_json::json!({"list": "allowed", "read": "unknown", "write": "unknown", "delete": "unknown"}),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListQuery {
    prefix: Option<String>,
    cursor: Option<String>,
    limit: Option<u16>,
}

pub async fn objects(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    Query(query): Query<ListQuery>,
) -> Result<Json<StorageObjectPage>, AppError> {
    let record = load(&state, parse_id(&id, request_id.0)?, request_id.0).await?;
    let prefix = query.prefix.unwrap_or_else(|| record.spec.prefix.clone());
    client(&state, record, request_id.0)
        .await?
        .list(&prefix, query.cursor.as_deref(), query.limit.unwrap_or(100))
        .await
        .map(Json)
        .map_err(|e| failure(e, request_id.0))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectQuery {
    pub key: String,
    #[serde(default)]
    pub overwrite: bool,
}

pub async fn upload(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    Query(query): Query<ObjectQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<StatusCode, AppError> {
    let length: u64 = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| {
            AppError::new(
                StatusCode::LENGTH_REQUIRED,
                "storage_length_required",
                "Provide Content-Length for S3 uploads",
                request_id.0,
            )
        })?;
    if length > MAX_OBJECT_BYTES {
        return Err(AppError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "storage_object_too_large",
            "Object transfers are limited to 512 MiB",
            request_id.0,
        ));
    }
    let _permit = TRANSFERS
        .acquire()
        .await
        .map_err(|_| AppError::internal(request_id.0))?;
    let record = load(&state, parse_id(&id, request_id.0)?, request_id.0).await?;
    client(&state, record, request_id.0)
        .await?
        .upload(&query.key, length, query.overwrite, body)
        .await
        .map_err(|e| failure(e, request_id.0))?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn download(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    Query(query): Query<ObjectQuery>,
) -> Result<Response, AppError> {
    let permit = TRANSFERS
        .acquire()
        .await
        .map_err(|_| AppError::internal(request_id.0))?;
    let record = load(&state, parse_id(&id, request_id.0)?, request_id.0).await?;
    let response = client(&state, record, request_id.0)
        .await?
        .download(&query.key)
        .await
        .map_err(|e| failure(e, request_id.0))?;
    let length = response
        .content_length()
        .expect("S3 client checked content length");
    let name = query.key.rsplit('/').next().unwrap_or("download");
    let filename =
        percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC).to_string();
    Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, length)
        .header(header::CACHE_CONTROL, "no-store")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename*=UTF-8''{filename}"),
        )
        .body(Body::from_stream(
            response
                .bytes_stream()
                .map_err(|_| std::io::Error::other("S3 download interrupted"))
                .scan(permit, |_, item| std::future::ready(Some(item))),
        ))
        .map_err(|_| AppError::internal(request_id.0))
}

pub async fn delete_object(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    Query(query): Query<ObjectQuery>,
) -> Result<StatusCode, AppError> {
    let record = load(&state, parse_id(&id, request_id.0)?, request_id.0).await?;
    client(&state, record, request_id.0)
        .await?
        .delete(&query.key)
        .await
        .map_err(|e| failure(e, request_id.0))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Called under the shared mutation lock before saving or deploying a service.
pub(crate) async fn validate_binding(
    state: &AppState,
    spec: &MicroServiceSpec,
    request_id: Uuid,
) -> Result<(), AppError> {
    let Some(id) = spec.storage_connection_id else {
        return Ok(());
    };
    let reserved = [
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_REGION",
        "AWS_DEFAULT_REGION",
        "AWS_PROFILE",
        "AWS_SHARED_CREDENTIALS_FILE",
        "AWS_CONFIG_FILE",
        "AWS_ENDPOINT_URL",
        "AWS_ENDPOINT_URL_S3",
        "FIRECRAB_S3_BUCKET",
        "FIRECRAB_S3_PREFIX",
    ];
    if reserved
        .iter()
        .any(|key| spec.env.contains_key(*key) || spec.secrets.contains_key(*key))
    {
        return Err(failure(
            StorageError::invalid(
                "Remove AWS credential/endpoint and FIRECRAB_S3 variables when binding a storage connection",
            ),
            request_id,
        ));
    }
    load(state, id, request_id).await?;
    Ok(())
}

/// Recheck every shared S3 connection before replacing the Auth AWS credential.
pub(crate) async fn validate_aws_replacement(
    state: &AppState,
    credentials: &AwsCredentials,
    id: Uuid,
) -> Result<(), AppError> {
    let store = state.store.clone();
    let records = database(id, move || store.list_pipe_storage()).await?;
    for record in records
        .into_iter()
        .filter(|r| r.spec.credential_ref == StorageCredentialRef::PipeAuthAws)
    {
        let mut keys = credentials.clone();
        keys.region = record.spec.region.clone();
        S3Client::new(record, keys)
            .map_err(|e| failure(e, id))?
            .verify()
            .await
            .map_err(|e| failure(e, id))?;
    }
    Ok(())
}

use futures_util::TryStreamExt;

#[cfg(test)]
#[path = "pipe_storage_tests.rs"]
mod tests;

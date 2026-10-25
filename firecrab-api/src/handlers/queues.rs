//! Independent Queue dashboard/API, not a Pipe connector.
use crate::{
    error::AppError,
    extract::ValidatedJson,
    queues::{self, QueueRecord, RabbitClient},
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use firecrab_api_types::*;
use std::collections::BTreeMap;
use uuid::Uuid;

/// Return public engine state and management readiness.
pub async fn engine(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
) -> Result<Json<Option<QueueEngineResponse>>, AppError> {
    queues::engine_status(&state, id.0).await.map(Json)
}
/// Accept installation of the single internal RabbitMQ engine.
pub async fn create_engine(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    ValidatedJson(spec): ValidatedJson<CreateQueueEngineRequest>,
) -> Result<(StatusCode, Json<QueueEngineResponse>), AppError> {
    Ok((
        StatusCode::ACCEPTED,
        Json(queues::create_engine(state, spec, id.0).await?),
    ))
}
/// Accept recovery of the same broker VM and disk.
pub async fn start_engine(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
) -> Result<(StatusCode, Json<QueueEngineResponse>), AppError> {
    Ok((
        StatusCode::ACCEPTED,
        Json(queues::start_engine(state, id.0).await?),
    ))
}
/// Accept a broker stop without deleting messages or credentials.
pub async fn stop_engine(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
) -> Result<(StatusCode, Json<QueueEngineResponse>), AppError> {
    Ok((
        StatusCode::ACCEPTED,
        Json(queues::stop_engine(state, id.0).await?),
    ))
}
/// List public queue declarations and their service references.
pub async fn list(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
) -> Result<Json<Vec<QueueResponse>>, AppError> {
    let records = state
        .store
        .list_queues()
        .map_err(|_| AppError::internal(id.0))?;
    let services = state
        .store
        .list_micro_services()
        .map_err(|_| AppError::internal(id.0))?;
    Ok(Json(
        records.iter().map(|q| q.response(&services)).collect(),
    ))
}
/// Resolve one validated private queue record or return a sanitized error.
fn load(state: &AppState, id: Uuid, request_id: Uuid) -> Result<QueueRecord, AppError> {
    state
        .store
        .queue(id)
        .map_err(|_| AppError::internal(request_id))?
        .ok_or_else(|| AppError::not_found(request_id))
}
/// Resolve the owned broker transport without exposing administrator credentials.
fn client(state: &AppState, id: Uuid) -> Result<RabbitClient, AppError> {
    RabbitClient::new(&state.store).map_err(|message| AppError::unavailable(message, id))
}
/// Validate and persist a queue identity before detached provisioning starts.
pub async fn create(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    ValidatedJson(spec): ValidatedJson<CreateQueueRequest>,
) -> Result<(StatusCode, Json<QueueResponse>), AppError> {
    let mut fields = BTreeMap::new();
    if !queues::valid_name(&spec.name) {
        fields.insert(
            "name".into(),
            "must be 1-40 lowercase ASCII letters, digits or '-'".into(),
        );
    }
    if !(1..=100).contains(&spec.delivery_limit) {
        fields.insert("deliveryLimit".into(), "must be between 1 and 100".into());
    }
    if !(1024 * 1024..=1024 * 1024 * 1024).contains(&spec.max_bytes) {
        fields.insert("maxBytes".into(), "must be between 1 MiB and 1 GiB".into());
    }
    if !fields.is_empty() {
        return Err(AppError::validation(fields, id.0));
    }
    let _services = state.service_mutations.lock().await;
    let guard = state
        .queue_mutations
        .clone()
        .try_lock_owned()
        .map_err(|_| {
            AppError::conflict(
                "queue_engine_busy",
                "Queue engine has an operation in progress",
                id.0,
            )
        })?;
    let records = state
        .store
        .list_queues()
        .map_err(|_| AppError::internal(id.0))?;
    if records.iter().any(|q| q.name == spec.name) {
        return Err(AppError::validation(
            BTreeMap::from([("name".into(), "is already registered".into())]),
            id.0,
        ));
    }
    if records.len() >= 64 {
        return Err(AppError::conflict(
            "queue_limit",
            "This host supports at most 64 application queues",
            id.0,
        ));
    }
    let client = client(&state, id.0)?;
    client.verify(id.0).await?;
    let record = QueueRecord {
        id: Uuid::new_v4(),
        name: spec.name,
        delivery_limit: spec.delivery_limit,
        max_bytes: spec.max_bytes,
        phase: QueuePhase::Provisioning,
        error: None,
        publish_password: queues::password(),
        consume_password: queues::password(),
    };
    state
        .store
        .save_queue(&record)
        .map_err(|_| AppError::internal(id.0))?;
    let result = record.response(&[]);
    drop(_services);
    tokio::spawn(async move {
        let _guard = guard;
        provision(state, record, client, id.0).await;
    });
    Ok((StatusCode::ACCEPTED, Json(result)))
}
/// Declare RabbitMQ resources and persist success or a retryable failure.
async fn provision(state: AppState, mut record: QueueRecord, client: RabbitClient, id: Uuid) {
    match client.declare(&record, id).await {
        Ok(()) => {
            record.phase = QueuePhase::Ready;
            record.error = None;
        }
        Err(error) => {
            record.phase = QueuePhase::Failed;
            record.error = Some(error.description());
        }
    }
    if state.store.save_queue(&record).is_err() {
        tracing::error!("could not save Queue provisioning result")
    }
}
/// Retry failed provisioning with the original identity and role passwords.
pub async fn retry(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<QueueResponse>), AppError> {
    let id = super::vms::parse_id(&id, request_id.0)?;
    let guard = state
        .queue_mutations
        .clone()
        .try_lock_owned()
        .map_err(|_| {
            AppError::conflict(
                "queue_engine_busy",
                "Queue engine has an operation in progress",
                request_id.0,
            )
        })?;
    let mut record = load(&state, id, request_id.0)?;
    if record.phase != QueuePhase::Failed {
        return Err(AppError::conflict(
            "queue_not_failed",
            "Only failed Queue setup can be retried",
            request_id.0,
        ));
    }
    let client = client(&state, request_id.0)?;
    record.phase = QueuePhase::Provisioning;
    record.error = None;
    state
        .store
        .save_queue(&record)
        .map_err(|_| AppError::internal(request_id.0))?;
    let result = record.response(
        &state
            .store
            .list_micro_services()
            .map_err(|_| AppError::internal(request_id.0))?,
    );
    tokio::spawn(async move {
        let _guard = guard;
        provision(state, record, client, request_id.0).await;
    });
    Ok((StatusCode::ACCEPTED, Json(result)))
}
/// Return message and consumer counts for one queue.
pub async fn metrics(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<QueueMetrics>, AppError> {
    let id = super::vms::parse_id(&id, request_id.0)?;
    let record = load(&state, id, request_id.0)?;
    client(&state, request_id.0)?
        .metrics(&record, request_id.0)
        .await
        .map(Json)
}
/// Explicitly reveal one role connection with Cache-Control set to no-store.
pub async fn reveal(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path((id, access)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let id = super::vms::parse_id(&id, request_id.0)?;
    let access = match access.as_str() {
        "publish" => QueueAccess::Publish,
        "consume" => QueueAccess::Consume,
        _ => return Err(AppError::not_found(request_id.0)),
    };
    let record = load(&state, id, request_id.0)?;
    let connection = queues::connection(&state.store, &record, access)
        .map_err(|_| AppError::unavailable("Queue connection is unavailable", request_id.0))?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(connection)).into_response())
}
/// Reject referenced or busy queues before deleting broker and local records.
pub async fn delete(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let id = super::vms::parse_id(&id, request_id.0)?;
    let _services = state.service_mutations.lock().await;
    let _guard = state
        .queue_mutations
        .clone()
        .try_lock_owned()
        .map_err(|_| {
            AppError::conflict(
                "queue_engine_busy",
                "Queue engine has an operation in progress",
                request_id.0,
            )
        })?;
    let record = load(&state, id, request_id.0)?;
    let services = state
        .store
        .list_micro_services()
        .map_err(|_| AppError::internal(request_id.0))?;
    if services
        .iter()
        .any(|s| s.spec.queues.values().any(|b| b.queue_id == id))
    {
        return Err(AppError::conflict(
            "queue_in_use",
            "Stop and unbind every referencing MicroService before deleting this Queue",
            request_id.0,
        ));
    }
    client(&state, request_id.0)?
        .delete(&record, request_id.0)
        .await?;
    state
        .store
        .delete_queue(id)
        .map_err(|_| AppError::internal(request_id.0))?;
    Ok(StatusCode::NO_CONTENT)
}

//! Database trigger lifecycle and redacted delivery health.
use super::{
    micro_services::database,
    pipe_databases::{bounded_write, load},
};
use crate::{
    error::AppError,
    extract::ValidatedJson,
    pipe_databases::{
        DatabaseConnection,
        events::{self, TriggerStatus},
    },
    server::RequestId,
    state::AppState,
};
use axum::{
    Json,
    extract::{Extension, Path, State},
};

/// Return RabbitMQ endpoint metadata without authentication material.
pub async fn rabbitmq_get(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
) -> Result<Json<serde_json::Value>, AppError> {
    let config = database(rid.0, move || state.store.database_rabbitmq()).await?;
    Ok(Json(match config {
        Some(c) => {
            serde_json::json!({"configured":true,"brokers":c.brokers,"tls":c.tls,"authenticated":!c.username.is_empty(),"development":c.development})
        }
        None => serde_json::json!({"configured":false}),
    }))
}
pub use super::pipe_database_broker_mutations::save as rabbitmq_save;
/// Return capture lifecycle, delivery counts and redacted worker health.
pub async fn list(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<Vec<TriggerStatus>>, AppError> {
    let record = load(state.clone(), rid.0, id).await?;
    let store = state.store.clone();
    let triggers = database(rid.0, move || store.database_triggers()).await?;
    let settings = record
        .input
        .settings(record.id)
        .map_err(|_| AppError::internal(rid.0))?;
    let mut output = Vec::new();
    for trigger in triggers.into_iter().filter(|t| t.database_id == record.id) {
        let id = trigger.id;
        let mut status = if trigger.installed {
            bounded_write(rid.0, async {
                let mut c = DatabaseConnection::connect(&record.input, &settings).await?;
                events::status(&mut c, trigger).await
            })
            .await?
        } else {
            TriggerStatus {
                trigger,
                pending: 0,
                failed: 0,
                delivered: 0,
                skipped: 0,
                last_error: Some("setup_incomplete".into()),
            }
        };
        if let Some(error) = state
            .store
            .database_trigger_health(id)
            .map_err(|_| AppError::internal(rid.0))?
        {
            status.last_error = Some(error)
        }
        output.push(status);
    }
    Ok(Json(output))
}

/// Return public state of the owned RabbitMQ cluster.
pub async fn managed_get(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
) -> Result<Json<serde_json::Value>, AppError> {
    Ok(Json(
        state
            .store
            .managed_database_rabbitmq()
            .map_err(|_| AppError::internal(rid.0))?
            .map(|c| c.summary())
            .unwrap_or(serde_json::Value::Null),
    ))
}
pub use super::pipe_database_event_mutations::{create, delete, detach, pause};
pub use super::pipe_database_managed_broker_mutations::{
    disconnect as rabbitmq_disconnect, prepare as managed_create,
};
/// Return replayable failure IDs without source row contents.
pub async fn failures(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<Vec<events::FailedEvent>>, AppError> {
    let id = super::vms::parse_id(&id, rid.0)?;
    let trigger = state
        .store
        .database_trigger(id)
        .map_err(|_| AppError::internal(rid.0))?
        .ok_or_else(|| AppError::not_found(rid.0))?;
    let record = load(state, rid.0, trigger.database_id.to_string()).await?;
    let settings = record
        .input
        .settings(record.id)
        .map_err(|_| AppError::internal(rid.0))?;
    bounded_write(rid.0, async {
        let mut c = DatabaseConnection::connect(&record.input, &settings).await?;
        events::failed(&mut c, &trigger).await
    })
    .await
    .map(Json)
}
/// Requeue one failed event while retaining its original delivery ID.
pub async fn replay(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path((id, event)): Path<(String, i64)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let id = super::vms::parse_id(&id, rid.0)?;
    let trigger = state
        .store
        .database_trigger(id)
        .map_err(|_| AppError::internal(rid.0))?
        .ok_or_else(|| AppError::not_found(rid.0))?;
    super::pipe_databases::available(&state, trigger.database_id, rid.0)?;
    let record = load(state, rid.0, trigger.database_id.to_string()).await?;
    let settings = record
        .input
        .settings(record.id)
        .map_err(|_| AppError::internal(rid.0))?;
    bounded_write(rid.0, async {
        let mut c = DatabaseConnection::connect_write(&record.input, &settings).await?;
        events::replay(&mut c, &trigger, event).await
    })
    .await?;
    Ok(Json(serde_json::json!({"queued":true})))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Exact event confirmation before skipping a retained failed delivery.
pub struct AcknowledgeInput {
    /// Full trigger UUID and outbox ID selected by the operator.
    confirm_event_id: String,
}

/// Skip a failed delivery after confirmation, preserving its source payload.
pub async fn acknowledge(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path((id, event)): Path<(String, i64)>,
    ValidatedJson(input): ValidatedJson<AcknowledgeInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let id = super::vms::parse_id(&id, rid.0)?;
    if input.confirm_event_id != format!("{id}:{event}") {
        return Err(crate::pipe_databases::validation(
            "Confirm the exact failed event ID",
            rid.0,
        ));
    }
    let trigger = state
        .store
        .database_trigger(id)
        .map_err(|_| AppError::internal(rid.0))?
        .ok_or_else(|| AppError::not_found(rid.0))?;
    super::pipe_databases::available(&state, trigger.database_id, rid.0)?;
    let record = load(state, rid.0, trigger.database_id.to_string()).await?;
    let settings = record
        .input
        .settings(record.id)
        .map_err(|_| AppError::internal(rid.0))?;
    bounded_write(rid.0, async {
        let mut c = DatabaseConnection::connect_write(&record.input, &settings).await?;
        events::acknowledge(&mut c, &trigger, event).await
    })
    .await?;
    Ok(Json(serde_json::json!({"acknowledged":true})))
}

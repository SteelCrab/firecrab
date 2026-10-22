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
        events::{self, KafkaInput, Trigger, TriggerInput, TriggerStatus},
        write::Error,
    },
    server::RequestId,
    state::AppState,
};
use axum::{
    Json,
    extract::{Extension, Path, State},
};

/// Return Kafka endpoint metadata without authentication material.
pub async fn kafka_get(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
) -> Result<Json<serde_json::Value>, AppError> {
    let config = database(rid.0, move || state.store.database_kafka()).await?;
    Ok(Json(match config {
        Some(c) => {
            serde_json::json!({"configured":true,"brokers":c.brokers,"tls":c.tls,"authenticated":c.username.is_some(),"development":c.development})
        }
        None => serde_json::json!({"configured":false}),
    }))
}
/// Validate external Kafka and prohibit cluster replacement with retained triggers.
pub async fn kafka_save(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    ValidatedJson(input): ValidatedJson<KafkaInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let _claim = state.service_mutations.lock().await;
    if !state
        .store
        .database_triggers()
        .map_err(|_| AppError::internal(rid.0))?
        .is_empty()
    {
        return Err(crate::pipe_databases::validation(
            "Finish retained triggers before changing the Kafka cluster",
            rid.0,
        ));
    }
    bounded_write(rid.0, async { input.connect().await.map(|_| ()) }).await?;
    if let Some(mut cluster) = state
        .store
        .managed_database_kafka()
        .map_err(|_| AppError::internal(rid.0))?
    {
        cluster.enabled = false;
        state
            .store
            .save_managed_database_kafka(&cluster)
            .map_err(|_| AppError::internal(rid.0))?;
    }
    database(rid.0, move || state.store.save_database_kafka(&input)).await?;
    Ok(Json(serde_json::json!({"configured":true})))
}
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

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
/// Explicit choice of durable or development Kafka profile.
pub struct ManagedInput {
    #[serde(default)]
    /// Explicit single-broker profile instead of three replicas.
    development: bool,
}
/// Return public state of the owned Kafka cluster.
pub async fn managed_get(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
) -> Result<Json<serde_json::Value>, AppError> {
    Ok(Json(
        state
            .store
            .managed_database_kafka()
            .map_err(|_| AppError::internal(rid.0))?
            .map(|c| c.summary())
            .unwrap_or(serde_json::Value::Null),
    ))
}
/// Reserve durable capacity and queue owned broker preparation.
pub async fn managed_create(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    ValidatedJson(input): ValidatedJson<ManagedInput>,
) -> Result<(axum::http::StatusCode, Json<serde_json::Value>), AppError> {
    use crate::pipe_databases::managed_kafka::{self, Cluster};
    let _claim = state.service_mutations.lock().await;
    if !state
        .store
        .database_triggers()
        .map_err(|_| AppError::internal(rid.0))?
        .is_empty()
    {
        return Err(crate::pipe_databases::validation(
            "Finish retained triggers before replacing the Kafka cluster",
            rid.0,
        ));
    }
    let existing = state
        .store
        .managed_database_kafka()
        .map_err(|_| AppError::internal(rid.0))?;
    let cluster = if let Some(mut cluster) = existing {
        if (cluster.enabled && cluster.phase != "failed")
            || cluster.development != input.development
        {
            return Err(AppError::conflict(
                "kafka_in_use",
                "The managed Kafka cluster already exists",
                rid.0,
            ));
        }
        cluster.enabled = true;
        cluster.phase = "queued".into();
        cluster
    } else {
        managed_kafka::preflight(&state, input.development).map_err(|e| match e {
            Error::Invalid(message) => crate::pipe_databases::validation(message, rid.0),
            _ => AppError::internal(rid.0),
        })?;
        Cluster {
            id: uuid::Uuid::new_v4(),
            enabled: true,
            image: managed_kafka::default_image(),
            development: input.development,
            phase: "queued".into(),
            error: None,
            brokers: vec![],
            config: None,
        }
    };
    state
        .store
        .save_managed_database_kafka(&cluster)
        .map_err(|_| AppError::internal(rid.0))?;
    let summary = cluster.summary();
    managed_kafka::queue(state.clone(), cluster);
    Ok((axum::http::StatusCode::ACCEPTED, Json(summary)))
}
/// Own the topic before capture DDL and preserve partial setup for retries.
pub async fn create(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(input): ValidatedJson<TriggerInput>,
) -> Result<Json<Trigger>, AppError> {
    let record = load(state.clone(), rid.0, id).await?;
    if input.request_id.is_nil() {
        return Err(crate::pipe_databases::validation(
            "A request ID is required",
            rid.0,
        ));
    }
    let _claim = state.service_mutations.lock().await;
    super::pipe_databases::available(&state, record.id, rid.0)?;
    if !record.input.write_enabled {
        return Err(crate::pipe_databases::validation(
            "Enable database writes before configuring capture",
            rid.0,
        ));
    }
    crate::service_runtime::ensure_editable(&state, input.service_id, rid.0).await?;
    let store = state.store.clone();
    let service_id = input.service_id;
    if database(rid.0, move || store.micro_service(service_id))
        .await?
        .is_none()
    {
        return Err(AppError::not_found(rid.0));
    }
    let store = state.store.clone();
    let kafka = database(rid.0, move || store.database_kafka())
        .await?
        .ok_or_else(|| {
            crate::pipe_databases::validation("Set up Kafka before enabling triggers", rid.0)
        })?;
    let mut trigger = Trigger {
        id: input.request_id,
        database_id: record.id,
        input,
        paused: true,
        installed: false,
        detached: false,
    };
    let mut retrying = false;
    if let Some(existing) = state
        .store
        .database_trigger(trigger.id)
        .map_err(|_| AppError::internal(rid.0))?
    {
        if existing.database_id != trigger.database_id
            || serde_json::to_value(&existing.input).ok()
                != serde_json::to_value(&trigger.input).ok()
        {
            return Err(AppError::new(
                axum::http::StatusCode::CONFLICT,
                "request_conflict",
                "The request ID belongs to another trigger",
                rid.0,
            ));
        }
        if existing.installed {
            return Ok(Json(existing));
        }
        retrying = true;
    }
    let client = bounded_write(rid.0, kafka.connect()).await?;
    let topic = trigger.topic();
    let topics = bounded_write(rid.0, async {
        client
            .list_topics()
            .await
            .map_err(|_| Error::Invalid("Cannot inspect Kafka topics"))
    })
    .await?;
    let exists = topics.iter().any(|t| t.name == topic);
    if exists && !retrying {
        return Err(crate::pipe_databases::validation(
            "A Kafka topic with this name already exists; use a new request ID",
            rid.0,
        ));
    }
    // Intent owns a topic even if the controller acknowledges after our timeout.
    state
        .store
        .save_database_trigger(&trigger)
        .map_err(|_| AppError::internal(rid.0))?;
    if !exists {
        bounded_write(rid.0,async {client.controller_client().map_err(|_|Error::Invalid("Cannot connect to the Kafka controller"))?.create_topic(topic,1,if kafka.development{1}else{3},5000).await.map_err(|_|Error::Invalid("Cannot create the Kafka topic; check replication capacity and CREATE permission"))}).await?;
    }
    state
        .store
        .database_service_secret(trigger.input.service_id)
        .map_err(|_| AppError::internal(rid.0))?;
    let settings = record
        .input
        .settings(record.id)
        .map_err(|_| AppError::internal(rid.0))?;
    bounded_write(rid.0, async {
        let mut c = DatabaseConnection::connect_write(&record.input, &settings).await?;
        events::install(&mut c, &trigger).await
    })
    .await?;
    trigger.installed = true;
    trigger.paused = false;
    state
        .store
        .save_database_trigger(&trigger)
        .map_err(|_| AppError::internal(rid.0))?;
    Ok(Json(trigger))
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
/// Operator control over subsequent HTTP delivery attempts.
pub struct PauseInput {
    /// Stop new delivery attempts after an in-flight request finishes.
    paused: bool,
}
/// Change delivery state without removing source capture.
pub async fn pause(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(input): ValidatedJson<PauseInput>,
) -> Result<Json<Trigger>, AppError> {
    let id = super::vms::parse_id(&id, rid.0)?;
    let _claim = state.service_mutations.lock().await;
    let mut trigger = state
        .store
        .database_trigger(id)
        .map_err(|_| AppError::internal(rid.0))?
        .ok_or_else(|| AppError::not_found(rid.0))?;
    super::pipe_databases::available(&state, trigger.database_id, rid.0)?;
    if (!trigger.installed || trigger.detached) && !input.paused {
        return Err(crate::pipe_databases::validation(
            "Complete trigger setup before resuming delivery",
            rid.0,
        ));
    }
    trigger.paused = input.paused;
    state
        .store
        .save_database_trigger(&trigger)
        .map_err(|_| AppError::internal(rid.0))?;
    Ok(Json(trigger))
}

/// Detach source capture and continue draining a partially installed outbox.
pub async fn detach(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<Trigger>, AppError> {
    let id = super::vms::parse_id(&id, rid.0)?;
    let _claim = state.service_mutations.lock().await;
    let mut trigger = state
        .store
        .database_trigger(id)
        .map_err(|_| AppError::internal(rid.0))?
        .ok_or_else(|| AppError::not_found(rid.0))?;
    super::pipe_databases::available(&state, trigger.database_id, rid.0)?;
    let record = load(state.clone(), rid.0, trigger.database_id.to_string()).await?;
    let settings = record
        .input
        .settings(record.id)
        .map_err(|_| AppError::internal(rid.0))?;
    let retained = bounded_write(rid.0, async {
        let mut c = DatabaseConnection::connect_write(&record.input, &settings).await?;
        events::detach(&mut c, &trigger).await?;
        events::outbox_exists(&mut c, &trigger).await
    })
    .await?;
    trigger.installed = retained;
    trigger.detached = true;
    trigger.paused = false;
    state
        .store
        .save_database_trigger(&trigger)
        .map_err(|_| AppError::internal(rid.0))?;
    Ok(Json(trigger))
}
/// Remove only detached, drained trigger metadata.
pub async fn delete(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let id = super::vms::parse_id(&id, rid.0)?;
    let _claim = state.service_mutations.lock().await;
    let trigger = state
        .store
        .database_trigger(id)
        .map_err(|_| AppError::internal(rid.0))?
        .ok_or_else(|| AppError::not_found(rid.0))?;
    super::pipe_databases::available(&state, trigger.database_id, rid.0)?;
    if !trigger.detached {
        return Err(crate::pipe_databases::validation(
            "Detach source capture before removing a trigger",
            rid.0,
        ));
    }
    if trigger.installed {
        let record = load(state.clone(), rid.0, trigger.database_id.to_string()).await?;
        let settings = record
            .input
            .settings(record.id)
            .map_err(|_| AppError::internal(rid.0))?;
        let status = bounded_write(rid.0, async {
            let mut c = DatabaseConnection::connect(&record.input, &settings).await?;
            events::status(&mut c, trigger).await
        })
        .await?;
        if status.pending != 0 || status.failed != 0 {
            return Err(crate::pipe_databases::validation(
                "Deliver or replay retained events before removing this trigger",
                rid.0,
            ));
        }
    }
    state
        .store
        .delete_database_trigger(id)
        .map_err(|_| AppError::internal(rid.0))?;
    Ok(Json(serde_json::json!({"deleted":true})))
}
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

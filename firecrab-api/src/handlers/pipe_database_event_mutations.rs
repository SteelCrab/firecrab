//! Event configuration workers retain their source session and original operation.
use super::{
    iac,
    stacks::{database, persistence_error},
};
use crate::{
    error::AppError,
    extract::ValidatedJson,
    persistence::iac_database_events::{Admission, EventChange, EventIntent},
    pipe_databases::{
        DatabaseConnection,
        bindings::{self, Access, BindingInput, BindingRecord},
        events, rabbitmq,
        write::Error,
    },
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use firecrab_stack::{NativeAction, NativeOperation, NativeOperationState};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

/// Admit event setup using stable caller request and operation UUIDs.
pub async fn create(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(database): Path<String>,
    headers: HeaderMap,
    ValidatedJson(input): ValidatedJson<events::TriggerInput>,
) -> Response {
    if input.request_id.is_nil() || input.service_id.is_nil() {
        return crate::pipe_databases::validation(
            "Choose a handler and a non-nil event UUID",
            request.0,
        )
        .into_response();
    }
    let database = match super::vms::parse_id(&database, request.0) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let id = match iac::operation_id(&headers, input.request_id) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    route(
        state,
        request,
        headers,
        id,
        EventChange::Create { database, input },
    )
    .await
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
/// A delivery switch; source and handler configuration remain immutable.
pub struct PauseInput {
    /// Whether the relay should leave retained events queued.
    paused: bool,
}
/// Change saved delivery state without replaying application rows.
pub async fn pause(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    headers: HeaderMap,
    ValidatedJson(input): ValidatedJson<PauseInput>,
) -> Response {
    control(state, request, headers, id, |id| EventChange::Pause {
        id,
        paused: input.paused,
    })
    .await
}
/// Stop capture while retaining existing outbox delivery.
pub async fn detach(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    control(state, request, headers, id, |id| EventChange::Detach { id }).await
}
/// Remove drained owned queues and configuration while retaining source evidence.
pub async fn delete(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    control(state, request, headers, id, |id| EventChange::Delete { id }).await
}
/// Validate selectors before constructing typed control work.
async fn control(
    state: AppState,
    request: RequestId,
    headers: HeaderMap,
    id: String,
    change: impl FnOnce(Uuid) -> EventChange,
) -> Response {
    let resource = match super::vms::parse_id(&id, request.0) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let operation = match iac::operation_id(&headers, request.0) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    route(state, request, headers, operation, change(resource)).await
}
/// Translate durable status into a redacted response with the original operation header.
async fn route(
    state: AppState,
    request: RequestId,
    headers: HeaderMap,
    id: Uuid,
    change: EventChange,
) -> Response {
    let store = state.store.clone();
    let result = supervise(state, request, headers, id, change).await;
    let mut response = match result {
        Ok((intent, operation)) => match operation.state {
            NativeOperationState::Succeeded => {
                if operation.action == NativeAction::DeleteDatabaseEvents {
                    Json(serde_json::json!({"deleted":true})).into_response()
                } else {
                    match database(request.0, move || store.database_events_receipt(&intent)).await
                    {
                        Ok(Some(value)) => Json(value).into_response(),
                        _ => AppError::internal(request.0).into_response(),
                    }
                }
            }
            NativeOperationState::Failed => {
                if operation.error_code.as_deref() == Some("database_events_not_drained") {
                    crate::pipe_databases::validation(
                        "Deliver or replay retained events before removing this trigger",
                        request.0,
                    )
                    .into_response()
                } else {
                    AppError::new(StatusCode::BAD_GATEWAY,"database_events_unverified","Event verification failed before changes; correct the configuration and use a new operation ID",request.0).into_response()
                }
            }
            NativeOperationState::NeedsRecovery => AppError::conflict(
                "database_events_recovery_required",
                "Recover the original event operation before changing its configuration",
                request.0,
            )
            .into_response(),
            NativeOperationState::Running => AppError::conflict(
                "database_events_operation_pending",
                "Observe the original event operation before retrying",
                request.0,
            )
            .into_response(),
        },
        Err(error) => error.into_response(),
    };
    response.headers_mut().insert(
        "x-firecrab-operation-id",
        HeaderValue::from_str(&id.to_string()).expect("UUID header"),
    );
    response
}
/// Keep admission and failure recording alive after HTTP disconnect.
async fn supervise(
    state: AppState,
    request: RequestId,
    headers: HeaderMap,
    id: Uuid,
    change: EventChange,
) -> Result<(EventIntent, NativeOperation), AppError> {
    let attempt: Arc<Mutex<Option<EventIntent>>> = Arc::default();
    let observed = attempt.clone();
    let supervisor = state.clone();
    let worker = tokio::spawn(async move {
        let _guard = state.service_mutations.clone().lock_owned().await;
        if let Some(expected) = headers.get("x-firecrab-endpoint-id") {
            let expected = expected.to_str().ok().and_then(|v| Uuid::parse_str(v).ok());
            let store = state.store.clone();
            if expected != Some(database(request.0, move || store.iac_endpoint_id()).await?) {
                return Err(persistence_error(
                    crate::persistence::PersistenceError::IacEndpointConflict,
                    request.0,
                ));
            }
        }
        let store = state.store.clone();
        let (intent, dispatch) =
            match database(request.0, move || store.begin_database_events(id, change)).await? {
                Admission::Ready(intent, dispatch) => (*intent, dispatch),
                Admission::Missing => return Err(AppError::not_found(request.0)),
                Admission::Invalid(message) => {
                    return Err(crate::pipe_databases::validation(message, request.0));
                }
            };
        *attempt.lock().unwrap_or_else(|p| p.into_inner()) = Some(intent.clone());
        if dispatch {
            run(&state, request, &intent).await?;
        }
        let store = state.store.clone();
        let operation = database(request.0, move || store.iac_native_operation(id))
            .await?
            .ok_or_else(|| AppError::internal(request.0))?;
        Ok((intent, operation))
    });
    tokio::spawn(async move {
        match worker.await {
            Ok(Ok(result)) => Ok(result),
            failure => {
                let intent = observed.lock().unwrap_or_else(|p| p.into_inner()).clone();
                if let Some(intent) = intent {
                    let store = supervisor.store.clone();
                    let failed = intent.clone();
                    let _ = database(request.0, move || store.fail_database_events(&failed)).await;
                    let store = supervisor.store.clone();
                    if let Some(operation) =
                        database(request.0, move || store.iac_native_operation(id)).await?
                    {
                        return Ok((intent, operation));
                    }
                }
                match failure {
                    Ok(Err(error)) => Err(error),
                    _ => Err(AppError::internal(request.0)),
                }
            }
        }
    })
    .await
    .map_err(|_| AppError::internal(request.0))?
}
/// Recheck the private attempt immediately before remote effects.
fn check(state: &AppState, intent: &EventIntent) -> Result<(), Error> {
    state
        .store
        .checkpoint_database_events(intent)
        .map_err(|_| Error::Invalid("Event execution changed; recover the original operation"))
}
/// Hold the source SQL-session claim through remote work and the local receipt.
async fn run(state: &AppState, request: RequestId, intent: &EventIntent) -> Result<(), AppError> {
    let work = async {
        let mut outcome = intent.trigger().clone();
        if let Some(paused) = intent.paused() {
            outcome.paused = paused;
            return complete(state, request, intent, Some(outcome)).await;
        }
        if intent.creation_noop() {
            return complete(state, request, intent, Some(outcome)).await;
        }
        let store = state.store.clone();
        let snapshot = intent.clone();
        database(request.0, move || store.prepare_database_events(&snapshot)).await?;
        let record = intent.database();
        let settings = record
            .input
            .settings(record.id)
            .map_err(|_| AppError::internal(request.0))?;
        let mut source = DatabaseConnection::connect_write(&record.input, &settings)
            .await
            .map_err(|_| AppError::internal(request.0))?;
        if !events::claim(&mut source, &outcome)
            .await
            .map_err(|_| AppError::internal(request.0))?
        {
            return Err(AppError::conflict(
                "database_events_busy",
                "An event delivery is finishing; recover the original operation after it finishes",
                request.0,
            ));
        }
        // Keep this same SQL-session claim across broker work and the final receipt.
        match intent.operation.action {
            NativeAction::CreateDatabaseEvents => {
                events::validate_capture(&mut source, &outcome)
                    .await
                    .map_err(|_| AppError::internal(request.0))?;
                let broker = intent
                    .broker()
                    .ok_or_else(|| AppError::internal(request.0))?;
                let client = broker
                    .connect()
                    .await
                    .map_err(|_| AppError::internal(request.0))?;
                let declared = async {
                    check(state, intent)?;
                    rabbitmq::channel(&client, &outcome.queue(), broker.development)
                        .await
                        .map(|_| ())
                }
                .await;
                let _ = tokio::time::timeout(
                    Duration::from_secs(3),
                    client.close(200, "event setup checked".into()),
                )
                .await;
                declared.map_err(|_| AppError::internal(request.0))?;
                if intent.changes_binding() {
                    let store = state.store.clone();
                    let snapshot = intent.clone();
                    let previous =
                        database(request.0, move || store.event_binding(&snapshot)).await?;
                    let mut binding = if let Some(value) = previous {
                        value
                    } else {
                        BindingRecord::new(
                            &record,
                            BindingInput {
                                request_id: intent
                                    .binding_id()
                                    .ok_or_else(|| AppError::internal(request.0))?,
                                service_id: outcome.input.service_id,
                                access: Access::Read,
                                env_name: "DATABASE_URL".into(),
                            },
                        )
                        .await
                        .map_err(|_| AppError::internal(request.0))?
                    };
                    binding.event_inbox = true;
                    binding.ready = false;
                    let store = state.store.clone();
                    let snapshot = intent.clone();
                    let prepared = binding.clone();
                    database(request.0, move || {
                        store.prepare_event_binding(&snapshot, &prepared)
                    })
                    .await?;
                    bindings::provision_checked(&record, &binding, || check(state, intent))
                        .await
                        .map_err(|_| AppError::internal(request.0))?;
                    binding.ready = true;
                    let store = state.store.clone();
                    let snapshot = intent.clone();
                    database(request.0, move || {
                        store.prepare_event_binding(&snapshot, &binding)
                    })
                    .await?;
                }
                check(state, intent).map_err(|_| AppError::internal(request.0))?;
                state
                    .store
                    .database_service_secret(outcome.input.service_id)
                    .map_err(|_| AppError::internal(request.0))?;
                events::install(&mut source, &outcome)
                    .await
                    .map_err(|_| AppError::internal(request.0))?;
                outcome.installed = true;
                outcome.detached = false;
                outcome.paused = false;
                complete(state, request, intent, Some(outcome)).await
            }
            NativeAction::DetachDatabaseEvents => {
                check(state, intent).map_err(|_| AppError::internal(request.0))?;
                events::detach(&mut source, &outcome)
                    .await
                    .map_err(|_| AppError::internal(request.0))?;
                outcome.installed = events::outbox_exists(&mut source, &outcome)
                    .await
                    .map_err(|_| AppError::internal(request.0))?;
                outcome.detached = true;
                outcome.paused = false;
                complete(state, request, intent, Some(outcome)).await
            }
            NativeAction::DeleteDatabaseEvents => {
                if events::outbox_exists(&mut source, &outcome)
                    .await
                    .map_err(|_| AppError::internal(request.0))?
                {
                    let status = events::status(&mut source, outcome.clone())
                        .await
                        .map_err(|_| AppError::internal(request.0))?;
                    if status.pending != 0 || status.failed != 0 {
                        let store = state.store.clone();
                        let snapshot = intent.clone();
                        database(request.0, move || {
                            store.database_events_not_drained(&snapshot)
                        })
                        .await?;
                        return Err(crate::pipe_databases::validation(
                            "Deliver or replay retained events before removing this trigger",
                            request.0,
                        ));
                    }
                }
                let broker = intent
                    .broker()
                    .ok_or_else(|| AppError::internal(request.0))?;
                let client = broker
                    .connect()
                    .await
                    .map_err(|_| AppError::internal(request.0))?;
                let removed = async {
                    check(state, intent)?;
                    let channel =
                        rabbitmq::channel(&client, &outcome.queue(), broker.development).await?;
                    for queue in [outcome.queue(), format!("{}.failed", outcome.queue())] {
                        channel
                            .queue_delete(
                                queue.into(),
                                lapin::options::QueueDeleteOptions::default(),
                            )
                            .await
                            .map_err(|_| {
                                Error::Invalid("Cannot remove the owned RabbitMQ queues")
                            })?;
                    }
                    Ok::<_, Error>(())
                }
                .await;
                let _ = tokio::time::timeout(
                    Duration::from_secs(3),
                    client.close(200, "event removal checked".into()),
                )
                .await;
                removed.map_err(|_| AppError::internal(request.0))?;
                complete(state, request, intent, None).await
            }
            _ => Err(AppError::internal(request.0)),
        }
    };
    // Workers continue after the ordinary HTTP deadline; observers keep their known UUID.
    tokio::time::timeout(Duration::from_secs(90), work)
        .await
        .map_err(|_| AppError::internal(request.0))?
}
/// Commit only the outcome belonging to this execution attempt.
async fn complete(
    state: &AppState,
    request: RequestId,
    intent: &EventIntent,
    outcome: Option<events::Trigger>,
) -> Result<(), AppError> {
    let store = state.store.clone();
    let snapshot = intent.clone();
    database(request.0, move || {
        store.finish_database_events(&snapshot, outcome.as_ref())
    })
    .await?;
    Ok(())
}
/// Run original uncertain work under its new private generation.
pub(crate) async fn recover(
    state: AppState,
    request: RequestId,
    id: Uuid,
) -> Result<NativeOperation, AppError> {
    tokio::spawn(async move {
        let _guard = state.service_mutations.clone().lock_owned().await;
        let store = state.store.clone();
        let (intent, dispatch) =
            database(request.0, move || store.recover_database_events(id)).await?;
        if dispatch {
            let result = tokio::spawn({
                let state = state.clone();
                let intent = intent.clone();
                async move { run(&state, request, &intent).await }
            })
            .await;
            if !matches!(result, Ok(Ok(()))) {
                let store = state.store.clone();
                let snapshot = intent.clone();
                let _ = database(request.0, move || store.fail_database_events(&snapshot)).await;
            }
        }
        let store = state.store.clone();
        database(request.0, move || store.iac_native_operation(id))
            .await?
            .ok_or_else(|| AppError::not_found(request.0))
    })
    .await
    .map_err(|_| AppError::internal(request.0))?
}

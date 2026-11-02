//! Managed lifecycle admission and execution survive cancellation of their HTTP waiter.
use super::{
    iac,
    stacks::{database, persistence_error},
};
use crate::{
    error::AppError,
    extract::ValidatedJson,
    persistence::iac_managed_database_brokers::{
        Admission, ManagedBrokerCommand, ManagedBrokerIntent,
    },
    pipe_databases::{managed_rabbitmq, write::Error},
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use firecrab_stack::{NativeOperation, NativeOperationState};
use uuid::Uuid;

/// An explicit durable or single-node profile; all guest identities remain server-owned.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedInput {
    /// Single-node mode must be explicitly selected for development.
    #[serde(default)]
    development: bool,
}
/// Queue one original setup operation and return its stable identity immediately.
pub async fn prepare(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    headers: HeaderMap,
    ValidatedJson(input): ValidatedJson<ManagedInput>,
) -> Response {
    route(
        state,
        request,
        headers,
        ManagedBrokerCommand::Prepare {
            development: input.development,
        },
    )
    .await
}
/// Stop owned guests and disconnect selection while retaining broker disks and private records.
pub async fn disconnect(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    route(state, request, headers, ManagedBrokerCommand::Disconnect).await
}
/// Public retry IDs select receipts; only private admission grants execution.
async fn route(
    state: AppState,
    request: RequestId,
    headers: HeaderMap,
    command: ManagedBrokerCommand,
) -> Response {
    let id = match iac::operation_id(&headers, request.0) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let store = state.store.clone();
    let result = admit(state, request, headers, id, command).await;
    let mut response=match result {
        Ok(intent)=>match intent.operation.state {
            NativeOperationState::Succeeded=>match database(request.0,move||store.managed_database_broker_receipt(id)).await {
                Ok(Some(value))=>Json(value).into_response(),_=>AppError::internal(request.0).into_response(),
            },
            NativeOperationState::Running=>(StatusCode::ACCEPTED,Json(intent.cluster().map(|c|c.summary()).unwrap_or_else(||serde_json::json!({"operationId":id,"phase":"stopping"})))).into_response(),
            NativeOperationState::NeedsRecovery=>AppError::conflict("database_broker_recovery_required","Recover the original broker operation before changing its configuration",request.0).into_response(),
            NativeOperationState::Failed=>AppError::new(StatusCode::BAD_GATEWAY,"database_broker_unstarted","The operation failed before managed effects; correct the configuration and use a new operation ID",request.0).into_response(),
        },
        Err(error)=>error.into_response(),
    };
    response.headers_mut().insert(
        "x-firecrab-operation-id",
        HeaderValue::from_str(&id.to_string()).expect("UUID header"),
    );
    response
}
/// Resolve exact retries before file locks or mutable capacity checks.
async fn admit(
    state: AppState,
    request: RequestId,
    headers: HeaderMap,
    id: Uuid,
    command: ManagedBrokerCommand,
) -> Result<ManagedBrokerIntent, AppError> {
    tokio::spawn(async move {
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
        let retry = command.clone();
        if let Some(intent) = database(request.0, move || {
            store.managed_database_broker_retry(id, &retry)
        })
        .await?
        {
            return Ok(intent);
        }
        let store = state.store.clone();
        if !database(request.0, move || {
            store.iac_public_write_allowed(&firecrab_stack::ManagedResource {
                kind: "database_rabbitmq".into(),
                id: "default".into(),
            })
        })
        .await?
        {
            return Err(iac::managed(request.0));
        }
        let _service = state.service_mutations.clone().lock_owned().await;
        let guard = state
            .store
            .database_broker_runtime_lock()
            .map_err(|e| persistence_error(e, request.0))?;
        if let ManagedBrokerCommand::Prepare { development } = command
            && state
                .store
                .managed_database_rabbitmq()
                .map_err(|_| AppError::internal(request.0))?
                .is_none()
        {
            managed_rabbitmq::preflight(&state, development).map_err(|e| match e {
                Error::Invalid(message) => crate::pipe_databases::validation(message, request.0),
                _ => AppError::internal(request.0),
            })?;
        }
        let store = state.store.clone();
        let (intent, dispatch) = match database(request.0, move || {
            store.begin_managed_database_broker(id, command, None)
        })
        .await?
        {
            Admission::Ready(intent, dispatch) => (*intent, dispatch),
            Admission::Invalid(message) => {
                return Err(crate::pipe_databases::validation(message, request.0));
            }
        };
        if dispatch {
            let worker = state.clone();
            let original = intent.clone();
            tokio::spawn(async move {
                let _ = supervise(worker, request, original, guard).await;
            });
        }
        Ok(intent)
    })
    .await
    .map_err(|_| AppError::internal(request.0))?
}
/// A panicked or failed worker keeps the original native reservation for recovery.
async fn supervise(
    state: AppState,
    request: RequestId,
    intent: ManagedBrokerIntent,
    guard: std::fs::File,
) -> Result<NativeOperation, AppError> {
    let worker = state.clone();
    let original = intent.clone();
    let outcome = tokio::spawn(async move {
        let cluster = managed_rabbitmq::execute(&worker, &original).await?;
        if let Some(cluster) = cluster {
            let _ = managed_rabbitmq::queue_ready(worker, cluster, guard);
        }
        Ok::<_, Error>(())
    })
    .await;
    if !matches!(outcome, Ok(Ok(()))) {
        let store = state.store.clone();
        let original = intent.clone();
        let _ = database(request.0, move || {
            store.fail_managed_database_broker(&original)
        })
        .await;
    }
    let store = state.store.clone();
    let id = intent.operation.operation_id;
    database(request.0, move || store.iac_native_operation(id))
        .await?
        .ok_or_else(|| AppError::internal(request.0))
}
/// Reuse original identities, progress and TLS material after interruption.
pub(crate) async fn recover(
    state: AppState,
    request: RequestId,
    id: Uuid,
) -> Result<NativeOperation, AppError> {
    tokio::spawn(async move {
        let guard = state
            .store
            .database_broker_runtime_lock()
            .map_err(|e| persistence_error(e, request.0))?;
        let store = state.store.clone();
        let (intent, dispatch) =
            database(request.0, move || store.recover_managed_database_broker(id)).await?;
        if dispatch {
            supervise(state, request, intent, guard).await
        } else {
            Ok(intent.operation)
        }
    })
    .await
    .map_err(|_| AppError::internal(request.0))?
}

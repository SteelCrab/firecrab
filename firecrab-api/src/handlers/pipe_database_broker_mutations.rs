//! A broker handshake cannot lose its admitted identity when its HTTP waiter disconnects.
use super::{
    iac,
    stacks::{database, persistence_error},
};
use crate::{
    error::AppError,
    extract::ValidatedJson,
    persistence::iac_database_brokers::{Admission, BrokerIntent},
    pipe_databases::events::RabbitMQInput,
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
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

/// Verify and select the original transport without declaring or deleting broker objects.
pub async fn save(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    headers: HeaderMap,
    ValidatedJson(input): ValidatedJson<RabbitMQInput>,
) -> Response {
    let id = match iac::operation_id(&headers, request.0) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let result = supervise(state, request, headers, id, Some(input), None).await;
    let mut response = match result {
        Ok(operation) => match operation.state {
            NativeOperationState::Succeeded => {
                Json(serde_json::json!({"configured":true})).into_response()
            }
            NativeOperationState::Failed => AppError::new(
                StatusCode::BAD_GATEWAY,
                "database_broker_unverified",
                "Cannot verify RabbitMQ; the original saved configuration is unchanged",
                request.0,
            )
            .into_response(),
            NativeOperationState::Running | NativeOperationState::NeedsRecovery => {
                AppError::conflict(
                    "database_broker_operation_pending",
                    "Observe or recover the original broker configuration operation",
                    request.0,
                )
                .into_response()
            }
        },
        Err(error) => error.into_response(),
    };
    response.headers_mut().insert(
        "x-firecrab-operation-id",
        HeaderValue::from_str(&id.to_string()).expect("UUID header"),
    );
    response
}
/// An operator recovers the existing UUID, never supplies replacement credentials here.
pub(crate) async fn recover(
    state: AppState,
    request: RequestId,
    id: Uuid,
    expected_generation: u64,
) -> Result<NativeOperation, AppError> {
    supervise(
        state,
        request,
        HeaderMap::new(),
        id,
        None,
        Some(expected_generation),
    )
    .await
}
/// Separate worker and supervisor lifetimes from the HTTP response and detect worker panics.
async fn supervise(
    state: AppState,
    request: RequestId,
    headers: HeaderMap,
    id: Uuid,
    input: Option<RabbitMQInput>,
    expected_generation: Option<u64>,
) -> Result<NativeOperation, AppError> {
    let admitted: Arc<Mutex<Option<BrokerIntent>>> = Arc::default();
    let observed = admitted.clone();
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
        let (intent, dispatch) = if let Some(input) = input {
            match database(request.0, move || store.begin_database_broker(id, input)).await? {
                Admission::Ready(intent, dispatch) => (*intent, dispatch),
                Admission::Invalid(message) => {
                    return Err(crate::pipe_databases::validation(message, request.0));
                }
            }
        } else {
            let expected_generation =
                expected_generation.ok_or_else(|| AppError::internal(request.0))?;
            database(request.0, move || {
                store.recover_database_broker_at(id, expected_generation)
            })
            .await?
        };
        *admitted.lock().unwrap_or_else(|p| p.into_inner()) = Some(intent.clone());
        if dispatch {
            run(&state, request, &intent).await?;
        }
        let store = state.store.clone();
        database(request.0, move || store.iac_native_operation(id))
            .await?
            .ok_or_else(|| AppError::internal(request.0))
    });
    tokio::spawn(async move {
        match worker.await {
            Ok(Ok(result)) => Ok(result),
            failure => {
                let intent = observed.lock().unwrap_or_else(|p| p.into_inner()).clone();
                if let Some(intent) = intent {
                    let store = supervisor.store.clone();
                    let _ = database(request.0, move || store.fail_database_broker(&intent)).await;
                    let store = supervisor.store.clone();
                    if let Some(operation) =
                        database(request.0, move || store.iac_native_operation(id)).await?
                    {
                        return Ok(operation);
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
/// Keep verification read-only and commit local selection with its original receipt.
async fn run(state: &AppState, request: RequestId, intent: &BrokerIntent) -> Result<(), AppError> {
    tokio::time::timeout(Duration::from_secs(90), async {
        let client = intent
            .input()
            .connect()
            .await
            .map_err(|_| AppError::internal(request.0))?;
        let store = state.store.clone();
        let original = intent.clone();
        let checked = database(request.0, move || store.check_database_broker(&original)).await;
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            client.close(200, "broker configuration verified".into()),
        )
        .await;
        checked?;
        let store = state.store.clone();
        let original = intent.clone();
        database(request.0, move || store.finish_database_broker(&original)).await?;
        Ok(())
    })
    .await
    .map_err(|_| AppError::internal(request.0))?
}

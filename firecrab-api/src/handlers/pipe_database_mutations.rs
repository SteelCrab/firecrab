//! Server-owned connection mutations keep their identity after an HTTP disconnect.

use super::{
    iac,
    stacks::{database, persistence_error},
};
use crate::{
    error::AppError,
    extract::ValidatedJson,
    persistence::iac_database_operations::{Admission, DatabaseChange, DatabaseIntent},
    pipe_databases::{
        DatabaseConnection, DatabaseInput, DatabaseRecord, DatabaseSummary, OPERATION_TIMEOUT,
        write,
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
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub async fn create(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    headers: HeaderMap,
    ValidatedJson(input): ValidatedJson<DatabaseInput>,
) -> Response {
    route(state, request, headers, DatabaseChange::Create(input)).await
}
pub async fn update(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    headers: HeaderMap,
    ValidatedJson(input): ValidatedJson<DatabaseInput>,
) -> Response {
    let id = match super::vms::parse_id(&id, request.0) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    route(state, request, headers, DatabaseChange::Update(id, input)).await
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessInput {
    write_enabled: bool,
}

pub async fn access(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    headers: HeaderMap,
    ValidatedJson(input): ValidatedJson<AccessInput>,
) -> Response {
    let id = match super::vms::parse_id(&id, request.0) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    route(
        state,
        request,
        headers,
        DatabaseChange::Access(id, input.write_enabled),
    )
    .await
}

async fn route(
    state: AppState,
    request: RequestId,
    headers: HeaderMap,
    change: DatabaseChange,
) -> Response {
    let id = match iac::operation_id(&headers, request.0) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let result = supervise(state, request, change, id, headers).await;
    let mut response = match result {
        Ok((intent, operation)) => match response_result(&intent, &operation, request.0) {
            Ok(summary) => (
                if operation.action == NativeAction::CreateDatabaseConnection {
                    StatusCode::CREATED
                } else {
                    StatusCode::OK
                },
                Json(summary),
            )
                .into_response(),
            Err(error) => error.into_response(),
        },
        Err(error) => error.into_response(),
    };
    response.headers_mut().insert(
        "x-firecrab-operation-id",
        HeaderValue::from_str(&id.to_string()).expect("UUID header"),
    );
    response
}

fn response_result(
    intent: &DatabaseIntent,
    operation: &NativeOperation,
    request: Uuid,
) -> Result<DatabaseSummary, AppError> {
    match operation.state {
        NativeOperationState::Succeeded => intent
            .input()
            .settings(intent.id())
            .map(|s| s.summary)
            .map_err(|_| AppError::internal(request)),
        NativeOperationState::Failed => Err(AppError::new(
            StatusCode::BAD_GATEWAY,
            "database_connection_unverified",
            "Database verification failed before changes; correct the configuration and retry",
            request,
        )),
        NativeOperationState::NeedsRecovery => Err(AppError::conflict(
            "database_recovery_required",
            "Recover the original database operation before changing the configuration",
            request,
        )),
        NativeOperationState::Running => Err(AppError::conflict(
            "database_operation_pending",
            "Observe the original database operation before retrying",
            request,
        )),
    }
}

async fn supervise(
    state: AppState,
    request: RequestId,
    change: DatabaseChange,
    id: Uuid,
    headers: HeaderMap,
) -> Result<(DatabaseIntent, NativeOperation), AppError> {
    let attempt: Arc<Mutex<Option<DatabaseIntent>>> = Arc::default();
    let observed = attempt.clone();
    let supervisor = state.clone();
    let worker = tokio::spawn(async move {
        let _guard = state.service_mutations.clone().lock_owned().await;
        if let Some(expected) = headers.get("x-firecrab-endpoint-id") {
            let expected = expected.to_str().ok().and_then(|v| Uuid::parse_str(v).ok());
            let store = state.store.clone();
            let endpoint = database(request.0, move || store.iac_endpoint_id()).await?;
            if expected != Some(endpoint) {
                return Err(persistence_error(
                    crate::persistence::PersistenceError::IacEndpointConflict,
                    request.0,
                ));
            }
        }
        let store = state.store.clone();
        let admitted = database(request.0, move || store.begin_database_change(id, change)).await?;
        let (intent, dispatch) = match admitted {
            Admission::Invalid(message) => {
                return Err(crate::pipe_databases::validation(message, request.0));
            }
            Admission::Missing => return Err(AppError::not_found(request.0)),
            Admission::Ready(intent, dispatch) => (*intent, dispatch),
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
            Ok(Ok(value)) => Ok(value),
            failure => {
                let intent = observed.lock().unwrap_or_else(|p| p.into_inner()).clone();
                if let Some(intent) = intent {
                    let store = supervisor.store.clone();
                    let failed = intent.clone();
                    let _ =
                        database(request.0, move || store.fail_database_connection(&failed)).await;
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

async fn run(
    state: &AppState,
    request: RequestId,
    intent: &DatabaseIntent,
) -> Result<(), AppError> {
    let settings = intent
        .input()
        .settings(intent.id())
        .map_err(|_| AppError::internal(request.0))?;
    let work = async {
        if !intent.verify_connection() {
            let store = state.store.clone();
            let completed = intent.clone();
            database(request.0, move || {
                store.finish_database_connection(&completed)
            })
            .await?;
            return Ok(());
        }
        let mut connection = if intent.input().write_enabled {
            DatabaseConnection::connect_write(intent.input(), &settings).await
        } else {
            DatabaseConnection::connect(intent.input(), &settings).await
        }
        .map_err(|_| AppError::internal(request.0))?;
        if intent.input().write_enabled {
            let store = state.store.clone();
            let checkpoint = intent.clone();
            database(request.0, move || {
                store.begin_database_connection_effects(&checkpoint)
            })
            .await?;
            write::initialize_saved(
                &mut connection,
                &DatabaseRecord {
                    id: intent.id(),
                    input: intent.input().clone(),
                },
                &state.store,
            )
            .await
            .map_err(|_| AppError::internal(request.0))?;
        } else {
            connection
                .ping()
                .await
                .map_err(|_| AppError::internal(request.0))?;
        }
        let store = state.store.clone();
        let completed = intent.clone();
        database(request.0, move || {
            store.finish_database_connection(&completed)
        })
        .await?;
        Ok(())
    };
    tokio::time::timeout(OPERATION_TIMEOUT, work)
        .await
        .map_err(|_| AppError::internal(request.0))?
}

/// Recovery is an operator action and always uses the immutable saved configuration.
pub(crate) async fn recover(
    state: AppState,
    request: RequestId,
    id: Uuid,
    expected_generation: u64,
) -> Result<NativeOperation, AppError> {
    let worker_state = state.clone();
    tokio::spawn(async move {
        let _guard = worker_state.service_mutations.clone().lock_owned().await;
        let store = worker_state.store.clone();
        let (intent, dispatch) = database(request.0, move || {
            store.recover_database_connection_at(id, expected_generation)
        })
        .await?;
        if dispatch {
            let action = tokio::spawn({
                let state = worker_state.clone();
                let intent = intent.clone();
                async move { run(&state, request, &intent).await }
            })
            .await;
            if !matches!(action, Ok(Ok(()))) {
                let store = worker_state.store.clone();
                let failed = intent.clone();
                let _ = database(request.0, move || store.fail_database_connection(&failed)).await;
            }
        }
        let store = worker_state.store.clone();
        database(request.0, move || store.iac_native_operation(id))
            .await?
            .ok_or_else(|| AppError::not_found(request.0))
    })
    .await
    .map_err(|_| AppError::internal(request.0))?
}

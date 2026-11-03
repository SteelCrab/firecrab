//! Account mutations outlive their HTTP waiter and recover their original private account.
use super::{
    iac,
    stacks::{database, persistence_error},
};
use crate::{
    error::AppError,
    extract::ValidatedJson,
    persistence::iac_database_bindings::{Admission, BindingChange, BindingIntent},
    pipe_databases::{
        OPERATION_TIMEOUT,
        bindings::{self, BindingInput, BindingRecord},
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
    Path(id): Path<String>,
    headers: HeaderMap,
    ValidatedJson(input): ValidatedJson<BindingInput>,
) -> Response {
    let id = match super::vms::parse_id(&id, request.0) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let operation = match iac::operation_id(&headers, input.request_id) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    route(
        state,
        request,
        headers,
        operation,
        BindingChange::Create {
            database: id,
            input,
        },
    )
    .await
}
pub async fn delete(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let id = match super::vms::parse_id(&id, request.0) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let operation = match iac::operation_id(&headers, request.0) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    route(
        state,
        request,
        headers,
        operation,
        BindingChange::Delete { id },
    )
    .await
}
async fn route(
    state: AppState,
    request: RequestId,
    headers: HeaderMap,
    id: Uuid,
    change: BindingChange,
) -> Response {
    let store = state.store.clone();
    let result = supervise(state, request, headers, id, change).await;
    let mut response=match result {
        Ok((intent,operation))=>{
            match operation.state {
                NativeOperationState::Succeeded=>{
                    if operation.action==NativeAction::DeleteDatabaseBinding {Json(serde_json::json!({"deleted":true})).into_response()}
                    else {match database(request.0,move||store.database_binding_receipt(&intent)).await {
                        Ok(Some(summary))=>Json(summary).into_response(),
                        _=>AppError::internal(request.0).into_response(),
                    }}
                }
                NativeOperationState::Failed=>AppError::new(StatusCode::BAD_GATEWAY,"database_binding_unverified","Binding verification failed before changes; correct the configuration and use a new operation ID",request.0).into_response(),
                NativeOperationState::NeedsRecovery=>AppError::conflict("database_binding_recovery_required","Recover the original binding operation and its private account before changing the configuration",request.0).into_response(),
                NativeOperationState::Running=>AppError::conflict("database_binding_operation_pending","Observe the original binding operation before retrying",request.0).into_response(),
            }
        }
        Err(error)=>error.into_response(),
    };
    response.headers_mut().insert(
        "x-firecrab-operation-id",
        HeaderValue::from_str(&id.to_string()).expect("UUID header"),
    );
    response
}
async fn supervise(
    state: AppState,
    request: RequestId,
    headers: HeaderMap,
    id: Uuid,
    change: BindingChange,
) -> Result<(BindingIntent, NativeOperation), AppError> {
    let attempt: Arc<Mutex<Option<BindingIntent>>> = Arc::default();
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
            match database(request.0, move || store.begin_database_binding(id, change)).await? {
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
                    let _ = database(request.0, move || store.fail_database_binding(&failed)).await;
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
async fn run(state: &AppState, request: RequestId, intent: &BindingIntent) -> Result<(), AppError> {
    let work = async {
        let store = state.store.clone();
        let snapshot = intent.clone();
        let previous = database(request.0, move || {
            store.database_binding_prepared(&snapshot)
        })
        .await?;
        let binding = if intent.operation.action == NativeAction::CreateDatabaseBinding {
            if let Some(binding) = previous {
                binding
            } else {
                let record = BindingRecord::new(&intent.database(), intent.input().clone())
                    .await
                    .map_err(|_| AppError::internal(request.0))?;
                let store = state.store.clone();
                let snapshot = intent.clone();
                database(request.0, move || {
                    store.prepare_database_binding(&snapshot, &record)
                })
                .await?
            }
        } else {
            previous.ok_or_else(|| AppError::internal(request.0))?
        };
        if intent.operation.action == NativeAction::DeleteDatabaseBinding || !binding.ready {
            let check = || {
                state
                    .store
                    .begin_database_binding_effects(intent)
                    .map_err(|_| {
                        crate::pipe_databases::write::Error::Invalid(
                            "Binding execution changed; observe and recover the original operation",
                        )
                    })
            };
            let result = if intent.operation.action == NativeAction::CreateDatabaseBinding {
                bindings::provision_checked(&intent.database(), &binding, check).await
            } else {
                bindings::revoke_checked(&intent.database(), &binding, check).await
            };
            result.map_err(|_| AppError::internal(request.0))?;
        }
        let store = state.store.clone();
        let completed = intent.clone();
        database(request.0, move || store.finish_database_binding(&completed)).await?;
        Ok(())
    };
    tokio::time::timeout(OPERATION_TIMEOUT, work)
        .await
        .map_err(|_| AppError::internal(request.0))?
}
pub(crate) async fn recover(
    state: AppState,
    request: RequestId,
    id: Uuid,
    expected_generation: u64,
) -> Result<NativeOperation, AppError> {
    tokio::spawn(async move {
        let _guard = state.service_mutations.clone().lock_owned().await;
        let store = state.store.clone();
        let (intent, dispatch) = database(request.0, move || {
            store.recover_database_binding_at(id, expected_generation)
        })
        .await?;
        if dispatch {
            let result = tokio::spawn({
                let state = state.clone();
                let intent = intent.clone();
                async move { run(&state, request, &intent).await }
            })
            .await;
            if !matches!(result, Ok(Ok(()))) {
                let store = state.store.clone();
                let failed = intent.clone();
                let _ = database(request.0, move || store.fail_database_binding(&failed)).await;
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

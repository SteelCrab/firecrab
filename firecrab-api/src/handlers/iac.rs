//! Public, generation-checked ownership metadata shared by IaC workflows.

use axum::extract::{Path, State};
use axum::{Extension, Json};
use firecrab_stack::{ChangeResourceOwner, ManagedResource, ResourceOwnership};

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::handlers::stacks::database;
use crate::server::RequestId;
use crate::state::AppState;

/// A retry key selects an immutable operation; it never provides write authority.
pub(crate) fn operation_id(
    headers: &axum::http::HeaderMap,
    request_id: uuid::Uuid,
) -> Result<uuid::Uuid, AppError> {
    let Some(value) = headers.get("x-firecrab-operation-id") else {
        return Ok(request_id);
    };
    value
        .to_str()
        .ok()
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .filter(|id| !id.is_nil())
        .ok_or_else(|| {
            AppError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "invalid_operation_id",
                "provide a non-nil operation UUID",
                request_id,
            )
        })
}

/// Advertise the actual transfer adapters separately from the full target inventory.
pub async fn identity(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
) -> Result<Json<serde_json::Value>, AppError> {
    let id = database(request.0, move || state.store.iac_endpoint_id()).await?;
    Ok(Json(
        serde_json::json!({"endpointId":id,"inventoryKinds":firecrab_stack::resource_kinds(),
        "transferKinds":crate::persistence::IAC_TRANSFER_KINDS}),
    ))
}

/// List the native and Terraform writers without returning desired configuration.
pub async fn owners(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
) -> Result<Json<Vec<ResourceOwnership>>, AppError> {
    database(request.0, move || state.store.list_iac_resource_owners())
        .await
        .map(Json)
}

/// A free owner slot does not assert that the corresponding resource exists.
pub async fn owner(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path((kind, id)): Path<(String, String)>,
) -> Result<Json<ResourceOwnership>, AppError> {
    let resource = ManagedResource { kind, id };
    database(request.0, move || state.store.iac_resource_owner(&resource))
        .await
        .map(Json)
}

/// Recover a committed acknowledgement after losing the original HTTP response.
pub async fn operation(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<firecrab_stack::OwnershipOperation>, AppError> {
    database(request.0, move || state.store.iac_ownership_operation(id))
        .await?
        .map(Json)
        .ok_or_else(|| AppError::not_found(request.0))
}

/// Observe native backend work, including an outcome requiring explicit recovery.
pub async fn native_operation(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<firecrab_stack::NativeOperation>, AppError> {
    database(request.0, move || state.store.iac_native_operation(id))
        .await?
        .map(Json)
        .ok_or_else(|| AppError::not_found(request.0))
}

/// Recover a lost acknowledgement using the exact resource already known to the caller.
pub async fn native_resource_operations(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path((kind, id)): Path<(String, String)>,
) -> Result<Json<Vec<firecrab_stack::NativeOperation>>, AppError> {
    let mut resource = ManagedResource { kind, id };
    resource.normalize().map_err(|_| {
        AppError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "invalid_iac_owner",
            "provide an exact resource identity",
            request.0,
        )
    })?;
    database(request.0, move || {
        state.store.iac_native_operations(&resource)
    })
    .await
    .map(Json)
}

/// Only an authenticated operator may resume the original typed native action.
pub async fn recover_native_operation(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: crate::ai_sandbox::access::Caller,
    Path(id): Path<uuid::Uuid>,
    headers: axum::http::HeaderMap,
) -> Result<Json<firecrab_stack::NativeOperation>, AppError> {
    caller.operator().map_err(|_| {
        AppError::new(
            axum::http::StatusCode::FORBIDDEN,
            "sandbox_permission_denied",
            "native operation recovery requires operator access",
            request.0,
        )
    })?;
    let store = state.store.clone();
    let operation = database(request.0, move || store.iac_native_operation(id))
        .await?
        .ok_or_else(|| AppError::not_found(request.0))?;
    if let Some(expected) = headers.get("x-firecrab-endpoint-id") {
        let expected = expected
            .to_str()
            .ok()
            .and_then(|value| uuid::Uuid::parse_str(value).ok());
        if expected != Some(operation.ownership.endpoint_id) {
            return Err(super::stacks::persistence_error(
                crate::persistence::PersistenceError::IacEndpointConflict,
                request.0,
            ));
        }
    }
    if matches!(
        operation.state,
        firecrab_stack::NativeOperationState::Succeeded
            | firecrab_stack::NativeOperationState::Failed
    ) {
        return Ok(Json(operation));
    }
    let expected_generation = headers
        .get("x-firecrab-execution-generation")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| {
            AppError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "iac_recovery_attempt_required",
                "unfinished recovery requires the observed x-firecrab-execution-generation",
                request.0,
            )
        })?;
    if !headers.contains_key("x-firecrab-endpoint-id") {
        return Err(AppError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "iac_endpoint_required",
            "unfinished recovery requires the observed x-firecrab-endpoint-id",
            request.0,
        ));
    }
    if expected_generation != operation.execution_generation {
        return Err(super::stacks::persistence_error(
            crate::persistence::PersistenceError::StaleIacExecution,
            request.0,
        ));
    }
    if matches!(
        operation.action,
        firecrab_stack::NativeAction::InstallImage
            | firecrab_stack::NativeAction::ImportImage
            | firecrab_stack::NativeAction::UpdateImageKernel
            | firecrab_stack::NativeAction::DeleteImage
    ) {
        return super::image_mutations::recover(state, request, id, expected_generation)
            .await
            .map(Json);
    }
    if matches!(
        operation.action,
        firecrab_stack::NativeAction::InstallKernel | firecrab_stack::NativeAction::DeleteKernel
    ) {
        return super::kernel_mutations::recover(state, request, id, expected_generation)
            .await
            .map(Json);
    }
    if matches!(
        operation.action,
        firecrab_stack::NativeAction::CreateDatabaseConnection
            | firecrab_stack::NativeAction::UpdateDatabaseConnection
    ) {
        return super::pipe_database_mutations::recover(state, request, id, expected_generation)
            .await
            .map(Json);
    }
    if matches!(
        operation.action,
        firecrab_stack::NativeAction::CreateDatabaseBinding
            | firecrab_stack::NativeAction::DeleteDatabaseBinding
    ) {
        return super::pipe_database_binding_mutations::recover(
            state,
            request,
            id,
            expected_generation,
        )
        .await
        .map(Json);
    }
    if matches!(
        operation.action,
        firecrab_stack::NativeAction::CreateDatabaseEvents
            | firecrab_stack::NativeAction::PauseDatabaseEvents
            | firecrab_stack::NativeAction::DetachDatabaseEvents
            | firecrab_stack::NativeAction::DeleteDatabaseEvents
    ) {
        return super::pipe_database_event_mutations::recover(
            state,
            request,
            id,
            expected_generation,
        )
        .await
        .map(Json);
    }
    if operation.action == firecrab_stack::NativeAction::DeleteVm {
        return super::vm_deletion::recover(state, request, operation)
            .await
            .map(Json);
    }
    if operation.action == firecrab_stack::NativeAction::ConfigureDatabaseBroker {
        return super::pipe_database_broker_mutations::recover(
            state,
            request,
            id,
            expected_generation,
        )
        .await
        .map(Json);
    }
    if matches!(
        operation.action,
        firecrab_stack::NativeAction::PrepareDatabaseBroker
            | firecrab_stack::NativeAction::RepairDatabaseBroker
            | firecrab_stack::NativeAction::DisconnectDatabaseBroker
    ) {
        return super::pipe_database_managed_broker_mutations::recover(
            state,
            request,
            id,
            expected_generation,
        )
        .await
        .map(Json);
    }
    if crate::persistence::queue_engine_operations::is_engine_action(operation.action) {
        return crate::queues::recover_operation(
            state,
            request.0,
            operation.operation_id,
            expected_generation,
        )
        .await
        .map(Json);
    }
    super::ai_sandbox::recover_project_delete(state, request, caller, operation)
        .await
        .map(Json)
}

/// Adoption/transfer/release commits with its retry result in the same transaction.
pub async fn change_owner(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    ValidatedJson(mut change): ValidatedJson<ChangeResourceOwner>,
) -> Result<Json<ResourceOwnership>, AppError> {
    change.validate().map_err(|_| {
        AppError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "invalid_iac_owner",
            "provide valid resource, owner and operation identities",
            request.0,
        )
    })?;
    let store = state.store.clone();
    let operation = change.operation_id;
    if database(request.0, move || store.iac_ownership_operation(operation))
        .await?
        .is_some()
    {
        // Validate the original fingerprint before returning its immutable result.
        // Current controller activity must not turn an acknowledged retry into a new mutation.
        return database(request.0, move || {
            state.store.change_iac_resource_owner(&change)
        })
        .await
        .map(Json);
    }
    let _admission = state.stack_apply_admission.lock().await;
    let _sandbox = if matches!(
        change.resource.kind.as_str(),
        "sandbox_project" | "sandbox_access"
    ) {
        Some(state.sandbox.admission.lock().await)
    } else {
        None
    };
    let _environment = if change.resource.kind == "sandbox_environment" {
        Some(
            state
                .lock_vm(
                    uuid::Uuid::parse_str(&change.resource.id)
                        .map_err(|_| AppError::internal(request.0))?,
                )
                .await,
        )
    } else {
        None
    };
    let _services = state.service_mutations.lock().await;
    let _queue = if matches!(change.resource.kind.as_str(), "queue" | "queue_engine") {
        Some(state.queue_mutations.try_lock().map_err(|_| {
            AppError::conflict(
                "iac_resource_busy",
                "Queue engine has an operation in progress",
                request.0,
            )
        })?)
    } else {
        None
    };
    let _ai = if matches!(change.resource.kind.as_str(), "ai_connection" | "ai_model") {
        Some(state.pipe_ai_mutations.lock().await)
    } else {
        None
    };
    if change.resource.kind == "ai_connection"
        && state
            .pipe_ai_logins
            .has_pending(
                uuid::Uuid::parse_str(&change.resource.id)
                    .map_err(|_| AppError::internal(request.0))?,
            )
            .map_err(|_| AppError::unavailable("AI login status is unavailable", request.0))?
    {
        return Err(AppError::conflict(
            "iac_resource_busy",
            "finish the pending AI login or wait for its expiry before transferring ownership",
            request.0,
        ));
    }
    if change.resource.kind == "image" {
        let presence = crate::image_lifecycle::prepare_transfer(
            &state.templates,
            &change.resource,
            change.owner.is_some(),
        )
        .map_err(|error| super::stacks::persistence_error(error, request.0))?;
        return database(request.0, move || {
            state.store.change_iac_image_owner(&change, &presence)
        })
        .await
        .map(Json);
    }
    if change.resource.kind == "kernel" && change.owner.is_some() {
        let presence =
            crate::kernel_lifecycle::prepare_transfer(&state.templates, &change.resource)
                .await
                .map_err(|e| super::stacks::persistence_error(e, request.0))?;
        return database(request.0, move || {
            state.store.change_iac_kernel_owner(&change, &presence)
        })
        .await
        .map(Json);
    }
    database(request.0, move || {
        if matches!(
            change.resource.kind.as_str(),
            "secret" | "secret_connection"
        ) {
            state.store.change_iac_vault_owner(&change, &state.secrets)
        } else {
            state.store.change_iac_resource_owner(&change)
        }
    })
    .await
    .map(Json)
}

/// Used by public mutation gates; request headers cannot authorize an IaC write.
pub(crate) fn managed(request: uuid::Uuid) -> AppError {
    AppError::conflict(
        "iac_resource_managed",
        "resource is managed by Terraform; transfer or release its exact ownership before direct edits",
        request,
    )
}

#[cfg(test)]
#[path = "../../tests/unit/iac_api.rs"]
mod tests;

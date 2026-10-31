//! Public, generation-checked ownership metadata shared by IaC workflows.

use axum::extract::{Path, State};
use axum::{Extension, Json};
use firecrab_stack::{ChangeResourceOwner, ManagedResource, ResourceOwnership};

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::handlers::stacks::database;
use crate::server::RequestId;
use crate::state::AppState;

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
    if operation.state == firecrab_stack::NativeOperationState::Succeeded {
        return Ok(Json(operation));
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
    let _sandbox = if change.resource.kind == "sandbox_project" {
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

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
    let _admission = state.stack_apply_admission.lock().await;
    let _services = state.service_mutations.lock().await;
    database(request.0, move || {
        state.store.change_iac_resource_owner(&change)
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

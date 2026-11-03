//! Original cleanup scope is observable; missing legacy proof requires explicit review.

use super::{
    stacks::{database, persistence_error},
    vms,
};
use crate::{
    artifacts::cleanup::VmCleanupStorage,
    error::AppError,
    extract::ValidatedJson,
    persistence::iac_vm_deletes::{StorageReview, VmDeleteIntent},
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use uuid::Uuid;

fn unavailable(id: Uuid) -> AppError {
    AppError::conflict(
        "vm_cleanup_storage_review_required",
        "inspect and explicitly review the original storage of this legacy VM cleanup",
        id,
    )
}

async fn candidate(
    state: &AppState,
    intent: &VmDeleteIntent,
    request: Uuid,
) -> Result<(std::path::PathBuf, VmCleanupStorage), AppError> {
    let vm = Uuid::parse_str(&intent.operation.ownership.resource.id)
        .map_err(|_| AppError::internal(request))?;
    let store = state.store.clone();
    let record = database(request, move || store.load_all())
        .await?
        .remove(&vm)
        .ok_or_else(|| unavailable(request))?;
    if record.storage_root != intent.storage_root() {
        return Err(unavailable(request));
    }
    let root = state
        .storage_path_for(intent.storage_root())
        .map_err(|_| unavailable(request))?;
    let proof = VmCleanupStorage::capture(&root, vm, record.disk_generation)
        .map_err(|_| unavailable(request))?;
    Ok((root, proof))
}

pub async fn observe(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let id = vms::parse_id(&id, request.0)?;
    let store = state.store.clone();
    let intent = database(request.0, move || store.native_vm_delete(id))
        .await?
        .ok_or_else(|| AppError::not_found(request.0))?;
    if let Some(proof) = intent.storage_proof() {
        return Ok(Json(
            serde_json::json!({"approved":intent.storage_view(proof).map_err(|error|persistence_error(error,request.0))?,
            "candidate":null,"reviewRequired":false,"executionGeneration":intent.operation.execution_generation}),
        ));
    }
    if intent.operation.state == firecrab_stack::NativeOperationState::Succeeded {
        return Ok(Json(
            serde_json::json!({"approved":null,"candidate":null,"reviewRequired":false,
            "originalStorageUnrecorded":true,"storageRoot":intent.storage_root(),"retainDisks":intent.retain_disks,
            "executionGeneration":intent.operation.execution_generation}),
        ));
    }
    let (_, proof) = candidate(&state, &intent, request.0).await?;
    Ok(Json(
        serde_json::json!({"approved":null,"candidate":intent.storage_view(&proof).map_err(|error|persistence_error(error,request.0))?,
        "reviewRequired":true,"executionGeneration":intent.operation.execution_generation}),
    ))
}

pub async fn review(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    caller: crate::ai_sandbox::access::Caller,
    ValidatedJson(review): ValidatedJson<StorageReview>,
) -> Result<Json<serde_json::Value>, AppError> {
    caller.operator().map_err(|_| {
        AppError::new(
            axum::http::StatusCode::FORBIDDEN,
            "vm_storage_review_forbidden",
            "storage review requires operator access",
            request.0,
        )
    })?;
    if !review.valid() {
        return Err(AppError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "invalid_vm_storage_review",
            "provide the exact endpoint, execution generation, physical hash and explicit original-storage confirmation",
            request.0,
        ));
    }
    let id = vms::parse_id(&id, request.0)?;
    let store = state.store.clone();
    let original = review.clone();
    if let Some(receipt) = database(request.0, move || {
        store.vm_delete_storage_review_receipt(id, &original)
    })
    .await?
    {
        return Ok(Json(receipt));
    }
    let store = state.store.clone();
    let intent = database(request.0, move || store.native_vm_delete(id))
        .await?
        .ok_or_else(|| AppError::not_found(request.0))?;
    if intent.storage_proof().is_some() {
        return Err(unavailable(request.0));
    }
    let vm = Uuid::parse_str(&intent.operation.ownership.resource.id)
        .map_err(|_| AppError::internal(request.0))?;
    let _guard = state.lock_vm(vm).await;
    let (root, proof) = candidate(&state, &intent, request.0).await?;
    let store = state.store.clone();
    database(request.0, move || {
        store.review_vm_delete_storage(id, &review, proof, &root)
    })
    .await
    .map(Json)
}

#[cfg(test)]
#[path = "vm_storage_tests.rs"]
mod tests;

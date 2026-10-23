//! Pause/resume retains one VMM and its guest memory. Requests outlive HTTP cancellation.
use axum::{
    Json,
    extract::{Extension, Path, State},
};
use firecrab_api_types::{VmResponse, VmState};
use uuid::Uuid;

use super::vms;
use crate::{artifacts::VmArtifactPaths, error::AppError, server::RequestId, state::AppState};

pub async fn pause_vm(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<VmResponse>, AppError> {
    change(state, request, id, VmState::Paused).await
}

pub async fn resume_vm(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<VmResponse>, AppError> {
    change(state, request, id, VmState::Running).await
}

async fn change(
    state: AppState,
    request: RequestId,
    id: String,
    target: VmState,
) -> Result<Json<VmResponse>, AppError> {
    let id = vms::parse_id(&id, request.0)?;
    tokio::spawn(change_inner(state, request, id, target))
        .await
        .map_err(|_| AppError::internal(request.0))?
}

async fn change_inner(
    state: AppState,
    request: RequestId,
    id: Uuid,
    target: VmState,
) -> Result<Json<VmResponse>, AppError> {
    let _guard = state.lock_vm(id).await;
    // Serialize the ownership check with service deployment/cleanup claims.
    let _services = state.service_mutations.lock().await;
    let store = state.store.clone();
    let owned = tokio::task::spawn_blocking(move || store.vm_has_service_owner(id))
        .await
        .map_err(|_| AppError::internal(request.0))?
        .map_err(|_| AppError::internal(request.0))?;
    drop(_services);
    if owned {
        return Err(AppError::conflict(
            "vm_service_owned",
            "Pause/resume is unavailable for MicroService-owned VMs; use service lifecycle actions",
            request.0,
        ));
    }
    let vm = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&id)
        .cloned()
        .ok_or_else(|| AppError::not_found(request.0))?;
    if vm.purpose != crate::model::VmPurpose::Instance {
        return Err(AppError::conflict(
            "vm_internal",
            "Pause/resume is unavailable for internal image builder VMs",
            request.0,
        ));
    }
    if !matches!(
        vm.state,
        VmState::Running | VmState::Paused | VmState::Pausing | VmState::Resuming
    ) {
        return Err(AppError::invalid_state(vm.state, request.0));
    }
    let runtime_id = vm
        .last_runtime_id
        .ok_or_else(|| AppError::vm_not_running(request.0))?;
    if !state
        .processes
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains_key(&id)
    {
        return Err(AppError::vm_not_running(request.0));
    }
    let runtime =
        VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), id).runtime(runtime_id);
    let client = crate::vm_execution::Client::new(&runtime.api_socket)
        .map_err(|_| runtime_error(request.0))?;
    // Persist uncertainty before changing the VMM. A lost answer never becomes a false stable state.
    let pending = if target == VmState::Paused {
        VmState::Pausing
    } else {
        VmState::Resuming
    };
    let claimed = vms::transition_if(&state, id, vm.state, pending)
        .ok_or_else(|| runtime_error(request.0))?;
    if let Err(error) = vms::persist_update(&state, &claimed, request.0).await {
        vms::transition_if(&state, id, pending, vm.state);
        return Err(error);
    }
    let result = client.set_state(target).await;
    // Also query after transport errors: the VMM may have accepted the request before the answer was lost.
    let actual = client.state().await.map_err(|error| {
        tracing::warn!(vm_id = %id, %error, "cannot verify VM execution state");
        runtime_error(request.0)
    })?;
    let confirmed =
        vms::transition_if(&state, id, pending, actual).ok_or_else(|| runtime_error(request.0))?;
    vms::persist_update(&state, &confirmed, request.0).await?;
    tracing::info!(request_id = %request.0, vm_id = %id, ?actual, "VM execution state confirmed");
    if actual != target {
        tracing::warn!(vm_id = %id, ?result, ?actual, ?target, "VM execution state did not reach target");
        return Err(runtime_error(request.0));
    }
    vms::get_vm(
        State(state.clone()),
        Extension(request),
        Path(id.to_string()),
    )
    .await
}

fn runtime_error(request: Uuid) -> AppError {
    AppError::unavailable(
        "Cannot confirm VM execution state; inspect the VM and retry Pause/Resume or Stop",
        request,
    )
}

#[cfg(test)]
mod tests;

//! Detached, typed VM cleanup. A cancelled HTTP waiter cannot release its durable fence.

use super::{stacks::persistence_error, vms};
use crate::{
    error::AppError, persistence::iac_vm_deletes::VmDeleteIntent, server::RequestId,
    state::AppState,
};
use axum::{
    Extension,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use firecrab_stack::{NativeOperation, NativeOperationState};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Public retry identities never serve as permission to mutate an owned or generated VM.
pub async fn route(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let id = match vms::parse_id(&id, request.0) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let operation = match super::iac::operation_id(&headers, request.0) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let result = detached(
        state,
        request,
        Cleanup {
            vm: id,
            retain_disks: false,
            id: operation,
            stack_operation: None,
            recovery: None,
        },
    )
    .await;
    let mut response = result.into_response();
    response.headers_mut().insert(
        "x-firecrab-operation-id",
        HeaderValue::from_str(&operation.to_string()).expect("UUID header"),
    );
    response
}

/// A parent request may clean up several replicas. Its children need separate stable receipts.
fn child_operation(request: Uuid, vm: Uuid, retain_disks: bool) -> Uuid {
    let mut hash = Sha256::new();
    hash.update(b"firecrab/native-vm-delete/v1");
    hash.update(request.as_bytes());
    hash.update(vm.as_bytes());
    hash.update([u8::from(retain_disks)]);
    let mut bytes: [u8; 16] = hash.finalize()[..16].try_into().expect("SHA256 prefix");
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

pub(crate) async fn execute(
    state: AppState,
    request: RequestId,
    vm: Uuid,
    retain_disks: bool,
    stack_operation: Option<Uuid>,
) -> Result<StatusCode, AppError> {
    let id = child_operation(stack_operation.unwrap_or(request.0), vm, retain_disks);
    detached(
        state,
        request,
        Cleanup {
            vm,
            retain_disks,
            id,
            stack_operation,
            recovery: None,
        },
    )
    .await
}

struct Cleanup {
    vm: Uuid,
    retain_disks: bool,
    id: Uuid,
    stack_operation: Option<Uuid>,
    recovery: Option<VmDeleteIntent>,
}

async fn detached(
    state: AppState,
    request: RequestId,
    cleanup: Cleanup,
) -> Result<StatusCode, AppError> {
    let vm = cleanup.vm;
    let id = cleanup.id;
    let supervisor_state = state.clone();
    let attempt = Arc::new(Mutex::new(cleanup.recovery.clone()));
    let worker = tokio::spawn(run(state, request, cleanup, attempt.clone()));
    // Keep observing panics even after the HTTP receiver disappears.
    tokio::spawn(async move {
        match worker.await {
            Ok(result) => {
                if result.is_err() && let Some(intent) = attempt.lock().unwrap_or_else(|p| p.into_inner()).as_ref()
                    && let Err(error) = supervisor_state.store.fail_native_vm_delete(intent) {
                    tracing::error!(operation_id=%id, %error, "Could not record uncertain VM cleanup");
                }
                result
            },
            Err(error) => {
                tracing::error!(operation_id=%id, %error, "VM cleanup worker interrupted");
                if let Some(intent) = attempt.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
                    let _ = supervisor_state.store.fail_native_vm_delete(intent);
                    // Cleanup removed the cache entry before dispatch; restore only its durable row.
                    if let Ok(records) = supervisor_state.store.load_all()
                        && let Some(record) = records.get(&vm) {
                        supervisor_state.vms.lock().unwrap_or_else(|p| p.into_inner())
                            .entry(vm).or_insert_with(|| record.clone());
                    }
                }
                Err(AppError::internal(request.0))
            }
        }
    }).await.map_err(|_| AppError::internal(request.0))?
}

async fn run(
    state: AppState,
    request: RequestId,
    cleanup: Cleanup,
    attempt: Arc<Mutex<Option<VmDeleteIntent>>>,
) -> Result<StatusCode, AppError> {
    let Cleanup {
        vm,
        retain_disks,
        id,
        stack_operation,
        recovery,
    } = cleanup;
    let _guard = state.lock_vm(vm).await;
    if recovery.is_none()
        && let Some(existing) = state
            .store
            .native_vm_delete(id)
            .map_err(|error| persistence_error(error, request.0))?
    {
        if existing.operation.ownership.resource.id != vm.to_string()
            || existing.retain_disks != retain_disks
        {
            return Err(persistence_error(
                crate::persistence::PersistenceError::IacOperationIdentity,
                request.0,
            ));
        }
        return if existing.operation.state == NativeOperationState::Succeeded {
            Ok(StatusCode::NO_CONTENT)
        } else {
            Err(persistence_error(
                crate::persistence::PersistenceError::IacResourceBusy,
                request.0,
            ))
        };
    }
    let prepared = vms::prepare_vm_delete(&state, request, vm, stack_operation)?;
    let intent = if let Some(intent) = recovery {
        state
            .store
            .validate_native_vm_delete(&intent)
            .map_err(|error| persistence_error(error, request.0))?;
        intent
    } else {
        if let Some(stack) = stack_operation {
            state
                .store
                .begin_stack_vm_delete(&prepared, id, retain_disks, stack)
        } else {
            state
                .store
                .begin_native_vm_delete(&prepared, id, retain_disks)
        }
        .map_err(|error| persistence_error(error, request.0))?
    };
    *attempt.lock().unwrap_or_else(|p| p.into_inner()) = Some(intent.clone());
    vms::delete_vm_admitted(state, request, intent).await
}

pub(crate) async fn recover(
    state: AppState,
    request: RequestId,
    operation: NativeOperation,
) -> Result<NativeOperation, AppError> {
    let vm = Uuid::parse_str(&operation.ownership.resource.id)
        .map_err(|_| AppError::internal(request.0))?;
    let id = operation.operation_id;
    // Recovery admission and dispatch belong to the server after the request is accepted.
    let store = state.store.clone();
    tokio::spawn(async move {
        let intent = store
            .recover_native_vm_delete(id)
            .map_err(|error| persistence_error(error, request.0))?;
        if intent.operation.state != NativeOperationState::Succeeded {
            let retain_disks = intent.retain_disks;
            detached(
                state,
                request,
                Cleanup {
                    vm,
                    retain_disks,
                    id,
                    stack_operation: None,
                    recovery: Some(intent),
                },
            )
            .await?;
        }
        store
            .iac_native_operation(id)
            .map_err(|error| persistence_error(error, request.0))?
            .ok_or_else(|| AppError::internal(request.0))
    })
    .await
    .map_err(|_| AppError::internal(request.0))?
}

/// Releasing an address requires confirmed firewall and TAP removal.
pub(crate) async fn teardown(
    state: &AppState,
    intent: &VmDeleteIntent,
    request: Uuid,
) -> Result<(), AppError> {
    let vm = Uuid::parse_str(&intent.operation.ownership.resource.id)
        .map_err(|_| AppError::internal(request))?;
    let _guard = state.network_mutations.lock().await;
    state
        .store
        .validate_native_vm_delete(intent)
        .map_err(|error| persistence_error(error, request))?;
    let policy = state.network.remove_vm_policy(vm).await;
    state
        .store
        .validate_native_vm_delete(intent)
        .map_err(|error| persistence_error(error, request))?;
    let tap = state.network.delete_tap(vm).await;
    if policy.is_err() || tap.is_err() {
        tracing::error!(vm_id=%vm, ?policy, ?tap, "VM network cleanup is unconfirmed");
        return Err(AppError::conflict(
            "vm_network_cleanup_incomplete",
            "host network cleanup is unconfirmed; recover the original VM operation",
            request,
        ));
    }
    Ok(())
}

/// A stale worker must not restore a cache entry after another attempt removed its row.
pub(crate) fn restore_failed_cleanup(
    state: &AppState,
    removed: crate::model::VmRecord,
    expected: &NativeOperation,
) {
    let id = removed.id;
    let current = state
        .store
        .iac_native_operation(expected.operation_id)
        .ok()
        .flatten();
    let rows = state.store.load_all();
    let mut vms = state.vms.lock().unwrap_or_else(|p| p.into_inner());
    match rows {
        Ok(mut rows) => match rows.remove(&id) {
            Some(record) => {
                vms.insert(
                    id,
                    if current.as_ref() == Some(expected) {
                        removed
                    } else {
                        record
                    },
                );
            }
            None => {
                vms.remove(&id);
            }
        },
        Err(_) if current.as_ref() == Some(expected) => {
            vms.insert(id, removed);
        }
        Err(_) => {}
    }
}

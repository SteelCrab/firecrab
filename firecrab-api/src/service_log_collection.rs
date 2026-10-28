//! Transfers the shim's bounded app-only output into service history.
//!
//! The shim owns capture and keeps writing if the API is unavailable. This
//! reader persists each page with its cursor in one transaction. It never
//! reads the guest's interactive console.

use uuid::Uuid;

use crate::artifacts::VmArtifactPaths;
use crate::model::VmRecord;
use crate::persistence::service_logs::InputLog;
use crate::state::AppState;
use crate::vm_shim::app_log::{self, Status};

const PAGE_SIZE: usize = 512;
const PAGES_PER_PASS: usize = 32;

fn status_name(status: Status) -> &'static str {
    match status {
        Status::Unavailable => "unavailable",
        Status::Waiting => "waiting",
        Status::Active => "active",
        Status::Inactive => "inactive",
        Status::Error => "error",
    }
}

fn collect_runtime(state: &AppState, vm: &VmRecord, final_pass: bool) -> Result<(), ()> {
    let Some(runtime_id) = vm.last_runtime_id else {
        return Ok(());
    };
    let Some(owner) = state
        .store
        .service_deployment_for_vm(vm.id)
        .map_err(|_| ())?
    else {
        return Ok(());
    };
    let service = owner.service_id;
    let paths = VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id);
    let runtime = paths.runtime(runtime_id);
    let mut cursor = state
        .store
        .service_log_cursor(service, vm.id, runtime_id)
        .map_err(|_| ())?;
    for page in 0..PAGES_PER_PASS {
        let batch = match app_log::read(&runtime, cursor, PAGE_SIZE) {
            Ok(batch) => batch,
            Err(_) => {
                let _ = state.store.append_service_logs(
                    service,
                    (vm.id, runtime_id),
                    cursor,
                    "error",
                    true,
                    &[],
                );
                return Err(());
            }
        };
        let count = batch.records.len();
        let has_more = final_pass
            && page + 1 == PAGES_PER_PASS
            && count == PAGE_SIZE
            && match app_log::read(&runtime, batch.next, 1) {
                Ok(next) => !next.records.is_empty(),
                Err(_) => true,
            };
        let lost = batch.truncated || has_more;
        let records: Vec<_> = batch
            .records
            .into_iter()
            .filter(|record| matches!(record.stream.as_str(), "stdout" | "stderr"))
            .map(|record| InputLog {
                sequence: record.sequence,
                timestamp: record.timestamp,
                stream: record.stream,
                message: record.message,
            })
            .collect();
        state
            .store
            .append_service_logs(
                service,
                (vm.id, runtime_id),
                batch.next,
                status_name(batch.status),
                lost,
                &records,
            )
            .map_err(|_| ())?;
        if count < PAGE_SIZE
            || batch.next <= cursor
            || (final_pass && page + 1 == PAGES_PER_PASS && !has_more)
        {
            return Ok(());
        }
        cursor = batch.next;
    }
    if final_pass { Err(()) } else { Ok(()) }
}

/// Collect one VM before its runtime is replaced or deleted.
/// Does not take the VM lifecycle lock because callers may already hold it.
pub(crate) async fn drain_vm(state: &AppState, id: Uuid) -> Result<(), ()> {
    let vm = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&id)
        .cloned();
    let Some(vm) = vm else {
        return Ok(());
    };
    let state = state.clone();
    tokio::task::spawn_blocking(move || collect_runtime(&state, &vm, true))
        .await
        .map_err(|_| ())?
}

/// Record possible loss when final collection cannot finish before cleanup.
pub(crate) async fn mark_loss(state: &AppState, id: Uuid) {
    let vm = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&id)
        .cloned();
    let Some(vm) = vm else {
        return;
    };
    let Some(runtime) = vm.last_runtime_id else {
        return;
    };
    let store = state.store.clone();
    // Resolve the owner before cleanup can remove the deployment. If the
    // caller's deadline expires, dropping this future prevents a later lookup.
    let lookup = store.clone();
    let Ok(Ok(Some(owner))) =
        tokio::task::spawn_blocking(move || lookup.service_deployment_for_vm(id)).await
    else {
        return;
    };
    let _ = tokio::task::spawn_blocking(move || {
        let Ok(cursor) = store.service_log_cursor(owner.service_id, id, runtime) else {
            return;
        };
        let _ =
            store.append_service_logs(owner.service_id, (id, runtime), cursor, "error", true, &[]);
    })
    .await;
}

/// Collect one bounded page range per VM without delaying lifecycle requests.
pub(crate) async fn collect(state: &AppState) {
    let vms: Vec<_> = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .cloned()
        .collect();
    let state = state.clone();
    let _ = tokio::task::spawn_blocking(move || {
        for vm in vms {
            if collect_runtime(&state, &vm, false).is_err() {
                tracing::warn!(vm_id = %vm.id, "Could not collect application logs");
            }
        }
    })
    .await;
}

//! Transfers the shim's bounded app-only output into service history.
//!
//! The shim owns capture and keeps writing if the API is unavailable. This
//! reader persists each page with its cursor in one transaction. It never
//! reads the guest's interactive console.

use uuid::Uuid;

use crate::artifacts::VmArtifactPaths;
use crate::model::VmRecord;
use crate::persistence::Store;
use crate::persistence::service_logs::InputLog;
use crate::state::AppState;
use crate::vm_shim::app_log::{self, Status};

const PAGE_SIZE: usize = 512;
const PAGES_PER_PASS: usize = 32;

/// Start retention independently of collection and export.
pub(crate) fn start_retention(store: Store) {
    tokio::spawn(retention_loop(store, std::time::Duration::from_secs(3600)));
}

async fn retention_loop(store: Store, period: std::time::Duration) {
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let store = store.clone();
        if !matches!(
            tokio::task::spawn_blocking(move || store.prune_service_observations()).await,
            Ok(Ok(()))
        ) {
            tracing::warn!("Could not prune service observation history");
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn retention_runs_without_new_service_activity() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let store = Store::open(&path).unwrap();
        let conn = rusqlite::Connection::open(path).unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        let service = Uuid::new_v4().to_string();
        let old = now - 8 * 24 * 60 * 60 * 1000;
        {
            conn.execute("INSERT INTO service_observation_logs(service_id, vm_id, runtime_id, sequence, timestamp_ms, stream, message) VALUES (?1, 'vm', 'runtime', 1, ?2, 'stdout', 'old')", rusqlite::params![service, old]).unwrap();
            conn.execute("INSERT INTO service_observation_events(service_id, timestamp, kind, severity, message) VALUES (?1, ?2, 'test', 'info', 'old')", rusqlite::params![service, old / 1000]).unwrap();
            conn.execute("INSERT INTO service_observation_cursors(service_id, vm_id, runtime_id, sequence, status, updated_at) VALUES (?1, 'vm', 'runtime', 1, 'inactive', ?2)", rusqlite::params![service, old]).unwrap();
        }
        let worker = tokio::spawn(retention_loop(
            store.clone(),
            std::time::Duration::from_millis(20),
        ));
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let remaining: i64 = conn.query_row("SELECT (SELECT COUNT(*) FROM service_observation_logs) + (SELECT COUNT(*) FROM service_observation_events) + (SELECT COUNT(*) FROM service_observation_cursors)", [], |row| row.get(0)).unwrap();
                if remaining == 0 { break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.expect("retention worker did not remove expired rows");
        worker.abort();
    }
}

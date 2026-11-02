//! Provision and restart a stateful RabbitMQ guest without replacing its disk.
use super::{EngineRecord, IMAGE, RabbitClient, TEMPLATE, password};
use crate::{error::AppError, handlers::vms, model::VmPurpose, server::RequestId, state::AppState};
use axum::{
    Extension,
    extract::{Path, State},
};
use firecrab_api_types::*;
use std::{collections::BTreeMap, time::Duration};
use uuid::Uuid;

/// Build a broker VM request without putting credentials into VM metadata.
fn vm_request(spec: &CreateQueueEngineRequest) -> CreateVmRequest {
    CreateVmRequest {
        name: "firecrab-queue".into(),
        template: TEMPLATE.into(),
        cpu: spec.cpu,
        ram: spec.ram,
        disk_gb: spec.disk_gb,
        egress_policy: EgressPolicy::Internet,
        micro_network_id: spec.micro_network_id,
        security_group_id: None,
        storage_root: spec.storage_root.clone(),
        block_storages: vec![],
        shell_ids: vec![],
        port_forwards: vec![],
        env: BTreeMap::new(),
    }
}
/// Reconcile persisted engine state with its VM and omit private credentials.
fn response(state: &AppState, record: &EngineRecord) -> QueueEngineResponse {
    let mut phase = record.phase;
    let mut error = record.error.clone();
    if phase == QueueEnginePhase::Running {
        let vms = state.vms.lock().unwrap_or_else(|p| p.into_inner());
        match vms.get(&record.vm_id).map(|vm| vm.state) {
            Some(VmState::Running) => {}
            Some(VmState::Created | VmState::Stopped) => phase = QueueEnginePhase::Stopped,
            _ => {
                phase = QueueEnginePhase::Failed;
                error = Some("Queue VM is unavailable; start the existing engine to recover".into())
            }
        }
    }
    QueueEngineResponse {
        vm_id: record.vm_id,
        micro_network_id: record.spec.micro_network_id,
        phase,
        image: IMAGE.into(),
        error,
        log: state.oci_imports.snapshot(TEMPLATE).log,
    }
}
/// Read engine state and verify management readiness for a running broker.
pub(crate) async fn engine_status(
    state: &AppState,
    id: Uuid,
) -> Result<Option<QueueEngineResponse>, AppError> {
    let Some(record) = state
        .store
        .queue_engine()
        .map_err(|_| AppError::internal(id))?
    else {
        return Ok(None);
    };
    let mut result = response(state, &record);
    if result.phase == QueueEnginePhase::Running {
        match RabbitClient::new(&state.store) {
            Ok(client) if client.verify(id).await.is_ok() => {}
            _ => {
                result.phase = QueueEnginePhase::Failed;
                result.error =
                    Some("RabbitMQ is not responding; its VM and data are retained".into());
            }
        }
    }
    Ok(Some(result))
}

/// Admission and dispatch are server-owned, including time spent waiting for SQLite.
pub(crate) async fn operate_engine(
    state: AppState,
    action: firecrab_stack::NativeAction,
    spec: Option<CreateQueueEngineRequest>,
    id: Uuid,
    operation_id: Uuid,
) -> Result<QueueEngineResponse, AppError> {
    tokio::spawn(async move {
        let create = spec.map(|spec| EngineRecord {
            vm_id: Uuid::new_v4(),
            vm_created: false,
            spec,
            phase: QueueEnginePhase::Installing,
            error: None,
            username: "firecrab_admin".into(),
            password: password(),
        });
        let store = state.store.clone();
        let retry_create = create.clone();
        let retry = crate::handlers::stacks::database(id, move || {
            if store.iac_native_operation(operation_id)?.is_none() {
                return Ok(None);
            }
            store
                .begin_queue_engine_operation(operation_id, action, retry_create.as_ref())
                .map(|(record, _, _)| Some(record))
        })
        .await?;
        if let Some(record) = retry {
            return Ok(response(&state, &record));
        }
        let guard = lock(&state, id)?;
        if let Some(record) = &create {
            let mut fields = vms::validate_create(&vm_request(&record.spec), &state);
            fields.remove("template");
            if !(4..=1024).contains(&record.spec.disk_gb) {
                fields.insert("diskGb".into(), "must be between 4 and 1024 GiB".into());
            }
            if !fields.is_empty() {
                return Err(AppError::validation(fields, id));
            }
        }
        let store = state.store.clone();
        let (record, operation, dispatch) = crate::handlers::stacks::database(id, move || {
            store.begin_queue_engine_operation(operation_id, action, create.as_ref())
        })
        .await?;
        let result = response(&state, &record);
        if dispatch {
            tokio::spawn(async move {
                let _guard = guard;
                run_operation(state, record, operation, id).await;
            });
        }
        Ok(result)
    })
    .await
    .map_err(|_| AppError::internal(id))?
}

fn lock(state: &AppState, id: Uuid) -> Result<tokio::sync::OwnedMutexGuard<()>, AppError> {
    state.queue_mutations.clone().try_lock_owned().map_err(|_| {
        AppError::conflict(
            "queue_engine_busy",
            "Queue engine has an operation in progress",
            id,
        )
    })
}

/// Recovery resumes only the original durable action and retains its queue admission.
pub(crate) async fn recover_operation(
    state: AppState,
    id: Uuid,
    operation_id: Uuid,
) -> Result<firecrab_stack::NativeOperation, AppError> {
    tokio::spawn(async move {
        let guard = lock(&state, id)?;
        let store = state.store.clone();
        let (record, operation) = crate::handlers::stacks::database(id, move || {
            store.recover_queue_engine_operation(operation_id)
        })
        .await?;
        let result = operation.clone();
        if operation.state != firecrab_stack::NativeOperationState::Succeeded {
            tokio::spawn(async move {
                let _guard = guard;
                run_operation(state, record, operation, id).await;
            });
        }
        Ok(result)
    })
    .await
    .map_err(|_| AppError::internal(id))?
}

async fn checkpoint(
    state: &AppState,
    record: &EngineRecord,
    id: Uuid,
    operation: &firecrab_stack::NativeOperation,
    finished: bool,
) -> Result<(), AppError> {
    let store = state.store.clone();
    let record = record.clone();
    let operation = operation.clone();
    crate::handlers::stacks::database(id, move || {
        store.checkpoint_queue_engine_operation(&operation, &record, finished)
    })
    .await
    .map(|_| ())
}

async fn stop(state: &AppState, record: &EngineRecord, id: Uuid) -> Result<(), AppError> {
    let vm_state = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&record.vm_id)
        .map(|vm| vm.state);
    if vm_state.is_none() && record.vm_created {
        return Err(AppError::unavailable(
            "Queue VM is missing; retain its original disk for recovery",
            id,
        ));
    }
    if vm_state.is_some_and(|phase| phase.has_live_runtime()) {
        let _ = vms::stop_vm(
            State(state.clone()),
            Extension(RequestId(id)),
            Path(record.vm_id.to_string()),
        )
        .await?;
    }
    Ok(())
}

/// Only runtime-confirmed success releases the ownership fence.
async fn run_operation(
    state: AppState,
    mut record: EngineRecord,
    operation: firecrab_stack::NativeOperation,
    id: Uuid,
) {
    if checkpoint(&state, &record, id, &operation, false)
        .await
        .is_err()
    {
        return;
    }
    let stopping = operation.action == firecrab_stack::NativeAction::StopQueueEngine;
    let result = if stopping {
        stop(&state, &record, id).await
    } else {
        provision(&state, &mut record, id, &operation).await
    };
    match result {
        Ok(()) => {
            record.phase = if stopping {
                QueueEnginePhase::Stopped
            } else {
                QueueEnginePhase::Running
            };
            record.error = None;
        }
        Err(_) => {
            record.phase = QueueEnginePhase::Failed;
            record.error = Some("Queue engine operation is incomplete; recover the original operation with the retained VM and disk".into());
        }
    }
    if checkpoint(&state, &record, id, &operation, true)
        .await
        .is_err()
    {
        tracing::error!(operation_id = %operation.operation_id, "could not commit Queue engine operation result; authority remains reserved");
    }
}
/// Import on first use, checkpoint ownership and start only the reserved VM.
async fn provision(
    state: &AppState,
    record: &mut EngineRecord,
    id: Uuid,
    operation: &firecrab_stack::NativeOperation,
) -> Result<(), AppError> {
    let existing = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&record.vm_id)
        .cloned();
    if existing
        .as_ref()
        .is_some_and(|vm| vm.purpose != VmPurpose::Queue)
    {
        return Err(AppError::conflict(
            "queue_vm_mismatch",
            "The configured VM does not belong to the Queue engine",
            id,
        ));
    }
    if existing.is_none() {
        if record.vm_created {
            return Err(AppError::unavailable(
                "Queue VM is missing; restore its records and disk instead of creating an empty replacement",
                id,
            ));
        }
        if state.templates.resolve_alias(TEMPLATE).is_none() {
            record.phase = QueueEnginePhase::Installing;
            checkpoint(state, record, id, operation, false).await?;
            state
                .oci_imports
                .begin_with(TEMPLATE, "Installing the internal RabbitMQ image")
                .map_err(|_| {
                    AppError::conflict(
                        "queue_image_busy",
                        "RabbitMQ image import is already running",
                        id,
                    )
                })?;
            let reference =
                crate::oci::ImageReference::parse(IMAGE).map_err(|_| AppError::internal(id))?;
            crate::oci::run_oci_import(
                state.oci_imports.clone(),
                state.templates.as_ref().clone(),
                reference,
                TEMPLATE.into(),
                None,
            )
            .await;
        }
        let template = state.templates.resolve_alias(TEMPLATE).ok_or_else(|| {
            AppError::unavailable(
                "RabbitMQ image import failed; inspect the engine import log and retry Start",
                id,
            )
        })?;
        let request = vm_request(&record.spec);
        let fields = vms::validate_create(&request, state);
        if !fields.is_empty() {
            return Err(AppError::validation(fields, id));
        }
        checkpoint(state, record, id, operation, false).await?;
        vms::insert_vm(
            state,
            RequestId(id),
            record.vm_id,
            request,
            &template,
            VmPurpose::Queue,
        )
        .await?;
    }
    // A restart may find the VM after insertion but before this checkpoint.
    // Persist ownership before RabbitMQ can accept its first message.
    record.vm_created = true;
    record.phase = QueueEnginePhase::Starting;
    checkpoint(state, record, id, operation, false).await?;
    if let Some(vm) = &existing
        && let Some(generation) = vm.disk_generation
        && !crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id)
            .rootfs(generation)
            .is_file()
    {
        return Err(AppError::unavailable(
            "Queue disk is missing; restore its original disk instead of creating an empty replacement",
            id,
        ));
    }
    let vm_state = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&record.vm_id)
        .map(|vm| vm.state);
    if vm_state != Some(VmState::Running) {
        let _ = vms::start_vm(
            State(state.clone()),
            Extension(RequestId(id)),
            Path(record.vm_id.to_string()),
        )
        .await?;
    }
    let client =
        RabbitClient::new(&state.store).map_err(|message| AppError::unavailable(message, id))?;
    let ready = tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            if client.verify(id).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await;
    ready.map_err(|_| {
        AppError::unavailable(
            "RabbitMQ did not become ready; the VM and its data are retained for recovery",
            id,
        )
    })?;
    Ok(())
}

/// Live VM reconciliation runs first. A broker is never replaced after an API restart.
pub(crate) async fn recover(state: &AppState) {
    let Ok(Some(mut record)) = state.store.queue_engine() else {
        return;
    };
    let observed_vm = !record.vm_created
        && state
            .vms
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&record.vm_id)
            .is_some_and(|vm| vm.purpose == VmPurpose::Queue);
    if observed_vm {
        record.vm_created = true;
    }
    let interrupted = matches!(
        record.phase,
        QueueEnginePhase::Installing | QueueEnginePhase::Starting | QueueEnginePhase::Stopping
    );
    if interrupted {
        record.phase = QueueEnginePhase::Failed;
        record.error = Some(
            "API restart interrupted Queue setup; use Start to recover the existing engine".into(),
        );
    }
    if (observed_vm || interrupted) && state.store.save_queue_engine(&record).is_err() {
        tracing::error!("could not save recovered Queue engine ownership");
    }
    if let Ok(queues) = state.store.list_queues() {
        for mut queue in queues
            .into_iter()
            .filter(|q| q.phase == QueuePhase::Provisioning)
        {
            queue.phase = QueuePhase::Failed;
            queue.error = Some("API restart interrupted Queue setup; retry provisioning".into());
            let _ = state.store.save_queue(&queue);
        }
    }
}

use super::*;
use crate::persistence::{PersistenceError, Store};
use crate::queues::EngineRecord;
use firecrab_api_types::{CreateQueueEngineRequest, QueueEnginePhase};
use firecrab_stack::{NativeAction, NativeOperationState};

fn broker(network: Uuid) -> EngineRecord {
    EngineRecord {
        vm_id: Uuid::new_v4(),
        vm_created: false,
        spec: CreateQueueEngineRequest {
            micro_network_id: network,
            cpu: 1,
            ram: 512,
            disk_gb: 8,
            storage_root: None,
        },
        phase: QueueEnginePhase::Stopped,
        error: None,
        username: "private-admin".into(),
        password: "private-broker-password".into(),
    }
}
fn resource(engine: &EngineRecord) -> ManagedResource {
    ManagedResource {
        kind: "queue_engine".into(),
        id: engine.vm_id.to_string(),
    }
}

#[tokio::test]
async fn iac_queue_engine_transfer_preserves_identity_and_blocks_public_lifecycle() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let engine = broker(network);
    state.store.save_queue_engine(&engine).unwrap();
    let target = resource(&engine);
    let wrong = ManagedResource {
        kind: target.kind.clone(),
        id: Uuid::new_v4().to_string(),
    };
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/iac/owners/transition",
            claim_configuration(&state, &wrong)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let claim = claim_configuration(&state, &target);
    let (code, owned) = call(&app, "POST", "/api/iac/owners/transition", claim).await;
    assert_eq!(code, StatusCode::OK, "{owned}");
    for path in ["/api/queue/engine/start", "/api/queue/engine/stop"] {
        let (code, error) = call(&app, "POST", path, Value::Null).await;
        assert_eq!(code, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["error"]["code"], "iac_ownership_conflict");
    }
    assert!(
        state
            .store
            .iac_native_operations(&target)
            .unwrap()
            .is_empty()
    );
    for (method, suffix) in [
        ("POST", "/start"),
        ("POST", "/stop"),
        ("PATCH", ""),
        ("DELETE", ""),
    ] {
        let (code, error) = call(
            &app,
            method,
            &format!("/api/vms/{}{suffix}", engine.vm_id),
            json!({"cpu":2}),
        )
        .await;
        assert_eq!(code, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["error"]["code"], "iac_parent_managed");
    }
    let release = json!({"operationId":Uuid::new_v4(),"expectedEndpointId":owned["endpointId"],"resource":owned["resource"],"expectedGeneration":owned["generation"],"expectedOwner":owned["owner"],"owner":null});
    assert_eq!(
        call(&app, "POST", "/api/iac/owners/transition", release)
            .await
            .0,
        StatusCode::OK
    );
    let saved = state.store.queue_engine().unwrap().unwrap();
    assert_eq!(saved.vm_id, engine.vm_id);
    assert_eq!(saved.spec, engine.spec);
    assert_eq!(saved.password, engine.password);
    assert_eq!(saved.phase, QueueEnginePhase::Stopped);
    assert!(!owned.to_string().contains("private"));
}

#[tokio::test]
async fn iac_queue_engine_installer_retries_keep_the_original_vm_and_private_credentials() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let engine = broker(network);
    let id = Uuid::new_v4();
    let (_, operation, dispatch) = state
        .store
        .begin_queue_engine_operation(id, NativeAction::InstallQueueEngine, Some(&engine))
        .unwrap();
    assert!(dispatch);
    let replacement = broker(network);
    let (saved, retried, dispatch) = state
        .store
        .begin_queue_engine_operation(id, NativeAction::InstallQueueEngine, Some(&replacement))
        .unwrap();
    assert!(!dispatch);
    assert_eq!(saved.vm_id, engine.vm_id);
    assert_eq!(saved.password, engine.password);
    assert_eq!(retried, operation);
    let mut changed = replacement;
    changed.spec.cpu += 1;
    assert!(matches!(
        state.store.begin_queue_engine_operation(
            id,
            NativeAction::InstallQueueEngine,
            Some(&changed)
        ),
        Err(PersistenceError::IacOperationIdentity)
    ));
    assert!(matches!(
        state
            .store
            .begin_queue_engine_operation(id, NativeAction::StopQueueEngine, None),
        Err(PersistenceError::IacOperationIdentity)
    ));
    let transfer: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &resource(&engine))).unwrap();
    assert!(matches!(
        state.store.change_iac_resource_owner(&transfer),
        Err(PersistenceError::IacResourceBusy)
    ));
    assert!(
        !serde_json::to_string(&operation)
            .unwrap()
            .contains("private")
    );
}

#[tokio::test]
async fn iac_queue_engine_recovery_retires_old_workers_and_keeps_completed_retries_immutable() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let engine = broker(network);
    state.store.save_queue_engine(&engine).unwrap();
    let id = Uuid::new_v4();
    let (mut record, old, _) = state
        .store
        .begin_queue_engine_operation(id, NativeAction::StartQueueEngine, None)
        .unwrap();
    record.vm_created = true;
    state
        .store
        .checkpoint_queue_engine_operation(&old, &record, false)
        .unwrap();
    let second = Store::open(&root.path().join("state.db")).unwrap();
    second.interrupt_native_iac_operations().unwrap();
    let interrupted = second.iac_native_operation(id).unwrap().unwrap();
    assert_eq!(interrupted.state, NativeOperationState::NeedsRecovery);
    let (mut recovered, current) = second.recover_queue_engine_operation(id).unwrap();
    assert_eq!(current.execution_generation, old.execution_generation + 1);
    assert!(recovered.vm_created);
    assert_eq!(recovered.vm_id, engine.vm_id);
    assert_eq!(recovered.password, engine.password);
    record.phase = QueueEnginePhase::Failed;
    assert!(matches!(
        state
            .store
            .checkpoint_queue_engine_operation(&old, &record, true),
        Err(PersistenceError::StaleIacOwner)
    ));
    recovered.phase = QueueEnginePhase::Running;
    let done = second
        .checkpoint_queue_engine_operation(&current, &recovered, true)
        .unwrap();
    assert_eq!(done.state, NativeOperationState::Succeeded);
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &resource(&engine))).unwrap();
    state.store.change_iac_resource_owner(&claim).unwrap();
    let (_, receipt, dispatch) = second
        .begin_queue_engine_operation(id, NativeAction::StartQueueEngine, None)
        .unwrap();
    assert!(!dispatch);
    assert_eq!(receipt, done);
    assert_eq!(state.store.iac_native_operation(id).unwrap(), Some(done));
}

#[tokio::test]
async fn iac_queue_engine_receipt_failure_rolls_back_the_reserved_identity() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let connection = rusqlite::Connection::open(root.path().join("state.db")).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_engine_receipt BEFORE INSERT ON iac_native_operations BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;").unwrap();
    let engine = broker(network);
    let id = Uuid::new_v4();
    assert!(
        state
            .store
            .begin_queue_engine_operation(id, NativeAction::InstallQueueEngine, Some(&engine))
            .is_err()
    );
    assert!(state.store.queue_engine().unwrap().is_none());
    assert!(state.store.iac_native_operation(id).unwrap().is_none());
    assert!(
        state
            .store
            .iac_native_operations(&resource(&engine))
            .unwrap()
            .is_empty()
    );
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM queue_engine_requests", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn iac_queue_engine_native_stop_survives_a_disconnected_http_waiter() {
    use std::{future::Future, task::Poll};
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let engine = broker(network);
    state.store.save_queue_engine(&engine).unwrap();
    let blocked = rusqlite::Connection::open(root.path().join("state.db")).unwrap();
    blocked.execute_batch("BEGIN IMMEDIATE").unwrap();
    let id = Uuid::new_v4();
    let mut waiter = Box::pin(crate::queues::operate_engine(
        state.clone(),
        NativeAction::StopQueueEngine,
        None,
        Uuid::new_v4(),
        id,
    ));
    assert!(
        std::future::poll_fn(|cx| Poll::Ready(matches!(waiter.as_mut().poll(cx), Poll::Pending)))
            .await
    );
    drop(waiter);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if state.queue_mutations.try_lock().is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    blocked.execute_batch("ROLLBACK").unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if state
                .store
                .iac_native_operation(id)
                .unwrap()
                .is_some_and(|operation| operation.state == NativeOperationState::Succeeded)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let saved = state.store.queue_engine().unwrap().unwrap();
    assert_eq!(saved.phase, QueueEnginePhase::Stopped);
    assert_eq!(saved.vm_id, engine.vm_id);
    assert_eq!(saved.password, engine.password);
}

#[tokio::test]
async fn iac_queue_engine_http_retry_does_not_redispatch_or_accept_a_different_action() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let engine = broker(network);
    state.store.save_queue_engine(&engine).unwrap();
    let id = Uuid::new_v4();
    let (_, original, _) = state
        .store
        .begin_queue_engine_operation(id, NativeAction::StopQueueEngine, None)
        .unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/queue/engine/stop")
                .header("x-firecrab-operation-id", id.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(
        response.headers()["x-firecrab-operation-id"],
        id.to_string()
    );
    assert_eq!(
        state.store.iac_native_operation(id).unwrap(),
        Some(original)
    );
    for (path, operation) in [
        ("/api/queue/engine/start", id.to_string()),
        ("/api/queue/engine/stop", "not-a-uuid".into()),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("x-firecrab-operation-id", operation)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(matches!(
            response.status(),
            StatusCode::CONFLICT | StatusCode::BAD_REQUEST
        ));
    }
    assert_eq!(
        state
            .store
            .iac_native_operations(&resource(&engine))
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn iac_queue_engine_admission_and_transfer_have_one_winner_across_sqlite_connections() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let engine = broker(network);
    state.store.save_queue_engine(&engine).unwrap();
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &resource(&engine))).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let gate = barrier.clone();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let op_id = Uuid::new_v4();
    let task = std::thread::spawn(move || {
        gate.wait();
        store
            .begin_queue_engine_operation(op_id, NativeAction::StopQueueEngine, None)
            .is_ok()
    });
    barrier.wait();
    let transferred = state.store.change_iac_resource_owner(&claim).is_ok();
    let admitted = task.join().unwrap();
    assert_ne!(transferred, admitted);
    assert_eq!(
        state.store.iac_native_operation(op_id).unwrap().is_some(),
        admitted
    );
    assert_eq!(
        state
            .store
            .iac_ownership_operation(claim.operation_id)
            .unwrap()
            .is_some(),
        transferred
    );
}

#[tokio::test]
async fn iac_queue_engine_transfer_rejects_busy_parent_and_pending_queue_provisioning() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let mut engine = broker(network);
    state.store.save_queue_engine(&engine).unwrap();
    let target = resource(&engine);
    let claim = claim_configuration(&state, &target);
    let queue = configuration_resource(&state, root.path(), network, "queue");
    let mut pending = state
        .store
        .queue(Uuid::parse_str(&queue.id).unwrap())
        .unwrap()
        .unwrap();
    pending.phase = firecrab_api_types::QueuePhase::Provisioning;
    state.store.save_queue(&pending).unwrap();
    let (status, error) = call(&app, "POST", "/api/iac/owners/transition", claim.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "iac_resource_busy");
    pending.phase = firecrab_api_types::QueuePhase::Ready;
    state.store.save_queue(&pending).unwrap();
    engine.phase = QueueEnginePhase::Starting;
    state.store.save_queue_engine(&engine).unwrap();
    let (status, error) = call(&app, "POST", "/api/iac/owners/transition", claim.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "iac_resource_busy");
    engine.phase = QueueEnginePhase::Stopped;
    state.store.save_queue_engine(&engine).unwrap();
    let guard = state.queue_mutations.lock().await;
    let (status, error) = call(&app, "POST", "/api/iac/owners/transition", claim.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "iac_resource_busy");
    drop(guard);
    assert!(
        state
            .store
            .iac_ownership_operation(
                Uuid::parse_str(claim["operationId"].as_str().unwrap()).unwrap()
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(
        call(&app, "POST", "/api/iac/owners/transition", claim)
            .await
            .0,
        StatusCode::OK
    );
}

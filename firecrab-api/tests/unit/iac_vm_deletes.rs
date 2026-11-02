use super::*;
use crate::{
    handlers::vms::test_support::{record, seed_vm, test_state},
    persistence::{PersistenceError, Store},
};
use firecrab_stack::{NativeAction, NativeOperationState};

async fn wait_state(state: &AppState, id: Uuid, wanted: NativeOperationState) {
    tokio::time::timeout(std::time::Duration::from_secs(4), async {
        loop {
            if state
                .store
                .iac_native_operation(id)
                .unwrap()
                .is_some_and(|op| op.state == wanted)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn iac_vm_delete_survives_lost_waiter_and_retires_identity_and_lease() {
    let root = crate::firecracker::test_support::short_tempdir();
    let state = test_state(root.path()).await;
    let vm = record("owned-cleanup", Uuid::new_v4());
    seed_vm(&state, &vm);
    let artifacts =
        crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id).dir;
    std::fs::create_dir_all(&artifacts).unwrap();
    std::fs::write(artifacts.join("rootfs.img"), b"private VM disk").unwrap();
    let lease = state.store.active_lease(vm.id).unwrap();
    let gate = state.network_mutations.lock().await;
    let id = Uuid::new_v4();
    let request = crate::server::RequestId(Uuid::new_v4());
    let task_state = state.clone();
    let task = tokio::spawn(async move {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-firecrab-operation-id", id.to_string().parse().unwrap());
        crate::handlers::vm_deletion::route(
            axum::extract::State(task_state),
            axum::Extension(request),
            headers,
            axum::extract::Path(vm.id.to_string()),
        )
        .await
    });
    wait_state(&state, id, NativeOperationState::Running).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(state.store.active_lease(vm.id).unwrap(), lease);
    assert!(artifacts.join("rootfs.img").is_file());
    let second = Store::open(&root.path().join("data/firecrab.db")).unwrap();
    assert!(matches!(
        second.update(&vm),
        Err(PersistenceError::IacResourceBusy)
    ));
    assert!(matches!(
        second.delete(vm.id),
        Err(PersistenceError::IacResourceBusy)
    ));
    let target = ManagedResource {
        kind: "vm".into(),
        id: vm.id.to_string(),
    };
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &target)).unwrap();
    assert!(matches!(
        second.change_iac_resource_owner(&claim),
        Err(PersistenceError::IacResourceBusy)
    ));
    drop(gate);
    wait_state(&state, id, NativeOperationState::Succeeded).await;
    assert!(!artifacts.exists());
    assert!(!second.load_all().unwrap().contains_key(&vm.id));
    assert!(second.active_lease(vm.id).unwrap().is_none());
    assert!(matches!(
        second.insert(&vm),
        Err(PersistenceError::IacOperationIdentity)
    ));
    assert!(matches!(
        second.allocate_lease(
            vm.id,
            crate::ipam::SubnetSpec::legacy_default_subnet(vm.micro_network_id)
        ),
        Err(crate::ipam::IpamError::VmUnavailable { .. })
    ));
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-firecrab-operation-id", id.to_string().parse().unwrap());
    let response = crate::handlers::vm_deletion::route(
        axum::extract::State(state.clone()),
        axum::Extension(request),
        headers,
        axum::extract::Path(vm.id.to_string()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response.headers()["x-firecrab-operation-id"],
        id.to_string()
    );
    let operation = state.store.iac_native_operation(id).unwrap().unwrap();
    assert_eq!(operation.action, NativeAction::DeleteVm);
    assert_eq!(operation.execution_generation, 0);
    assert!(
        !serde_json::to_string(&operation)
            .unwrap()
            .contains(root.path().to_str().unwrap())
    );
}

#[tokio::test]
async fn iac_vm_delete_metadata_failure_retains_authority_until_same_operation_recovers() {
    let root = crate::firecracker::test_support::short_tempdir();
    let state = test_state(root.path()).await;
    let vm = record("failed-cleanup", Uuid::new_v4());
    seed_vm(&state, &vm);
    let artifacts =
        crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id).dir;
    std::fs::create_dir_all(&artifacts).unwrap();
    std::fs::write(artifacts.join("rootfs.img"), b"private VM disk").unwrap();
    let before = state.store.active_lease(vm.id).unwrap();
    let revision = state.store.lease_revision().unwrap();
    let db = rusqlite::Connection::open(root.path().join("data/firecrab.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER reject_vm_receipt BEFORE UPDATE ON iac_native_operations
        WHEN NEW.state='succeeded' BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END",
    )
    .unwrap();
    let request = crate::server::RequestId(Uuid::new_v4());
    assert!(
        crate::handlers::vms::delete_vm(
            axum::extract::State(state.clone()),
            axum::Extension(request),
            axum::extract::Path(vm.id.to_string())
        )
        .await
        .is_err()
    );
    let target = ManagedResource {
        kind: "vm".into(),
        id: vm.id.to_string(),
    };
    let op = state
        .store
        .iac_native_operations(&target)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(op.state, NativeOperationState::NeedsRecovery);
    assert!(state.store.load_all().unwrap().contains_key(&vm.id));
    assert!(state.vms.lock().unwrap().contains_key(&vm.id));
    assert_eq!(state.store.active_lease(vm.id).unwrap(), before);
    assert_eq!(state.store.lease_revision().unwrap(), revision);
    assert!(!artifacts.exists()); // The authorized filesystem effect is not rolled back or recreated.
    assert!(
        crate::handlers::vms::delete_vm(
            axum::extract::State(state.clone()),
            axum::Extension(request),
            axum::extract::Path(vm.id.to_string())
        )
        .await
        .is_err()
    );
    db.execute_batch("DROP TRIGGER reject_vm_receipt").unwrap();
    let completed = crate::handlers::vm_deletion::recover(state.clone(), request, op.clone())
        .await
        .unwrap();
    assert_eq!(completed.operation_id, op.operation_id);
    assert_eq!(completed.state, NativeOperationState::Succeeded);
    assert_eq!(completed.execution_generation, 1);
    assert!(state.store.active_lease(vm.id).unwrap().is_none());
    assert!(!state.store.load_all().unwrap().contains_key(&vm.id));
    assert!(!artifacts.exists());
}

#[tokio::test]
async fn iac_vm_delete_stale_attempt_and_changed_retention_cannot_finish_or_replay() {
    let root = crate::firecracker::test_support::short_tempdir();
    let state = test_state(root.path()).await;
    let vm = record("attempt-fences", Uuid::new_v4());
    seed_vm(&state, &vm);
    let id = Uuid::new_v4();
    let first = state.store.begin_native_vm_delete(&vm, id, true).unwrap();
    state.store.fail_native_vm_delete(&first).unwrap();
    let recovered = state.store.recover_native_vm_delete(id).unwrap();
    assert!(recovered.retain_disks);
    assert_eq!(recovered.operation.execution_generation, 1);
    assert!(matches!(
        state.store.finish_native_vm_delete(&first),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert!(matches!(
        state.store.fail_native_vm_delete(&first),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert!(matches!(
        state.store.begin_native_vm_delete(&vm, id, false),
        Err(PersistenceError::IacOperationIdentity)
    ));
    let other = record("another-vm", Uuid::new_v4());
    assert!(matches!(
        state.store.begin_native_vm_delete(&other, id, true),
        Err(PersistenceError::IacOperationIdentity)
    ));
    state.store.finish_native_vm_delete(&recovered).unwrap();
    assert!(matches!(
        state.store.fail_native_vm_delete(&recovered),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert_eq!(
        state
            .store
            .native_vm_delete(id)
            .unwrap()
            .unwrap()
            .operation
            .state,
        NativeOperationState::Succeeded
    );
}

#[tokio::test]
async fn iac_vm_delete_retention_preserves_exact_private_disk_and_parent_requests_have_distinct_receipts()
 {
    let root = crate::firecracker::test_support::short_tempdir();
    let state = test_state(root.path()).await;
    let request = crate::server::RequestId(Uuid::new_v4());
    for name in ["retained-one", "retained-two"] {
        let vm = record(name, Uuid::new_v4());
        seed_vm(&state, &vm);
        let artifacts =
            crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id)
                .dir;
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(artifacts.join("rootfs.img"), name).unwrap();
        assert_eq!(
            crate::handlers::vms::delete_vm_with_disk_policy(
                state.clone(),
                request,
                vm.id,
                true,
                None
            )
            .await
            .unwrap(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            std::fs::read(artifacts.join("rootfs.img")).unwrap(),
            name.as_bytes()
        );
        let target = ManagedResource {
            kind: "vm".into(),
            id: vm.id.to_string(),
        };
        let operations = state.store.iac_native_operations(&target).unwrap();
        assert_eq!(operations.len(), 1);
        assert!(
            state
                .store
                .native_vm_delete(operations[0].operation_id)
                .unwrap()
                .unwrap()
                .retain_disks
        );
        assert!(!state.store.load_all().unwrap().contains_key(&vm.id));
    }
}

#[tokio::test]
async fn iac_vm_delete_fence_blocks_stack_admission_even_when_its_vm_is_not_in_the_new_plan() {
    let root = crate::firecracker::test_support::short_tempdir();
    let state = test_state(root.path()).await;
    let vm = record("stack-cleanup-fence", Uuid::new_v4());
    seed_vm(&state, &vm);
    let stack = state.store.register_stack("cleanup-owner").unwrap();
    for (kind, key, id) in [
        (firecrab_stack::ResourceKind::Vm, "machine", vm.id),
        (
            firecrab_stack::ResourceKind::Network,
            "network",
            vm.micro_network_id,
        ),
    ] {
        state
            .store
            .bind_stack_resource(
                &stack.name,
                &firecrab_stack::Binding {
                    kind,
                    key: key.into(),
                    resource_id: id,
                },
            )
            .unwrap();
    }
    let stack = state.store.stack(&stack.name).unwrap().unwrap();
    let intent = state
        .store
        .begin_native_vm_delete(&vm, Uuid::new_v4(), false)
        .unwrap();
    let status:firecrab_stack::ApplyStatus=serde_json::from_value(json!({"operationId":Uuid::new_v4(),"stackId":stack.id,"name":stack.name,
        "state":"queued","changes":[{"kind":"network","key":"network","resourceId":vm.micro_network_id,
        "action":"update","fields":["internetEnabled"],"reason":null,"state":"pending"}],"error":null})).unwrap();
    for _ in 0..2 {
        assert!(matches!(
            state
                .store
                .begin_stack_apply(&stack, "pending-network-change", false, &status, None),
            Err(PersistenceError::IacResourceBusy)
        ));
        assert!(
            state
                .store
                .stored_stack_apply(status.operation_id)
                .unwrap()
                .is_none()
        );
        if state
            .store
            .iac_native_operation(intent.operation.operation_id)
            .unwrap()
            .unwrap()
            .state
            == NativeOperationState::Running
        {
            state.store.fail_native_vm_delete(&intent).unwrap();
        }
    }
}

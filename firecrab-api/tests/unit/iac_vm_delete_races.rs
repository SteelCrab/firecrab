use super::*;
use crate::{
    handlers::vms::test_support::{record, seed_vm, test_state},
    persistence::{PersistenceError, Store},
};
use firecrab_stack::{Manager, NativeOperationState, ResourceOwner};

fn member(
    state: &AppState,
    root: &std::path::Path,
    vm: &crate::model::VmRecord,
) -> ManagedResource {
    let pool = configuration_resource(state, root, vm.micro_network_id, "pool");
    let db = rusqlite::Connection::open(root.join("data/firecrab.db")).unwrap();
    db.execute(
        "INSERT INTO vm_pool_members(vm_id,pool_id,state,created_at_ms) VALUES(?1,?2,'ready',1)",
        rusqlite::params![vm.id.to_string(), pool.id],
    )
    .unwrap();
    pool
}

#[tokio::test]
async fn iac_vm_delete_parent_lease_blocks_transfer_through_failure_and_allows_it_after_completion()
{
    let root = crate::firecracker::test_support::short_tempdir();
    let state = test_state(root.path()).await;
    let mut vm = record("generated-parent", Uuid::new_v4());
    vm.purpose = crate::model::VmPurpose::Pool;
    seed_vm(&state, &vm);
    let parent = member(&state, root.path(), &vm);
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &parent)).unwrap();
    let owned = state.store.change_iac_resource_owner(&claim).unwrap();
    let intent = state
        .store
        .begin_native_vm_delete(&vm, Uuid::new_v4(), false)
        .unwrap();
    assert!(intent.operation.ownership.owner.is_none());
    let second = Store::open(&root.path().join("data/firecrab.db")).unwrap();
    let release = ChangeResourceOwner {
        operation_id: Uuid::new_v4(),
        expected_endpoint_id: owned.endpoint_id,
        resource: parent,
        expected_generation: owned.generation,
        expected_owner: owned.owner,
        owner: None,
    };
    assert!(matches!(
        second.change_iac_resource_owner(&release),
        Err(PersistenceError::IacResourceBusy)
    ));
    state.store.fail_native_vm_delete(&intent).unwrap();
    assert!(matches!(
        second.change_iac_resource_owner(&release),
        Err(PersistenceError::IacResourceBusy)
    ));
    let resumed = state
        .store
        .recover_native_vm_delete(intent.operation.operation_id)
        .unwrap();
    state.store.finish_native_vm_delete(&resumed).unwrap();
    second.change_iac_resource_owner(&release).unwrap();
}

#[tokio::test]
async fn iac_vm_delete_generated_child_blocks_stack_work_via_parent_ownership() {
    let root = crate::firecracker::test_support::short_tempdir();
    let state = test_state(root.path()).await;
    let mut vm = record("generated-stack-parent", Uuid::new_v4());
    vm.purpose = crate::model::VmPurpose::Pool;
    seed_vm(&state, &vm);
    let parent = member(&state, root.path(), &vm);
    let stack = state.store.register_stack("generated-owner").unwrap();
    let snapshot = state.store.iac_resource_owner(&parent).unwrap();
    state
        .store
        .change_iac_resource_owner(&ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: snapshot.endpoint_id,
            resource: parent,
            expected_generation: snapshot.generation,
            expected_owner: snapshot.owner,
            owner: Some(ResourceOwner {
                manager: Manager::Stack,
                id: stack.id,
                key: "workers".into(),
            }),
        })
        .unwrap();
    state
        .store
        .bind_stack_resource(
            &stack.name,
            &firecrab_stack::Binding {
                kind: firecrab_stack::ResourceKind::Network,
                key: "network".into(),
                resource_id: vm.micro_network_id,
            },
        )
        .unwrap();
    let stack = state.store.stack(&stack.name).unwrap().unwrap();
    let intent = state
        .store
        .begin_native_vm_delete(&vm, Uuid::new_v4(), false)
        .unwrap();
    assert!(intent.operation.ownership.owner.is_none());
    let status:firecrab_stack::ApplyStatus=serde_json::from_value(json!({"operationId":Uuid::new_v4(),"stackId":stack.id,
        "name":stack.name,"state":"queued","changes":[{"kind":"network","key":"network","resourceId":vm.micro_network_id,
        "action":"update","fields":["internetEnabled"],"state":"pending","reason":null}],"error":null})).unwrap();
    assert!(matches!(
        state
            .store
            .begin_stack_apply(&stack, "network-change", false, &status, None),
        Err(PersistenceError::IacResourceBusy)
    ));
    assert!(
        state
            .store
            .stored_stack_apply(status.operation_id)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn iac_vm_delete_old_worker_cannot_dispatch_or_restore_a_vm_after_recovery_elsewhere() {
    let root = crate::firecracker::test_support::short_tempdir();
    let state = test_state(root.path()).await;
    let vm = record("stale-cleanup-worker", Uuid::new_v4());
    seed_vm(&state, &vm);
    let artifacts =
        crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id);
    std::fs::create_dir_all(&artifacts.dir).unwrap();
    std::fs::write(artifacts.dir.join("private.ext4"), b"exact private scope").unwrap();
    let gate = state.network_mutations.lock().await;
    let id = Uuid::new_v4();
    let original = state.clone();
    let target = vm.id;
    let pending = tokio::spawn(async move {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-firecrab-operation-id", id.to_string().parse().unwrap());
        crate::handlers::vm_deletion::route(
            axum::extract::State(original),
            axum::Extension(crate::server::RequestId(Uuid::new_v4())),
            headers,
            axum::extract::Path(target.to_string()),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(4), async {
        loop {
            if state.store.iac_native_operation(id).unwrap().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let original_attempt = state.store.iac_native_operation(id).unwrap().unwrap();
    let second = test_state(root.path()).await;
    second.store.interrupt_native_iac_operations().unwrap();
    let interrupted = second.store.iac_native_operation(id).unwrap().unwrap();
    let completed = crate::handlers::vm_deletion::recover(
        second.clone(),
        crate::server::RequestId(Uuid::new_v4()),
        interrupted,
    )
    .await
    .unwrap();
    assert_eq!(completed.state, NativeOperationState::Succeeded);
    assert_eq!(completed.execution_generation, 1);
    assert!(!artifacts.dir.exists());
    drop(gate);
    assert_eq!(pending.await.unwrap().status(), StatusCode::CONFLICT);
    assert_eq!(
        state.store.iac_native_operation(id).unwrap().unwrap(),
        completed
    );
    assert!(!state.store.load_all().unwrap().contains_key(&vm.id));
    // Exercise the late filesystem-failure continuation with its original authority.
    // The earlier checkpoint may have rejected this worker before it removed its cache entry.
    crate::handlers::vm_deletion::restore_failed_cleanup(&state, vm.clone(), &original_attempt);
    assert!(!state.vms.lock().unwrap().contains_key(&vm.id));
}

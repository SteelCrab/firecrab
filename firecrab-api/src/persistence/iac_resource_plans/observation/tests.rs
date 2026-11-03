use super::*;

fn shell(store: &Store) -> ResourcePlanReceipt {
    let request: PlanResource = serde_json::from_value(serde_json::json!({
        "operationId":Uuid::new_v4(),"expectedEndpointId":store.iac_endpoint_id().unwrap(),
        "owner":{"manager":"terraform","id":Uuid::new_v4(),"key":"coherent"},"resource":null,
        "desired":{"kind":"shell","configuration":{"name":"coherent","content":"echo ready"}}
    }))
    .unwrap();
    let plan = store.plan_resource(request, 100).unwrap();
    store
        .apply_resource_plan(
            plan.operation_id,
            &ApplyResourcePlan {
                expected_endpoint_id: plan.ownership.endpoint_id,
                plan_sha256: plan.plan_sha256,
            },
            101,
        )
        .unwrap()
}

#[test]
fn iac_resource_state_ownership_and_configuration_share_one_sqlite_snapshot() {
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let original = shell(&store);
    let target = original.ownership.resource;
    let second = Store::open(&path).unwrap();
    let changed = target.clone();
    let barrier = Arc::new(Barrier::new(2));
    let start = barrier.clone();
    let done = Arc::new(AtomicBool::new(false));
    let finished = done.clone();
    let worker = std::thread::spawn(move || {
        start.wait();
        for generation in 1..=100 {
            let value = format!("revision-{generation}");
            let mut conn = second.lock();
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            tx.execute(
                "UPDATE shells SET name=?2 WHERE id=?1",
                params![changed.id, value],
            )
            .unwrap();
            tx.execute("UPDATE iac_resource_owners SET logical_key=?2,generation=?3 WHERE kind='shell' AND resource_id=?1",
                params![changed.id,value,generation]).unwrap();
            tx.commit().unwrap();
            std::thread::yield_now();
        }
        finished.store(true, Ordering::Release);
    });
    let vault = crate::secrets::Vault::new(root.path().join("unused-vault"));
    barrier.wait();
    loop {
        let current = store.resource_state(&vault, &target).unwrap();
        let Some(ResourceConfiguration::Shell(config)) = current.configuration else {
            panic!("existing configuration was reported missing")
        };
        assert_eq!(current.ownership.owner.unwrap().key, config.name);
        if done.load(Ordering::Acquire) {
            break;
        }
    }
    worker.join().unwrap();
    assert!(!vault.is_initialized());
}

#[test]
fn iac_resource_state_reports_dependency_recovery_fences_without_claiming_missing_configuration() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let receipt = shell(&store);
    let vault = crate::secrets::Vault::new(root.path().join("unused-vault"));
    let target = receipt.ownership.resource.clone();
    let operation = NativeOperation {
        operation_id: Uuid::new_v4(),
        ownership: ResourceOwnership {
            endpoint_id: receipt.ownership.endpoint_id,
            resource: ManagedResource {
                kind: "vm".into(),
                id: Uuid::new_v4().to_string(),
            },
            generation: 0,
            owner: None,
        },
        action: NativeAction::DeleteVm,
        state: NativeOperationState::NeedsRecovery,
        error_code: Some("server_restarted".into()),
        execution_generation: 1,
        configuration_generation: None,
        retain_disks: Some(true),
    };
    {
        let mut conn = store.lock();
        let tx = conn.transaction().unwrap();
        iac_native::save(&tx, &operation).unwrap();
        super::super::super::iac_dependencies::reserve(&tx, &operation, target.clone()).unwrap();
        tx.commit().unwrap();
    }
    let current = store.resource_state(&vault, &target).unwrap();
    assert_eq!(current.active_operation_ids, vec![operation.operation_id]);
    assert_eq!(current.ownership, receipt.ownership);
    assert!(current.configuration.is_some());
}

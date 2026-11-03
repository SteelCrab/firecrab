use super::*;
use firecrab_api_types::CreateShellRequest;
use std::sync::{Arc, Barrier};

fn request(store: &Store, manager: Manager) -> PlanResource {
    let owner_id = if manager == Manager::Stack {
        store.register_stack("shared-plan").unwrap().id
    } else {
        Uuid::new_v4()
    };
    PlanResource {
        operation_id: Uuid::new_v4(),
        expected_endpoint_id: store.iac_endpoint_id().unwrap(),
        owner: ResourceOwner {
            manager,
            id: owner_id,
            key: if manager == Manager::Stack {
                "setup".into()
            } else {
                "firecrab_shell.setup".into()
            },
        },
        resource: None,
        desired: Some(ResourceConfiguration::Shell(CreateShellRequest {
            name: "setup".into(),
            description: Some("original metadata".into()),
            content: "echo private-original-script".into(),
        })),
    }
}

fn apply(
    store: &Store,
    plan: &ResourcePlan,
    now: u64,
) -> Result<ResourcePlanReceipt, PersistenceError> {
    store.apply_resource_plan(
        plan.operation_id,
        &ApplyResourcePlan {
            expected_endpoint_id: plan.ownership.endpoint_id,
            plan_sha256: plan.plan_sha256.clone(),
        },
        now,
    )
}

#[test]
fn iac_resource_plan_both_workflows_commit_configuration_ownership_and_original_receipt() {
    for manager in [Manager::Stack, Manager::Terraform] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let store = Store::open(&path).unwrap();
        let input = request(&store, manager);
        let plan = store.plan_resource(input.clone(), 100).unwrap();
        assert_eq!(plan.action, ResourcePlanAction::Create);
        assert!(store.list_shells().unwrap().is_empty());
        let public = serde_json::to_string(&plan).unwrap();
        assert!(
            !public.contains("private-original-script") && !public.contains("original metadata")
        );
        let receipt = apply(&store, &plan, 101).unwrap();
        assert_eq!(receipt.ownership.owner, Some(input.owner.clone()));
        assert_eq!(receipt.ownership.generation, 1);
        let id = Uuid::parse_str(&receipt.ownership.resource.id).unwrap();
        assert_eq!(
            store
                .shell_detail(id)
                .unwrap()
                .unwrap()
                .latest_content
                .as_deref(),
            Some("echo private-original-script")
        );
        assert_eq!(store.plan_resource(input.clone(), 900_000).unwrap(), plan);
        if manager == Manager::Terraform {
            assert!(matches!(
                store.add_shell_revision(id, Uuid::new_v4(), "echo delayed", "unused", 102),
                Err(PersistenceError::IacOwnershipConflict)
            ));
        }
        let mut changed = input.clone();
        changed.desired = None;
        assert!(matches!(
            store.plan_resource(changed, 102),
            Err(PersistenceError::InvalidIacOwner) | Err(PersistenceError::IacOperationIdentity)
        ));
        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(apply(&reopened, &plan, 900_001).unwrap(), receipt);
        assert_eq!(
            reopened
                .resource_plan_receipt(plan.operation_id)
                .unwrap()
                .unwrap(),
            receipt
        );
        assert_eq!(
            reopened
                .iac_native_operation(plan.operation_id)
                .unwrap()
                .unwrap()
                .state,
            NativeOperationState::Succeeded
        );
        assert_eq!(reopened.list_shells().unwrap().len(), 1);
    }
}

#[test]
fn iac_resource_plan_noop_metadata_and_revision_updates_preserve_identity_and_pins() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let mut input = request(&store, Manager::Terraform);
    let initial = store.plan_resource(input.clone(), 100).unwrap();
    let receipt = apply(&store, &initial, 101).unwrap();
    let id = Uuid::parse_str(&receipt.ownership.resource.id).unwrap();
    input.resource = Some(receipt.ownership.resource.clone());
    input.operation_id = Uuid::new_v4();
    let noop = store.plan_resource(input.clone(), 102).unwrap();
    assert_eq!(noop.action, ResourcePlanAction::Noop);
    assert_eq!(
        apply(&store, &noop, 103).unwrap().revision_id,
        receipt.revision_id
    );
    assert_eq!(store.shell_detail(id).unwrap().unwrap().revisions.len(), 1);
    input.operation_id = Uuid::new_v4();
    let Some(ResourceConfiguration::Shell(config)) = input.desired.as_mut() else {
        unreachable!()
    };
    config.name = "renamed".into();
    config.description = None;
    let metadata = store.plan_resource(input.clone(), 104).unwrap();
    assert_eq!(metadata.changed_fields, vec!["name", "description"]);
    assert_eq!(
        apply(&store, &metadata, 105).unwrap().revision_id,
        receipt.revision_id
    );
    let pinned_vm = Uuid::new_v4();
    store
        .lock()
        .execute(
            "INSERT INTO vm_shells(vm_id,shell_id,revision_id,position) VALUES(?1,?2,?3,0)",
            params![
                pinned_vm.to_string(),
                id.to_string(),
                receipt.revision_id.unwrap().to_string()
            ],
        )
        .unwrap();
    input.operation_id = Uuid::new_v4();
    let Some(ResourceConfiguration::Shell(config)) = input.desired.as_mut() else {
        unreachable!()
    };
    config.content = "echo next-revision".into();
    let revision = store.plan_resource(input.clone(), 106).unwrap();
    assert_eq!(revision.changed_fields, vec!["content"]);
    let updated = apply(&store, &revision, 107).unwrap();
    assert_ne!(updated.revision_id, receipt.revision_id);
    assert_eq!(updated.ownership, receipt.ownership);
    let original_pin: String = store
        .lock()
        .query_row(
            "SELECT revision_id FROM vm_shells WHERE vm_id=?1",
            [pinned_vm.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(original_pin, receipt.revision_id.unwrap().to_string());
    input.operation_id = Uuid::new_v4();
    input.desired = None;
    assert!(matches!(
        store.plan_resource(input, 108),
        Err(PersistenceError::IacResourceBusy)
    ));
}

#[test]
fn iac_resource_plan_rejects_stale_owner_expiry_and_wrong_digest_before_mutation() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let input = request(&store, Manager::Terraform);
    let plan = store.plan_resource(input.clone(), 100).unwrap();
    assert!(matches!(
        apply(&store, &plan, 600_100),
        Err(PersistenceError::StaleIacExecution)
    ));
    assert!(store.list_shells().unwrap().is_empty());
    let mut wrong = plan.clone();
    wrong.plan_sha256 = "a".repeat(64);
    assert!(matches!(
        apply(&store, &wrong, 101),
        Err(PersistenceError::IacOperationIdentity)
    ));
    let receipt = apply(&store, &plan, 101).unwrap();
    let mut next = input.clone();
    next.operation_id = Uuid::new_v4();
    next.resource = Some(receipt.ownership.resource.clone());
    next.desired = None;
    let delete = store.plan_resource(next, 102).unwrap();
    store
        .change_iac_resource_owner(&ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: receipt.ownership.endpoint_id,
            resource: receipt.ownership.resource.clone(),
            expected_generation: 1,
            expected_owner: Some(input.owner),
            owner: None,
        })
        .unwrap();
    assert!(matches!(
        apply(&store, &delete, 103),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert_eq!(store.list_shells().unwrap().len(), 1);
    assert_eq!(apply(&store, &plan, 900_000).unwrap(), receipt);
}

#[test]
fn iac_resource_plan_delete_receipt_survives_external_data_and_later_ownership() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let mut input = request(&store, Manager::Terraform);
    let created = apply(&store, &store.plan_resource(input.clone(), 1).unwrap(), 2).unwrap();
    input.resource = Some(created.ownership.resource.clone());
    input.operation_id = Uuid::new_v4();
    input.desired = None;
    let plan = store.plan_resource(input, 3).unwrap();
    let deleted = apply(&store, &plan, 4).unwrap();
    assert!(deleted.ownership.owner.is_none());
    assert_eq!(deleted.ownership.generation, 2);
    assert!(store.list_shells().unwrap().is_empty());
    assert_eq!(apply(&store, &plan, u64::MAX).unwrap(), deleted);
    assert_eq!(apply(&store, &created.plan, u64::MAX).unwrap(), created);
}

#[test]
fn iac_resource_plan_serializes_two_api_processes_and_reserves_retry_namespaces() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let mut input = request(&store, Manager::Terraform);
    let created = apply(&store, &store.plan_resource(input.clone(), 1).unwrap(), 2).unwrap();
    input.resource = Some(created.ownership.resource.clone());
    let mut plans = Vec::new();
    for content in ["echo contender-a", "echo contender-b"] {
        input.operation_id = Uuid::new_v4();
        let Some(ResourceConfiguration::Shell(config)) = input.desired.as_mut() else {
            unreachable!()
        };
        config.content = content.into();
        plans.push(store.plan_resource(input.clone(), 3).unwrap());
    }
    let reserved = plans[0].operation_id;
    assert!(matches!(
        store.iac_native_operation(reserved),
        Err(PersistenceError::IacOperationIdentity)
    ));
    let other_native = NativeOperation {
        operation_id: reserved,
        ownership: ResourceOwnership {
            endpoint_id: created.ownership.endpoint_id,
            resource: ManagedResource {
                kind: "vm".into(),
                id: Uuid::new_v4().to_string(),
            },
            generation: 0,
            owner: None,
        },
        action: NativeAction::ReviewVmBootPin,
        state: NativeOperationState::Succeeded,
        error_code: None,
        execution_generation: 0,
        configuration_generation: None,
        retain_disks: None,
    };
    assert!(matches!(
        iac_native::save(&store.lock(), &other_native),
        Err(PersistenceError::IacOperationIdentity)
    ));
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = plans
        .into_iter()
        .map(|plan| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let store = Store::open(&path).unwrap();
                barrier.wait();
                apply(&store, &plan, 4)
            })
        })
        .collect();
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .any(|result| matches!(result, Err(PersistenceError::StaleIacExecution)))
    );
    assert_eq!(
        store
            .shell_detail(created.ownership.resource.id.parse().unwrap())
            .unwrap()
            .unwrap()
            .revisions
            .len(),
        2
    );
    let conflict = store.change_iac_resource_owner(&ChangeResourceOwner {
        operation_id: reserved,
        expected_endpoint_id: created.ownership.endpoint_id,
        resource: created.ownership.resource.clone(),
        expected_generation: 1,
        expected_owner: created.ownership.owner.clone(),
        owner: None,
    });
    assert!(matches!(
        conflict,
        Err(PersistenceError::IacOperationIdentity)
    ));
    assert_eq!(
        store
            .iac_resource_owner(&created.ownership.resource)
            .unwrap(),
        created.ownership
    );
}

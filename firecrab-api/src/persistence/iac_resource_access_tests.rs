use super::*;
use firecrab_api_types::{
    SandboxAccessPolicy, SandboxPermission, SandboxResource, SandboxResourceGrant,
};
use firecrab_stack::SandboxAccessConfiguration;

fn input(store: &Store, manager: Manager) -> PlanResource {
    PlanResource {
        operation_id: Uuid::new_v4(),
        expected_endpoint_id: store.iac_endpoint_id().unwrap(),
        owner: ResourceOwner {
            manager,
            id: if manager == Manager::Stack {
                store.register_stack("access-plan").unwrap().id
            } else {
                Uuid::new_v4()
            },
            key: if manager == Manager::Stack {
                "delegated-host".into()
            } else {
                "firecrab_sandbox_access.worker".into()
            },
        },
        resource: None,
        removal: None,
        desired: Some(ResourceConfiguration::SandboxAccess(
            SandboxAccessConfiguration {
                principal: Uuid::new_v4(),
                revocation_generation: None,
                policy: SandboxAccessPolicy {
                    grants: vec![SandboxResourceGrant {
                        resource: SandboxResource::Project { id: Uuid::new_v4() },
                        permissions: vec![SandboxPermission::Execute, SandboxPermission::Read],
                    }],
                },
            },
        )),
    }
}
fn config(input: &mut PlanResource) -> &mut SandboxAccessConfiguration {
    let Some(ResourceConfiguration::SandboxAccess(config)) = input.desired.as_mut() else {
        panic!("expected access policy")
    };
    config
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
fn iac_resource_access_both_workflows_preserve_identity_noops_and_deny_tombstones() {
    for manager in [Manager::Stack, Manager::Terraform] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.db");
        let store = Store::open(&path).unwrap();
        let mut request = input(&store, manager);
        let principal = config(&mut request).principal;
        let first = store.plan_resource(request.clone(), 100).unwrap();
        assert!(!store.sandbox_delegate(principal).unwrap());
        let receipt = apply(&store, &first, 101).unwrap();
        assert_eq!(receipt.ownership.resource.id, principal.to_string());
        assert!(store.sandbox_delegate(principal).unwrap());
        assert!(receipt.revision_id.is_none());
        request.operation_id = Uuid::new_v4();
        request.resource = Some(receipt.ownership.resource.clone());
        // Rights and grant ordering have set semantics; ordinary refresh is not a mutation.
        config(&mut request).policy.grants[0].permissions.reverse();
        config(&mut request).policy.grants[0]
            .permissions
            .push(SandboxPermission::Read);
        let noop = store.plan_resource(request.clone(), 102).unwrap();
        assert_eq!(noop.action, ResourcePlanAction::Noop);
        assert_eq!(
            apply(&store, &noop, 103).unwrap().ownership,
            receipt.ownership
        );
        request.operation_id = Uuid::new_v4();
        config(&mut request).policy.grants[0].permissions = vec![SandboxPermission::Read];
        let changed = store.plan_resource(request.clone(), 104).unwrap();
        assert_eq!(changed.changed_fields, vec!["grants"]);
        apply(&store, &changed, 105).unwrap();
        request.operation_id = Uuid::new_v4();
        request.desired = None;
        let deletion = store.plan_resource(request.clone(), 106).unwrap();
        let deleted = apply(&store, &deletion, 107).unwrap();
        assert!(deleted.ownership.owner.is_none());
        assert!(store.sandbox_access(principal).unwrap().grants.is_empty());
        // Removing this row would incorrectly let a delegated certificate reach unrelated APIs.
        assert!(store.sandbox_delegate(principal).unwrap());
        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(apply(&reopened, &first, 900_000).unwrap(), receipt);
        assert!(
            reopened
                .sandbox_access(principal)
                .unwrap()
                .grants
                .is_empty()
        );
        let mut recreated = input(&reopened, manager);
        config(&mut recreated).principal = principal;
        assert!(matches!(
            reopened.plan_resource(recreated.clone(), 108),
            Err(PersistenceError::StaleIacOwner)
        ));
        config(&mut recreated).revocation_generation = Some(deleted.ownership.generation);
        // Explicit revocation acknowledgement permits new intent without deleting its deny row.
        let plan = reopened.plan_resource(recreated, 108).unwrap();
        assert_eq!(plan.action, ResourcePlanAction::Create);
        apply(&reopened, &plan, 109).unwrap();
        assert!(
            !reopened
                .sandbox_access(principal)
                .unwrap()
                .grants
                .is_empty()
        );
    }
}

#[test]
fn iac_resource_access_revoke_invalidates_pending_grants_and_completed_retry_never_regrants() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let mut request = input(&store, Manager::Terraform);
    let principal = config(&mut request).principal;
    let first = store.plan_resource(request.clone(), 100).unwrap();
    let receipt = apply(&store, &first, 101).unwrap();
    request.resource = Some(receipt.ownership.resource.clone());
    request.operation_id = Uuid::new_v4();
    config(&mut request).policy.grants[0]
        .permissions
        .push(SandboxPermission::Write);
    let pending = store.plan_resource(request, 102).unwrap();
    store
        .revoke_sandbox_access(principal, Uuid::new_v4())
        .unwrap();
    assert!(matches!(
        apply(&store, &pending, 103),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert_eq!(apply(&store, &first, 900_000).unwrap(), receipt);
    assert!(store.sandbox_access(principal).unwrap().grants.is_empty());
    assert!(store.sandbox_delegate(principal).unwrap());
}

#[test]
fn iac_resource_access_sources_keep_original_physical_directories_and_borrowed_contents() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let source = root.path().join("private-source-canary");
    std::fs::create_dir(&source).unwrap();
    let source = source.canonicalize().unwrap();
    std::fs::write(source.join("borrowed.txt"), b"original application data").unwrap();
    let mut request = input(&store, Manager::Terraform);
    config(&mut request)
        .policy
        .grants
        .push(SandboxResourceGrant {
            resource: SandboxResource::Source {
                path: source.to_str().unwrap().into(),
            },
            permissions: vec![SandboxPermission::Read],
        });
    let first = store.plan_resource(request.clone(), 100).unwrap();
    assert!(
        !serde_json::to_string(&first)
            .unwrap()
            .contains("private-source-canary")
    );
    let moved = root.path().join("original-source");
    std::fs::rename(&source, &moved).unwrap();
    std::fs::create_dir(&source).unwrap();
    assert!(matches!(
        apply(&store, &first, 101),
        Err(PersistenceError::StaleIacExecution)
    ));
    std::fs::remove_dir(&source).unwrap();
    std::fs::rename(&moved, &source).unwrap();
    let receipt = apply(&store, &first, 102).unwrap();
    request.resource = Some(receipt.ownership.resource.clone());
    request.operation_id = Uuid::new_v4();
    request.desired = None;
    let deletion = store.plan_resource(request, 103).unwrap();
    std::fs::rename(&source, &moved).unwrap();
    apply(&store, &deletion, 104).unwrap();
    assert_eq!(
        std::fs::read(moved.join("borrowed.txt")).unwrap(),
        b"original application data"
    );
    assert_eq!(apply(&store, &first, 900_000).unwrap(), receipt);
}

#[test]
fn iac_resource_access_rejects_implicit_adoption_duplicate_resources_and_invalid_rights() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let mut request = input(&store, Manager::Terraform);
    let principal = config(&mut request).principal;
    store
        .save_sandbox_access(principal, &config(&mut request).policy)
        .unwrap();
    assert!(matches!(
        store.plan_resource(request.clone(), 100),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    config(&mut request).principal = Uuid::new_v4();
    let duplicate = config(&mut request).policy.grants[0].clone();
    config(&mut request).policy.grants.push(duplicate);
    assert!(matches!(
        store.plan_resource(request.clone(), 100),
        Err(PersistenceError::InvalidIacOwner)
    ));
    config(&mut request).policy.grants.pop();
    config(&mut request).policy.grants[0].resource =
        SandboxResource::Connection { id: Uuid::new_v4() };
    config(&mut request).policy.grants[0].permissions = vec![SandboxPermission::Write];
    assert!(matches!(
        store.plan_resource(request, 100),
        Err(PersistenceError::InvalidIacOwner)
    ));
}

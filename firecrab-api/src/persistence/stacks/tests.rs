use std::sync::{Arc, Barrier};

use firecrab_api_types::{
    DeployMicroServiceRequest, MicroServiceResponse, MicroServiceSpec, ServiceInstance,
    ServicePhase, VmState,
};
use serde_json::json;

use super::*;
use crate::model::{VmPurpose, VmRecord};

fn network(id: Uuid) -> firecrab_api_types::MicroNetworkResponse {
    serde_json::from_value(json!({"id":id,"name":"existing","subnetCidr":"172.31.20.0/24","gateway":"172.31.20.1","internetEnabled":true})).unwrap()
}

fn binding(id: Uuid) -> Binding {
    Binding {
        kind: ResourceKind::Network,
        key: "app".to_owned(),
        resource_id: id,
    }
}

#[test]
fn stacks_delete_preserves_bound_resources_and_newer_registrations() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let stack = store.register_stack("demo").unwrap();
    let resource = network(Uuid::new_v4());
    store.insert_micro_network(&resource).unwrap();
    let owned = binding(resource.id);
    store.bind_stack_resource("demo", &owned).unwrap();
    assert!(matches!(
        store.delete_empty_stack("demo", stack.id),
        Err(PersistenceError::StackNotEmpty)
    ));
    store.unbind_stack_resource("demo", &owned).unwrap();
    store.delete_empty_stack("demo", stack.id).unwrap();
    let store = Store::open(&path).unwrap();
    assert!(store.stack("demo").unwrap().is_none());
    assert_eq!(store.micro_network(resource.id).unwrap(), Some(resource));
    let newer = store.register_stack("demo").unwrap();
    assert!(matches!(
        store.delete_empty_stack("demo", stack.id),
        Err(PersistenceError::StackConflict)
    ));
    assert_eq!(store.stack("demo").unwrap(), Some(newer));
}

fn vm(id: Uuid, purpose: VmPurpose) -> VmRecord {
    VmRecord {
        id,
        name: "worker".to_owned(),
        purpose,
        state: VmState::Created,
        template: "alpine".to_owned(),
        template_version: "v1".to_owned(),
        template_kernel_sha256: "kernel".to_owned(),
        template_rootfs_sha256: "rootfs".to_owned(),
        template_boot_args_sha256: "args".to_owned(),
        cpu: 1,
        ram: 256,
        disk_gb: 2,
        egress_policy: Default::default(),
        micro_network_id: Uuid::from_u128(1),
        security_group_id: None,
        storage_root: "default".to_owned(),
        disk_generation: None,
        last_runtime_id: None,
        startup_step: None,
        startup_timeline: Vec::new(),
        env: Default::default(),
    }
}

#[test]
fn stacks_registration_binding_and_revision_survive_reopen() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let registered = store.register_stack("demo").unwrap();
    assert_eq!(registered.revision, 0);
    assert_eq!(registered, store.register_stack("demo").unwrap());
    let resource = network(Uuid::new_v4());
    store.insert_micro_network(&resource).unwrap();
    let ownership = binding(resource.id);
    let claimed = store.bind_stack_resource("demo", &ownership).unwrap();
    assert_eq!(claimed.revision, 1);
    assert_eq!(
        claimed,
        store.bind_stack_resource("demo", &ownership).unwrap()
    );
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.stack("demo").unwrap(), Some(claimed));
    let released = store.unbind_stack_resource("demo", &ownership).unwrap();
    assert_eq!(released.revision, 2);
    assert!(released.bindings.is_empty());
    assert_eq!(
        released,
        store.unbind_stack_resource("demo", &ownership).unwrap()
    );
    assert_eq!(store.micro_network(resource.id).unwrap(), Some(resource));
    assert_eq!(store.list_stacks().unwrap(), vec![released]);
}

#[test]
fn stacks_conflicts_never_overwrite_a_key_or_another_stack_owner() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    store.register_stack("first").unwrap();
    store.register_stack("second").unwrap();
    let first = network(Uuid::new_v4());
    let second = network(Uuid::new_v4());
    store.insert_micro_network(&first).unwrap();
    store.insert_micro_network(&second).unwrap();
    let ownership = binding(first.id);
    let before = store.bind_stack_resource("first", &ownership).unwrap();
    assert!(matches!(
        store.bind_stack_resource("second", &ownership),
        Err(PersistenceError::StackConflict)
    ));
    assert!(matches!(
        store.bind_stack_resource("first", &binding(second.id)),
        Err(PersistenceError::StackConflict)
    ));
    assert!(matches!(
        store.unbind_stack_resource("first", &binding(second.id)),
        Err(PersistenceError::StackConflict)
    ));
    assert_eq!(store.stack("first").unwrap(), Some(before));
    assert_eq!(store.stack("second").unwrap().unwrap().revision, 0);
}

#[test]
fn stacks_only_one_connection_can_claim_the_same_uuid() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let first = Store::open(&path).unwrap();
    let second = Store::open(&path).unwrap();
    first.register_stack("first").unwrap();
    second.register_stack("second").unwrap();
    let resource = network(Uuid::new_v4());
    first.insert_micro_network(&resource).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let other = barrier.clone();
    let id = resource.id;
    let thread = std::thread::spawn(move || {
        other.wait();
        first.bind_stack_resource("first", &binding(id))
    });
    barrier.wait();
    let a = second.bind_stack_resource("second", &binding(id));
    let b = thread.join().unwrap();
    assert_ne!(a.is_ok(), b.is_ok());
    let rows = second.list_stacks().unwrap();
    assert_eq!(rows.iter().map(|s| s.bindings.len()).sum::<usize>(), 1);
    assert_eq!(rows.iter().map(|s| s.revision).sum::<u64>(), 1);
}

#[test]
fn stacks_missing_and_protected_resources_cannot_be_adopted() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    store.register_stack("demo").unwrap();
    assert!(matches!(
        store.bind_stack_resource("demo", &binding(Uuid::new_v4())),
        Err(PersistenceError::InvalidStackResource)
    ));
    let builder = vm(Uuid::new_v4(), VmPurpose::Builder);
    store.insert(&builder).unwrap();
    let mut owned = Binding {
        kind: ResourceKind::Vm,
        key: "worker".to_owned(),
        resource_id: builder.id,
    };
    assert!(matches!(
        store.bind_stack_resource("demo", &owned),
        Err(PersistenceError::InvalidStackResource)
    ));
    let replica = vm(Uuid::new_v4(), VmPurpose::Instance);
    let pooled = vm(Uuid::new_v4(), VmPurpose::Pool);
    store.insert(&pooled).unwrap();
    owned.resource_id = pooled.id;
    assert!(matches!(
        store.bind_stack_resource("demo", &owned),
        Err(PersistenceError::InvalidStackResource)
    ));
    store.insert(&replica).unwrap();
    let spec: MicroServiceSpec = serde_json::from_value(
        json!({"name":"web","image":"nginx:latest","port":80,"cpu":1,"ram":256}),
    )
    .unwrap();
    let service = MicroServiceResponse {
        id: Uuid::new_v4(),
        spec: spec.clone(),
    };
    store.insert_micro_service(&service).unwrap();
    let mut deployment = MicroServiceDeployment {
        recovery: None,
        started_at: 0,
        finished_at: None,
        update: None,
        id: Uuid::new_v4(),
        service_id: service.id,
        phase: ServicePhase::Running,
        spec,
        request: DeployMicroServiceRequest {
            micro_network_id: Some(Uuid::from_u128(1)),
            host_port: 30000,
        },
        digest: None,
        instances: vec![ServiceInstance {
            vm_id: replica.id,
            ipv4: "172.31.20.10".to_owned(),
            host_port: 30000,
            state: VmState::Running,
            healthy: true,
        }],
        log: Vec::new(),
        error: None,
        domain_route_owned: false,
        internal_route_owned: false,
    };
    store.save_service_deployment(&deployment).unwrap();
    owned.resource_id = replica.id;
    assert!(matches!(
        store.bind_stack_resource("demo", &owned),
        Err(PersistenceError::InvalidStackResource)
    ));
    let unrelated = vm(Uuid::new_v4(), VmPurpose::Instance);
    store.insert(&unrelated).unwrap();
    owned.resource_id = unrelated.id;
    deployment.phase = ServicePhase::Starting;
    store.save_service_deployment(&deployment).unwrap();
    assert!(matches!(
        store.bind_stack_resource("demo", &owned),
        Err(PersistenceError::InvalidStackResource)
    ));
    deployment.phase = ServicePhase::Running;
    store.save_service_deployment(&deployment).unwrap();
    assert_eq!(
        store.bind_stack_resource("demo", &owned).unwrap().revision,
        1
    );
}

#[test]
fn stacks_stale_bindings_can_be_released_without_touching_any_resource() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    store.register_stack("demo").unwrap();
    let resource = network(Uuid::new_v4());
    store.insert_micro_network(&resource).unwrap();
    let owned = binding(resource.id);
    let before = store.bind_stack_resource("demo", &owned).unwrap();
    store.delete_micro_network(resource.id).unwrap();
    assert_eq!(store.bind_stack_resource("demo", &owned).unwrap(), before);
    assert_eq!(
        store
            .unbind_stack_resource("demo", &owned)
            .unwrap()
            .revision,
        2
    );
    assert!(store.micro_network(resource.id).unwrap().is_none());
}

#[test]
fn stacks_binding_limit_preserves_idempotence_and_allows_explicit_release() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    store.register_stack("demo").unwrap();
    let mut bindings = Vec::new();
    for i in 0..=firecrab_stack::MAX_RESOURCES {
        let resource = network(Uuid::from_u128(100 + i as u128));
        store.insert_micro_network(&resource).unwrap();
        let owned = Binding {
            kind: ResourceKind::Network,
            key: format!("net-{i}"),
            resource_id: resource.id,
        };
        if i < firecrab_stack::MAX_RESOURCES {
            store.bind_stack_resource("demo", &owned).unwrap();
        }
        bindings.push(owned);
    }
    let before = store.stack("demo").unwrap().unwrap();
    assert_eq!(before.bindings.len(), firecrab_stack::MAX_RESOURCES);
    assert_eq!(
        store.bind_stack_resource("demo", &bindings[0]).unwrap(),
        before
    );
    assert!(matches!(
        store.bind_stack_resource("demo", bindings.last().unwrap()),
        Err(PersistenceError::StackBindingLimit)
    ));
    assert_eq!(store.stack("demo").unwrap().unwrap(), before);
    store.unbind_stack_resource("demo", &bindings[0]).unwrap();
    assert_eq!(
        store
            .bind_stack_resource("demo", bindings.last().unwrap())
            .unwrap()
            .bindings
            .len(),
        firecrab_stack::MAX_RESOURCES
    );
}

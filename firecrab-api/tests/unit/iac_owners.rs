use super::*;
use firecrab_stack::ResourceKind;
use std::sync::{Arc, Barrier};

fn resource(store: &Store) -> ManagedResource {
    let id = Uuid::new_v4();
    store.insert_micro_network(&serde_json::from_value(serde_json::json!({
        "id":id,"name":"owned","subnetCidr":"172.31.20.0/24","gateway":"172.31.20.1","internetEnabled":true
    })).unwrap()).unwrap();
    ManagedResource {
        kind: "network".into(),
        id: id.to_string(),
    }
}

fn terraform() -> ResourceOwner {
    ResourceOwner {
        manager: Manager::Terraform,
        id: Uuid::new_v4(),
        key: "firecrab_network.app".into(),
    }
}

fn change(current: &ResourceOwnership, owner: Option<ResourceOwner>) -> ChangeResourceOwner {
    ChangeResourceOwner {
        expected_endpoint_id: current.endpoint_id,
        operation_id: Uuid::new_v4(),
        resource: current.resource.clone(),
        expected_generation: current.generation,
        expected_owner: current.owner.clone(),
        owner,
    }
}

fn volume(store: &Store) -> ManagedResource {
    let id = Uuid::new_v4();
    store
        .insert_block_storage(
            &firecrab_api_types::MicroBlockStorageResponse {
                id,
                name: format!("volume-{id}"),
                size_gib: 1,
                storage_root: "default".into(),
                state: "creating".into(),
                attached_vm_id: None,
                mount_path: None,
            },
            None,
        )
        .unwrap();
    store.finish_block_storage_create(id).unwrap();
    ManagedResource {
        kind: "volume".into(),
        id: id.to_string(),
    }
}

#[test]
fn iac_configuration_native_slots_survive_restart_and_reject_duplicate_addresses() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let first = volume(&store);
    let second = volume(&store);
    let stack = store.register_stack("configuration").unwrap();
    let native = ResourceOwner {
        manager: Manager::Stack,
        id: stack.id,
        key: "disk".into(),
    };
    let before = store.iac_resource_owner(&first).unwrap();
    let request = change(&before, Some(native.clone()));
    let owned = store.change_iac_resource_owner(&request).unwrap();
    assert_eq!(
        store.list_iac_resource_owners().unwrap(),
        vec![owned.clone()]
    );
    assert!(matches!(
        store.delete_empty_stack("configuration", stack.id),
        Err(PersistenceError::StackNotEmpty)
    ));
    let conflict = change(&store.iac_resource_owner(&second).unwrap(), Some(native));
    assert!(matches!(
        store.change_iac_resource_owner(&conflict),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    assert!(
        store
            .iac_ownership_operation(conflict.operation_id)
            .unwrap()
            .is_none()
    );
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.iac_resource_owner(&first).unwrap(), owned);
    assert_eq!(store.change_iac_resource_owner(&request).unwrap(), owned);
    let external = store
        .change_iac_resource_owner(&change(&owned, Some(terraform())))
        .unwrap();
    assert_eq!(external.generation, 2);
    assert_eq!(store.stack("configuration").unwrap().unwrap().revision, 2);
    store.delete_empty_stack("configuration", stack.id).unwrap();
    let mut alias = first.clone();
    alias.id = Uuid::parse_str(&alias.id)
        .unwrap()
        .simple()
        .to_string()
        .to_uppercase();
    assert_eq!(store.iac_resource_owner(&alias).unwrap(), external);
}

#[test]
fn iac_volume_transfer_waits_for_disk_work_without_journaling_a_failed_admission() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let resource = volume(&store);
    let request = change(
        &store.iac_resource_owner(&resource).unwrap(),
        Some(terraform()),
    );
    store
        .lock()
        .execute(
            "UPDATE micro_block_storages SET phase='creating' WHERE id=?1",
            [&resource.id],
        )
        .unwrap();
    assert!(matches!(
        store.change_iac_resource_owner(&request),
        Err(PersistenceError::IacResourceBusy)
    ));
    assert!(
        store
            .iac_ownership_operation(request.operation_id)
            .unwrap()
            .is_none()
    );
    store
        .finish_block_storage_create(Uuid::parse_str(&resource.id).unwrap())
        .unwrap();
    let owned = store.change_iac_resource_owner(&request).unwrap();
    assert_eq!(owned.generation, 1);
    assert!(
        store
            .block_storage(Uuid::parse_str(&resource.id).unwrap())
            .is_ok()
    );
}

#[test]
fn iac_configuration_slots_count_toward_the_legacy_stack_binding_limit() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let stack = store.register_stack("configuration").unwrap();
    let resource = resource(&store);
    let mut retained = Vec::new();
    {
        let mut conn = store.lock();
        let tx = conn.transaction().unwrap();
        // Externally deleted resources retain their ownership until explicitly released.
        for i in 0..firecrab_stack::MAX_RESOURCES {
            let id = Uuid::new_v4().to_string();
            tx.execute("INSERT INTO iac_resource_owners(kind,resource_id,generation,manager,owner_id,logical_key) VALUES('volume',?1,1,'stack',?2,?3)",
                params![id,stack.id.to_string(),format!("retained-{i}")]).unwrap();
            retained.push(id);
        }
        tx.commit().unwrap();
    }
    let binding = Binding {
        kind: ResourceKind::Network,
        key: "network".into(),
        resource_id: Uuid::parse_str(&resource.id).unwrap(),
    };
    assert!(matches!(
        store.bind_stack_resource("configuration", &binding),
        Err(PersistenceError::StackBindingLimit)
    ));
    let retained = ManagedResource {
        kind: "volume".into(),
        id: retained.remove(0),
    };
    store
        .change_iac_resource_owner(&change(&store.iac_resource_owner(&retained).unwrap(), None))
        .unwrap();
    store
        .bind_stack_resource("configuration", &binding)
        .unwrap();
    assert_eq!(
        store.stack("configuration").unwrap().unwrap().bindings,
        vec![binding]
    );
}

#[test]
fn iac_owner_transfer_preserves_resource_and_replays_only_the_exact_request() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let resource = resource(&store);
    let before = store.iac_resource_owner(&resource).unwrap();
    let claim = change(&before, Some(terraform()));
    let owned = store.change_iac_resource_owner(&claim).unwrap();
    assert_eq!(owned.generation, 1);
    assert_eq!(owned.owner, claim.owner);
    assert_eq!(owned, store.change_iac_resource_owner(&claim).unwrap());
    let mut different = claim.clone();
    different.owner = Some(terraform());
    assert!(matches!(
        store.change_iac_resource_owner(&different),
        Err(PersistenceError::IacOperationIdentity)
    ));
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(owned, store.iac_resource_owner(&resource).unwrap());
    let released = store
        .change_iac_resource_owner(&change(&owned, None))
        .unwrap();
    assert_eq!(released.generation, 2);
    assert!(released.owner.is_none());
    assert!(
        store
            .micro_network(Uuid::parse_str(&resource.id).unwrap())
            .unwrap()
            .is_some()
    );
    assert_eq!(owned, store.change_iac_resource_owner(&claim).unwrap());
    assert_eq!(released, store.iac_resource_owner(&resource).unwrap());
}

#[test]
fn iac_owner_generation_rejects_release_reclaim_races_and_native_rebinding() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let resource = resource(&store);
    let free = store.iac_resource_owner(&resource).unwrap();
    let stale = change(&free, Some(terraform()));
    store.register_stack("demo").unwrap();
    let binding = Binding {
        kind: ResourceKind::Network,
        key: "app".into(),
        resource_id: Uuid::parse_str(&resource.id).unwrap(),
    };
    store.bind_stack_resource("demo", &binding).unwrap();
    store.unbind_stack_resource("demo", &binding).unwrap();
    assert!(matches!(
        store.change_iac_resource_owner(&stale),
        Err(PersistenceError::StaleIacOwner)
    ));
    let current = store.iac_resource_owner(&resource).unwrap();
    assert_eq!(current.generation, 2);
    let owned = store
        .change_iac_resource_owner(&change(&current, Some(terraform())))
        .unwrap();
    assert!(matches!(
        store.bind_stack_resource("demo", &binding),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    assert_eq!(owned, store.iac_resource_owner(&resource).unwrap());
    assert!(store.stack("demo").unwrap().unwrap().bindings.is_empty());
}

#[test]
fn iac_owner_native_terraform_transfer_is_atomic_and_retains_uuid() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let resource = resource(&store);
    let stack = store.register_stack("demo").unwrap();
    let free = store.iac_resource_owner(&resource).unwrap();
    let native = ResourceOwner {
        manager: Manager::Stack,
        id: stack.id,
        key: "app".into(),
    };
    let bound = store
        .change_iac_resource_owner(&change(&free, Some(native.clone())))
        .unwrap();
    let before = store.stack("demo").unwrap().unwrap();
    assert_eq!(before.bindings[0].resource_id.to_string(), resource.id);
    let owned = store
        .change_iac_resource_owner(&change(&bound, Some(terraform())))
        .unwrap();
    assert!(store.stack("demo").unwrap().unwrap().bindings.is_empty());
    let returned = store
        .change_iac_resource_owner(&change(&owned, Some(native)))
        .unwrap();
    assert_eq!(returned.generation, 3);
    assert_eq!(
        store.stack("demo").unwrap().unwrap().bindings[0]
            .resource_id
            .to_string(),
        resource.id
    );
    assert!(
        store
            .micro_network(Uuid::parse_str(&resource.id).unwrap())
            .unwrap()
            .is_some()
    );
}

#[test]
fn iac_owner_rejects_missing_unknown_and_unsupported_resources_without_journaling() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let missing = ManagedResource {
        kind: "network".into(),
        id: Uuid::new_v4().to_string(),
    };
    let current = store.iac_resource_owner(&missing).unwrap();
    let request = change(&current, Some(terraform()));
    assert!(matches!(
        store.change_iac_resource_owner(&request),
        Err(PersistenceError::InvalidStackResource)
    ));
    assert_eq!(current, store.iac_resource_owner(&missing).unwrap());
    let unsupported = ManagedResource {
        kind: "management_settings".into(),
        id: "local".into(),
    };
    let request = change(
        &store.iac_resource_owner(&unsupported).unwrap(),
        Some(terraform()),
    );
    assert!(matches!(
        store.change_iac_resource_owner(&request),
        Err(PersistenceError::UnsupportedIacResource)
    ));
    assert_eq!(
        store
            .lock()
            .query_row("SELECT count(*) FROM iac_owner_changes", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let mut bad = request.clone();
    bad.resource.kind = "unknown".into();
    assert!(matches!(
        store.change_iac_resource_owner(&bad),
        Err(PersistenceError::InvalidIacOwner)
    ));
}

#[test]
fn iac_owner_only_one_database_connection_can_adopt_the_same_resource() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.db");
    let first = Store::open(&path).unwrap();
    let second = Store::open(&path).unwrap();
    let resource = resource(&first);
    let current = first.iac_resource_owner(&resource).unwrap();
    let left = change(&current, Some(terraform()));
    let right = change(&current, Some(terraform()));
    let barrier = Arc::new(Barrier::new(2));
    let other = barrier.clone();
    let thread = std::thread::spawn(move || {
        other.wait();
        first.change_iac_resource_owner(&left)
    });
    barrier.wait();
    let a = second.change_iac_resource_owner(&right);
    let b = thread.join().unwrap();
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(second.iac_resource_owner(&resource).unwrap().generation, 1);
}

#[test]
fn iac_owner_duplicate_terraform_address_and_failed_stack_transfer_preserve_current_owner() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let first = resource(&store);
    let second = resource(&store);
    let owner = terraform();
    let first_owner = store
        .change_iac_resource_owner(&change(
            &store.iac_resource_owner(&first).unwrap(),
            Some(owner.clone()),
        ))
        .unwrap();
    let second_free = store.iac_resource_owner(&second).unwrap();
    assert!(matches!(
        store.change_iac_resource_owner(&change(&second_free, Some(owner))),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    let missing = ResourceOwner {
        manager: Manager::Stack,
        id: Uuid::new_v4(),
        key: "app".into(),
    };
    assert!(matches!(
        store.change_iac_resource_owner(&change(&first_owner, Some(missing))),
        Err(PersistenceError::MissingStack)
    ));
    assert_eq!(first_owner, store.iac_resource_owner(&first).unwrap());
    assert_eq!(second_free, store.iac_resource_owner(&second).unwrap());
}

#[test]
fn iac_owner_endpoint_identity_survives_restart_and_rejects_another_host() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("first.db");
    let first = Store::open(&path).unwrap();
    let second = Store::open(&root.path().join("second.db")).unwrap();
    let resource = resource(&first);
    let current = first.iac_resource_owner(&resource).unwrap();
    let row = first
        .micro_network(Uuid::parse_str(&resource.id).unwrap())
        .unwrap()
        .unwrap();
    second.insert_micro_network(&row).unwrap();
    assert_ne!(current.endpoint_id, second.iac_endpoint_id().unwrap());
    let request = change(&current, Some(terraform()));
    assert!(matches!(
        second.change_iac_resource_owner(&request),
        Err(PersistenceError::IacEndpointConflict)
    ));
    drop(first);
    assert_eq!(
        Store::open(&path).unwrap().iac_endpoint_id().unwrap(),
        current.endpoint_id
    );
    assert!(
        second
            .iac_resource_owner(&resource)
            .unwrap()
            .owner
            .is_none()
    );
}

#[test]
fn iac_owner_active_service_operations_block_transfer_without_claiming_a_receipt() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let id = Uuid::new_v4();
    let spec = serde_json::json!({"name":"application","image":"nginx:latest","port":80,"cpu":1,"ram":256});
    store.insert_micro_service(&serde_json::from_value(serde_json::json!({"id":id,"name":"application","image":"nginx:latest","port":80,"cpu":1,"ram":256})).unwrap()).unwrap();
    let resource = ManagedResource {
        kind: "service".into(),
        id: id.to_string(),
    };
    let before = store.iac_resource_owner(&resource).unwrap();
    let mut owner = terraform();
    owner.key = "firecrab_service.application".into();
    let request = change(&before, Some(owner));
    let mut deployment:firecrab_api_types::MicroServiceDeployment=serde_json::from_value(serde_json::json!({
        "id":Uuid::new_v4(),"serviceId":id,"phase":"queued","spec":spec,"request":{"microNetworkId":null,"hostPort":8080},
        "digest":null,"instances":[],"log":[],"error":null
    })).unwrap();
    store.save_service_deployment(&deployment).unwrap();
    assert!(matches!(
        store.change_iac_resource_owner(&request),
        Err(PersistenceError::IacResourceBusy)
    ));
    assert!(
        store
            .iac_ownership_operation(request.operation_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(store.iac_resource_owner(&resource).unwrap(), before);
    deployment.phase = firecrab_api_types::ServicePhase::Stopped;
    store.save_service_deployment(&deployment).unwrap();
    let owned = store.change_iac_resource_owner(&request).unwrap();
    assert_eq!(owned.owner, request.owner);
    assert!(!store.stack_service_mutation_allowed(id, None).unwrap());
}

#[test]
fn iac_owner_vm_transition_blocks_adoption_and_release() {
    use crate::model::{VmPurpose, VmRecord};
    use firecrab_api_types::VmState;
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let mut vm = VmRecord {
        id: Uuid::new_v4(),
        name: "worker".into(),
        purpose: VmPurpose::Instance,
        state: VmState::Starting,
        template: "alpine".into(),
        template_version: "v1".into(),
        template_kernel_sha256: "kernel".into(),
        template_rootfs_sha256: "rootfs".into(),
        template_boot_args_sha256: "args".into(),
        cpu: 1,
        ram: 256,
        disk_gb: 2,
        egress_policy: Default::default(),
        micro_network_id: Uuid::new_v4(),
        security_group_id: None,
        storage_root: "default".into(),
        disk_generation: None,
        last_runtime_id: None,
        startup_step: None,
        startup_timeline: vec![],
        env: Default::default(),
    };
    store.insert(&vm).unwrap();
    store.register_stack("legacy").unwrap();
    let binding = Binding {
        kind: ResourceKind::Vm,
        key: "worker".into(),
        resource_id: vm.id,
    };
    assert!(matches!(
        store.bind_stack_resource("legacy", &binding),
        Err(PersistenceError::IacResourceBusy)
    ));
    let resource = ManagedResource {
        kind: "vm".into(),
        id: vm.id.to_string(),
    };
    let before = store.iac_resource_owner(&resource).unwrap();
    let mut owner = terraform();
    owner.key = "firecrab_vm.worker".into();
    let request = change(&before, Some(owner));
    assert!(matches!(
        store.change_iac_resource_owner(&request),
        Err(PersistenceError::IacResourceBusy)
    ));
    vm.state = VmState::Running;
    store.update(&vm).unwrap();
    let owned = store.change_iac_resource_owner(&request).unwrap();
    vm.state = VmState::Stopping;
    store.update(&vm).unwrap();
    assert!(matches!(
        store.change_iac_resource_owner(&change(&owned, None)),
        Err(PersistenceError::IacResourceBusy)
    ));
    assert_eq!(store.iac_resource_owner(&resource).unwrap(), owned);
    vm.state = VmState::Stopped;
    store.update(&vm).unwrap();
    store
        .change_iac_resource_owner(&change(&owned, None))
        .unwrap();
    store.bind_stack_resource("legacy", &binding).unwrap();
    vm.state = VmState::Starting;
    store.update(&vm).unwrap();
    assert!(matches!(
        store.unbind_stack_resource("legacy", &binding),
        Err(PersistenceError::IacResourceBusy)
    ));
    assert_eq!(
        store.stack("legacy").unwrap().unwrap().bindings,
        vec![binding.clone()]
    );
    vm.state = VmState::Stopped;
    store.update(&vm).unwrap();
    store.unbind_stack_resource("legacy", &binding).unwrap();
    assert!(store.iac_resource_owner(&resource).unwrap().owner.is_none());
}

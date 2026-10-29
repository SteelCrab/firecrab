use super::*;
use crate::{artifacts::VmArtifactPaths, model::VmRecord, storage::StorageRegistry};
use firecrab_api_types::{
    DeployMicroServiceRequest, DeployStrategy, MicroServiceDeployment, MicroServiceResponse,
    ServiceInstance, ServicePhase, ServiceUpdate, VmState,
};

fn service() -> MicroServiceResponse {
    MicroServiceResponse {
        id: Uuid::new_v4(),
        spec: serde_json::from_value(json!({
            "name":"web", "image":format!("ghcr.io/team/app@sha256:{}", "a".repeat(64)),
            "port":8080, "ram":128, "cpu":1
        }))
        .unwrap(),
    }
}

fn deployment(service: &MicroServiceResponse) -> MicroServiceDeployment {
    MicroServiceDeployment {
        recovery: None,
        started_at: 0,
        finished_at: None,
        update: None,
        id: Uuid::new_v4(),
        service_id: service.id,
        phase: ServicePhase::Failed,
        spec: service.spec.clone(),
        request: DeployMicroServiceRequest {
            micro_network_id: None,
            host_port: 30100,
        },
        digest: None,
        instances: vec![],
        log: vec![],
        domain_route_owned: false,
        internal_route_owned: false,
        error: None,
    }
}

fn replica(vm: Uuid) -> ServiceInstance {
    ServiceInstance {
        vm_id: vm,
        ipv4: "172.31.0.2".into(),
        host_port: 30100,
        state: VmState::Stopped,
        healthy: false,
    }
}

fn update(previous: MicroServiceDeployment) -> ServiceUpdate {
    ServiceUpdate {
        requested_spec: previous.spec.clone(),
        previous: Box::new(previous),
        failure: None,
        cancelled: false,
        strategy: DeployStrategy::Rolling,
        rollback_of: None,
        restoring: false,
        routed_new: false,
        transition: None,
        retiring: vec![],
        completed: false,
    }
}

async fn owned(
    root: &std::path::Path,
) -> (Router, AppState, VmRecord, Arc<Mutex<Vec<&'static str>>>) {
    let (_, state, calls) = fixture(root, None).await;
    let state = state.with_test_storage(StorageRegistry::single("default", root.join("storage")));
    let network =
        crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);
    state
        .store
        .bind_stack_resource(
            "demo",
            &Binding {
                kind: ResourceKind::Network,
                key: "app".into(),
                resource_id: network,
            },
        )
        .unwrap();
    let mut vm = crate::handlers::vms::test_support::record("worker", Uuid::new_v4());
    inputs::pin_record(&state, root, &mut vm);
    vm.disk_gb = 1;
    vm.micro_network_id = network;
    vm.disk_generation = Some(Uuid::new_v4());
    state.store.insert(&vm).unwrap();
    state.vms.lock().unwrap().insert(vm.id, vm.clone());
    state
        .store
        .bind_stack_resource(
            "demo",
            &Binding {
                kind: ResourceKind::Vm,
                key: "worker".into(),
                resource_id: vm.id,
            },
        )
        .unwrap();
    let disk = VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id);
    disk.ensure_directories().unwrap();
    std::fs::write(disk.rootfs(vm.disk_generation.unwrap()), b"guest-data-KEEP").unwrap();
    (router(&state), state, vm, calls)
}

fn desired(vm: &VmRecord) -> Value {
    json!({"apiVersion":"firecrab.dev/v1alpha1","kind":"Stack","name":"demo","microNetworks":{"app":{"name":"test-net","subnetCidr":"172.31.0.0/24"}},"microVMs":{"worker":{"name":vm.name,"template":vm.template,"network":"app","ram":vm.ram,"cpu":vm.cpu,"diskGb":vm.disk_gb}}})
}

fn empty() -> Value {
    json!({"apiVersion":"firecrab.dev/v1alpha1","kind":"Stack","name":"demo","lifecycle":{"prune":true}})
}

#[tokio::test]
async fn stack_prune_preserves_standalone_disks_by_default_and_releases_bindings_in_reverse_order()
{
    let root = tempfile::tempdir().unwrap();
    let (router, state, vm, _) = owned(root.path()).await;
    let id = Uuid::new_v4();
    let body = planned(&router, &empty(), id).await;
    let (code, error) = request(&router, "POST", "/api/stacks/demo/apply", body.clone()).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{error}");
    let status = terminal(&router, id).await;
    assert_eq!(status.state, ApplyState::Succeeded, "{status:?}");
    assert_eq!(
        status
            .changes
            .iter()
            .map(|s| (s.change.kind, s.change.action))
            .collect::<Vec<_>>(),
        [
            (ResourceKind::Vm, Action::Delete),
            (ResourceKind::Network, Action::Delete)
        ]
    );
    assert_eq!(
        status.changes[0].vm_disk.as_ref().unwrap().generation,
        vm.disk_generation
    );
    let disk = VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id)
        .rootfs(vm.disk_generation.unwrap());
    assert_eq!(std::fs::read(&disk).unwrap(), b"guest-data-KEEP");
    assert!(
        state
            .store
            .stack("demo")
            .unwrap()
            .unwrap()
            .bindings
            .is_empty()
    );
    assert!(state.store.load_all().unwrap().is_empty());
    assert!(state.store.list_micro_networks().unwrap().is_empty());
    assert_eq!(
        request(&router, "POST", "/api/stacks/demo/apply", body)
            .await
            .0,
        StatusCode::ACCEPTED
    );
    let reopened = crate::persistence::Store::open(&root.path().join("state.db")).unwrap();
    assert_eq!(
        reopened.stored_stack_apply(id).unwrap().unwrap().status,
        status
    );
    assert!(disk.exists());
}

#[tokio::test]
async fn stack_replacement_uses_a_new_uuid_and_retains_old_guest_data_without_restarting_the_new_vm()
 {
    let root = tempfile::tempdir().unwrap();
    let (router, state, vm, calls) = owned(root.path()).await;
    let mut stack = desired(&vm);
    stack["microVMs"]["worker"]["name"] = json!("worker-next");
    let rejected = planned(&router, &stack, Uuid::new_v4()).await;
    assert_eq!(
        request(&router, "POST", "/api/stacks/demo/apply", rejected)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert!(calls.lock().unwrap().is_empty());
    stack["lifecycle"] = json!({"replace":true});
    let id = Uuid::new_v4();
    let body = planned(&router, &stack, id).await;
    let (code, error) = request(&router, "POST", "/api/stacks/demo/apply", body).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{error}");
    let status = terminal(&router, id).await;
    assert_eq!(status.state, ApplyState::Succeeded, "{status:?}");
    let current = state
        .store
        .load_all()
        .unwrap()
        .into_values()
        .next()
        .unwrap();
    assert_ne!(current.id, vm.id);
    assert_eq!(current.name, "worker-next");
    assert_eq!(current.state, firecrab_api_types::VmState::Created);
    let create = status
        .changes
        .iter()
        .find(|s| s.replacement_for == Some(vm.id))
        .unwrap();
    assert_eq!(create.change.resource_id, Some(current.id));
    assert_eq!(
        state
            .store
            .stack("demo")
            .unwrap()
            .unwrap()
            .bindings
            .iter()
            .find(|b| b.kind == ResourceKind::Vm)
            .unwrap()
            .resource_id,
        current.id
    );
    let disk = VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id)
        .rootfs(vm.disk_generation.unwrap());
    assert_eq!(std::fs::read(disk).unwrap(), b"guest-data-KEEP");
    let second = Uuid::new_v4();
    let body = planned(&router, &stack, second).await;
    request(&router, "POST", "/api/stacks/demo/apply", body).await;
    assert_eq!(terminal(&router, second).await.state, ApplyState::Succeeded);
    assert_eq!(state.store.load_all().unwrap().len(), 1);
}

#[tokio::test]
async fn stack_prune_disk_deletion_requires_its_own_manifest_flag() {
    let root = tempfile::tempdir().unwrap();
    let (router, state, vm, _) = owned(root.path()).await;
    let disk = VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id)
        .rootfs(vm.disk_generation.unwrap());
    let mut stack = empty();
    stack["lifecycle"]["deleteVmDisks"] = json!(true);
    let id = Uuid::new_v4();
    let body = planned(&router, &stack, id).await;
    let (code, error) = request(&router, "POST", "/api/stacks/demo/apply", body).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{error}");
    let status = terminal(&router, id).await;
    assert_eq!(status.state, ApplyState::Succeeded, "{status:?}");
    assert!(!disk.exists());
    assert!(status.lifecycle.delete_vm_disks);
    assert!(status.changes[0].vm_disk.is_some());
}

#[tokio::test]
async fn stack_prune_external_users_and_running_workloads_block_before_any_mutation() {
    let root = tempfile::tempdir().unwrap();
    let (router, state, vm, calls) = owned(root.path()).await;
    let mut external = vm.clone();
    external.id = Uuid::new_v4();
    external.name = "external".into();
    state.store.insert(&external).unwrap();
    state
        .vms
        .lock()
        .unwrap()
        .insert(external.id, external.clone());
    let id = Uuid::new_v4();
    let body = planned(&router, &empty(), id).await;
    let (code, error) = request(&router, "POST", "/api/stacks/demo/apply", body).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "stack_external_dependency");
    assert!(calls.lock().unwrap().is_empty());
    assert!(state.store.stored_stack_apply(id).unwrap().is_none());
    assert_eq!(state.store.load_all().unwrap().len(), 2);
    state.store.delete(external.id).unwrap();
    state.vms.lock().unwrap().remove(&external.id);
    let mut running = vm.clone();
    running.state = firecrab_api_types::VmState::Running;
    state.store.update(&running).unwrap();
    state.vms.lock().unwrap().insert(vm.id, running);
    let body = planned(&router, &empty(), Uuid::new_v4()).await;
    let (_, error) = request(&router, "POST", "/api/stacks/demo/apply", body).await;
    assert_eq!(error["error"]["code"], "stack_stop_permission_required");
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stack_prune_external_volumes_are_preserved_and_disk_changes_invalidate_review() {
    let root = tempfile::tempdir().unwrap();
    let (router, state, vm, calls) = owned(root.path()).await;
    let stale = planned(&router, &empty(), Uuid::new_v4()).await;
    let mut changed = vm.clone();
    changed.disk_generation = Some(Uuid::new_v4());
    state.store.update(&changed).unwrap();
    state.vms.lock().unwrap().insert(vm.id, changed);
    let (_, error) = request(&router, "POST", "/api/stacks/demo/apply", stale).await;
    assert_eq!(error["error"]["code"], "stale_stack_plan");
    let volume = firecrab_api_types::MicroBlockStorageResponse {
        id: Uuid::new_v4(),
        name: "external-data".into(),
        size_gib: 1,
        storage_root: "default".into(),
        state: "creating".into(),
        attached_vm_id: None,
        mount_path: None,
    };
    state.store.insert_block_storage(&volume, None).unwrap();
    state.store.finish_block_storage_create(volume.id).unwrap();
    state
        .store
        .attach_block_storages(
            vm.id,
            &[firecrab_api_types::VmBlockStorageMount {
                volume_id: volume.id,
                mount_path: "/data".into(),
            }],
        )
        .unwrap();
    let body = planned(&router, &empty(), Uuid::new_v4()).await;
    let (_, error) = request(&router, "POST", "/api/stacks/demo/apply", body).await;
    assert_eq!(error["error"]["code"], "stack_external_volume_attached");
    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(
        state.store.block_storage(volume.id).unwrap().attached_vm_id,
        Some(vm.id)
    );
}

#[tokio::test]
async fn stack_network_replacement_recreates_dependents_after_reverse_order_removal() {
    let root = tempfile::tempdir().unwrap();
    let (router, state, vm, _) = owned(root.path()).await;
    let mut stack = desired(&vm);
    stack["lifecycle"] = json!({"replace":true});
    stack["microNetworks"]["app"]["subnetCidr"] = json!("172.31.24.0/24");
    let id = Uuid::new_v4();
    let body = planned(&router, &stack, id).await;
    let (code, error) = request(&router, "POST", "/api/stacks/demo/apply", body).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{error}");
    let status = terminal(&router, id).await;
    assert_eq!(status.state, ApplyState::Succeeded, "{status:?}");
    assert_eq!(
        status
            .changes
            .iter()
            .map(|s| (s.change.kind, s.change.action))
            .collect::<Vec<_>>(),
        [
            (ResourceKind::Vm, Action::Delete),
            (ResourceKind::Network, Action::Delete),
            (ResourceKind::Network, Action::Create),
            (ResourceKind::Vm, Action::Create)
        ]
    );
    let network = state.store.list_micro_networks().unwrap().pop().unwrap();
    assert_ne!(network.id, vm.micro_network_id);
    let current = state
        .store
        .load_all()
        .unwrap()
        .into_values()
        .next()
        .unwrap();
    assert_eq!(current.micro_network_id, network.id);
    assert_ne!(current.id, vm.id);
    assert_eq!(
        std::fs::read(
            VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id)
                .rootfs(vm.disk_generation.unwrap())
        )
        .unwrap(),
        b"guest-data-KEEP"
    );
}

#[tokio::test]
async fn stack_pruning_cleans_services_first_without_touching_unowned_services() {
    let root = tempfile::tempdir().unwrap();
    let (router, state, _, _) = owned(root.path()).await;
    let service = service();
    state.store.insert_micro_service(&service).unwrap();
    state
        .store
        .bind_stack_resource(
            "demo",
            &Binding {
                kind: ResourceKind::Service,
                key: "web".into(),
                resource_id: service.id,
            },
        )
        .unwrap();
    let mut external = service.clone();
    external.id = Uuid::new_v4();
    external.spec.name = "external".into();
    state.store.insert_micro_service(&external).unwrap();
    let id = Uuid::new_v4();
    let body = planned(&router, &empty(), id).await;
    let (code, error) = request(&router, "POST", "/api/stacks/demo/apply", body).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{error}");
    let status = terminal(&router, id).await;
    assert_eq!(status.state, ApplyState::Succeeded, "{status:?}");
    assert_eq!(
        status
            .changes
            .iter()
            .map(|s| s.change.kind)
            .collect::<Vec<_>>(),
        [
            ResourceKind::Service,
            ResourceKind::Vm,
            ResourceKind::Network
        ]
    );
    assert_eq!(
        state.store.micro_service(external.id).unwrap(),
        Some(external)
    );
    assert!(state.store.micro_service(service.id).unwrap().is_none());
}

fn queued(state: &AppState, stack: &Value) -> (firecrab_stack::StackRecord, ApplyStatus, String) {
    let stack: Stack = serde_json::from_value(stack.clone()).unwrap();
    let record = state.store.stack("demo").unwrap().unwrap();
    let inventory = crate::handlers::stacks::inventory(&state.store, &state.templates).unwrap();
    let plan = firecrab_stack::plan(&stack, &record, &inventory).unwrap();
    let steps = super::super::lifecycle::steps(&plan, &inventory);
    let status = ApplyStatus {
        lifecycle: stack.lifecycle,
        inputs: plan.inputs,
        operation_id: Uuid::new_v4(),
        stack_id: record.id,
        name: record.name.clone(),
        state: ApplyState::Queued,
        changes: steps,
        error: None,
        git_revision: None,
    };
    (record, status, firecrab_stack::desired_hash(&stack))
}

#[tokio::test]
async fn stack_replacement_journal_reserves_new_uuid_and_atomically_transfers_only_after_deletion()
{
    let root = tempfile::tempdir().unwrap();
    let (_, state, vm, _) = owned(root.path()).await;
    let mut stack = desired(&vm);
    stack["lifecycle"] = json!({"replace":true});
    stack["microVMs"]["worker"]["name"] = json!("next");
    let (record, mut status, hash) = queued(&state, &stack);
    let new = status
        .changes
        .iter()
        .find(|s| s.replacement_for == Some(vm.id))
        .unwrap()
        .change
        .resource_id
        .unwrap();
    state
        .store
        .begin_stack_apply(&record, &hash, false, &status, None)
        .unwrap();
    assert_eq!(state.store.stack("demo").unwrap().unwrap(), record);
    assert!(
        state
            .store
            .guard_stack_delete_resource(status.operation_id, ResourceKind::Vm, vm.id)
            .is_err()
    );
    status.state = ApplyState::Running;
    status.changes[0].state = StepState::Running;
    state.store.save_stack_apply(&status).unwrap();
    assert!(
        state
            .store
            .finish_stack_delete(status.operation_id, 0)
            .is_err()
    );
    let mut bad = status.clone();
    bad.lifecycle.delete_vm_disks = true;
    assert!(state.store.save_stack_apply(&bad).is_err());
    bad = status.clone();
    bad.changes[0].change.resource_id = Some(Uuid::new_v4());
    assert!(state.store.save_stack_apply(&bad).is_err());
    state.store.delete(vm.id).unwrap();
    let after = state
        .store
        .finish_stack_delete(status.operation_id, 0)
        .unwrap();
    assert_eq!(after.changes[0].state, StepState::Succeeded);
    assert_eq!(
        state
            .store
            .stack("demo")
            .unwrap()
            .unwrap()
            .bindings
            .iter()
            .find(|b| b.kind == ResourceKind::Vm)
            .unwrap()
            .resource_id,
        new
    );
    assert!(
        state
            .store
            .finish_stack_delete(status.operation_id, 0)
            .is_err()
    );
    state.store.interrupt_stack_applies().unwrap();
    let (retry, fresh) = state
        .store
        .begin_stack_apply(&record, &hash, false, &status, None)
        .unwrap();
    assert!(!fresh);
    assert_eq!(retry.state, ApplyState::Interrupted);
    assert_eq!(retry.changes[0].vm_disk, after.changes[0].vm_disk);
    assert!(
        state
            .store
            .guard_stack_delete_resource(status.operation_id, ResourceKind::Vm, vm.id)
            .is_err()
    );
    assert!(
        VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id)
            .rootfs(vm.disk_generation.unwrap())
            .exists()
    );
}

#[tokio::test]
async fn stack_destructive_admission_rolls_back_unapproved_or_foreign_delete_targets() {
    let root = tempfile::tempdir().unwrap();
    let (_, state, vm, _) = owned(root.path()).await;
    let (record, status, hash) = queued(&state, &empty());
    let mut bad = status.clone();
    bad.lifecycle.prune = false;
    assert!(
        state
            .store
            .begin_stack_apply(&record, &hash, false, &bad, None)
            .is_err()
    );
    bad = status.clone();
    bad.changes[0].change.resource_id = Some(Uuid::new_v4());
    assert!(
        state
            .store
            .begin_stack_apply(&record, &hash, false, &bad, None)
            .is_err()
    );
    assert!(
        state
            .store
            .stored_stack_apply(status.operation_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(state.store.stack("demo").unwrap().unwrap(), record);
    assert!(state.store.load_all().unwrap().contains_key(&vm.id));
}

#[tokio::test]
async fn stack_prune_never_selects_current_previous_or_retiring_service_replicas() {
    for owner in ["current", "previous", "retiring"] {
        let root = tempfile::tempdir().unwrap();
        let (router, state, vm, calls) = owned(root.path()).await;
        let service = service();
        state.store.insert_micro_service(&service).unwrap();
        let mut current = deployment(&service);
        let mut previous = current.clone();
        if owner == "current" {
            current.instances.push(replica(vm.id));
        } else {
            if owner == "previous" {
                previous.instances.push(replica(vm.id));
            }
            let mut journal = update(previous);
            journal.completed = true;
            if owner == "retiring" {
                journal.retiring.push(vm.id);
            }
            current.update = Some(Box::new(journal));
        }
        state.store.save_service_deployment(&current).unwrap();
        assert!(state.store.vm_has_service_owner(vm.id).unwrap(), "{owner}");
        assert!(
            crate::handlers::stacks::inventory(&state.store, &state.templates)
                .unwrap()
                .vms
                .is_empty()
        );
        state.store.register_stack("other").unwrap();
        assert!(
            state
                .store
                .bind_stack_resource(
                    "other",
                    &Binding {
                        kind: ResourceKind::Vm,
                        key: "replica".into(),
                        resource_id: vm.id,
                    }
                )
                .is_err()
        );
        let mut stack = desired(&vm);
        stack["microVMs"] = json!({});
        stack["lifecycle"] = json!({"prune":true});
        let id = Uuid::new_v4();
        let body = planned(&router, &stack, id).await;
        let (code, error) = request(&router, "POST", "/api/stacks/demo/apply", body).await;
        assert_eq!(code, StatusCode::CONFLICT, "{owner}: {error}");
        assert_eq!(error["error"]["code"], "protected_stack_vm");
        assert!(state.store.stored_stack_apply(id).unwrap().is_none());
        assert!(state.store.load_all().unwrap().contains_key(&vm.id));
        assert!(calls.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn stack_prune_uses_service_cleanup_only_for_owned_replicas_and_retains_standalone_data() {
    let root = tempfile::tempdir().unwrap();
    let (router, state, standalone, calls) = owned(root.path()).await;
    let owned_service = service();
    state.store.insert_micro_service(&owned_service).unwrap();
    state
        .store
        .bind_stack_resource(
            "demo",
            &Binding {
                kind: ResourceKind::Service,
                key: "web".into(),
                resource_id: owned_service.id,
            },
        )
        .unwrap();
    let mut owned_replica = standalone.clone();
    owned_replica.id = Uuid::new_v4();
    owned_replica.name = "owned-replica".into();
    state.store.insert(&owned_replica).unwrap();
    state
        .vms
        .lock()
        .unwrap()
        .insert(owned_replica.id, owned_replica.clone());
    let artifacts = VmArtifactPaths::for_vm(
        &state.vms_dir_for(&owned_replica.storage_root),
        owned_replica.id,
    );
    artifacts.ensure_directories().unwrap();
    std::fs::write(
        artifacts.rootfs(owned_replica.disk_generation.unwrap()),
        b"disposable-replica",
    )
    .unwrap();
    let mut active = deployment(&owned_service);
    active.phase = ServicePhase::Running;
    active.request.micro_network_id = Some(standalone.micro_network_id);
    active.instances.push(replica(owned_replica.id));
    state.store.save_service_deployment(&active).unwrap();
    let mut external_service = service();
    external_service.spec.name = "external".into();
    state.store.insert_micro_service(&external_service).unwrap();
    let mut external_vm = owned_replica.clone();
    external_vm.id = Uuid::new_v4();
    external_vm.name = "external-replica".into();
    external_vm.micro_network_id = Uuid::new_v4();
    state.store.insert(&external_vm).unwrap();
    state
        .vms
        .lock()
        .unwrap()
        .insert(external_vm.id, external_vm.clone());
    let mut external = deployment(&external_service);
    external.phase = ServicePhase::Running;
    external.request.micro_network_id = Some(external_vm.micro_network_id);
    external.instances.push(replica(external_vm.id));
    state.store.save_service_deployment(&external).unwrap();
    let id = Uuid::new_v4();
    let mut body = planned(&router, &empty(), id).await;
    let (code, error) = request(&router, "POST", "/api/stacks/demo/apply", body.clone()).await;
    assert_eq!(code, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "stack_stop_permission_required");
    assert!(calls.lock().unwrap().is_empty());
    body["allowStop"] = json!(true);
    assert_eq!(
        request(&router, "POST", "/api/stacks/demo/apply", body)
            .await
            .0,
        StatusCode::ACCEPTED
    );
    let status = terminal(&router, id).await;
    assert_eq!(status.state, ApplyState::Succeeded, "{status:?}");
    assert!(
        state
            .store
            .micro_service(owned_service.id)
            .unwrap()
            .is_none()
    );
    assert!(!artifacts.dir.exists());
    assert_eq!(
        state
            .store
            .load_all()
            .unwrap()
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        [external_vm.id]
    );
    assert_eq!(
        state.store.micro_service(external_service.id).unwrap(),
        Some(external_service)
    );
    assert_eq!(
        state.store.service_deployment(external.service_id).unwrap(),
        Some(external)
    );
    assert_eq!(
        std::fs::read(
            VmArtifactPaths::for_vm(&state.vms_dir_for(&standalone.storage_root), standalone.id)
                .rootfs(standalone.disk_generation.unwrap())
        )
        .unwrap(),
        b"guest-data-KEEP"
    );
}

#[tokio::test]
async fn stack_prune_blocks_busy_and_unfinished_service_recovery_before_mutation() {
    for scenario in ["busy", "update", "replica"] {
        let root = tempfile::tempdir().unwrap();
        let (router, state, _, calls) = owned(root.path()).await;
        let service = service();
        state.store.insert_micro_service(&service).unwrap();
        state
            .store
            .bind_stack_resource(
                "demo",
                &Binding {
                    kind: ResourceKind::Service,
                    key: "web".into(),
                    resource_id: service.id,
                },
            )
            .unwrap();
        let mut current = deployment(&service);
        match scenario {
            "busy" => current.phase = ServicePhase::Queued,
            "update" => current.update = Some(Box::new(update(current.clone()))),
            _ => {
                current.recovery = Some(Box::new(firecrab_api_types::ServiceRecovery {
                    repair: Some(firecrab_api_types::ReplicaRepair {
                        id: Uuid::new_v4(),
                        vm_id: Uuid::new_v4(),
                        replacement_id: None,
                        stage: firecrab_api_types::ReplicaRepairStage::Paused,
                        transition: None,
                    }),
                    ..Default::default()
                }))
            }
        }
        state.store.save_service_deployment(&current).unwrap();
        let body = planned(&router, &empty(), Uuid::new_v4()).await;
        let (code, error) = request(&router, "POST", "/api/stacks/demo/apply", body).await;
        assert_eq!(code, StatusCode::CONFLICT, "{error}");
        assert!(calls.lock().unwrap().is_empty());
        assert_eq!(
            state.store.stack("demo").unwrap().unwrap().bindings.len(),
            3
        );
    }
}

#[tokio::test]
async fn stack_prune_requires_stop_permission_for_a_retained_internal_route() {
    let root = tempfile::tempdir().unwrap();
    let (router, state, vm, calls) = owned(root.path()).await;
    let service = service();
    state.store.insert_micro_service(&service).unwrap();
    state
        .store
        .bind_stack_resource(
            "demo",
            &Binding {
                kind: ResourceKind::Service,
                key: "web".into(),
                resource_id: service.id,
            },
        )
        .unwrap();
    let mut current = deployment(&service);
    current.internal_route_owned = true;
    state.store.save_service_deployment(&current).unwrap();
    let body = planned(&router, &empty(), Uuid::new_v4()).await;
    let (code, error) = request(&router, "POST", "/api/stacks/demo/apply", body).await;
    assert_eq!(code, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "stack_stop_permission_required");
    assert!(calls.lock().unwrap().is_empty());
    assert!(state.store.load_all().unwrap().contains_key(&vm.id));
    assert_eq!(
        state.store.stack("demo").unwrap().unwrap().bindings.len(),
        3
    );
}

#[tokio::test]
async fn stack_network_delete_rechecks_late_external_vm_references_under_its_lock() {
    let root = tempfile::tempdir().unwrap();
    let (_, state, vm, calls) = owned(root.path()).await;
    let (record, mut status, hash) = queued(&state, &empty());
    state
        .store
        .begin_stack_apply(&record, &hash, false, &status, None)
        .unwrap();
    status.state = ApplyState::Running;
    status.changes[0].state = StepState::Running;
    state.store.save_stack_apply(&status).unwrap();
    status = super::super::lifecycle::delete(&state, &status, 0, false)
        .await
        .unwrap();
    let expected = state.store.stack("demo").unwrap().unwrap();
    let mut external = vm.clone();
    external.id = Uuid::new_v4();
    external.name = "late-external".into();
    state.store.insert(&external).unwrap();
    state
        .vms
        .lock()
        .unwrap()
        .insert(external.id, external.clone());
    status.changes[1].state = StepState::Running;
    state.store.save_stack_apply(&status).unwrap();
    let count = calls.lock().unwrap().len();
    let error = super::super::lifecycle::delete(&state, &status, 1, false)
        .await
        .unwrap_err();
    assert_eq!(error.code(), "stack_external_dependency");
    assert_eq!(state.store.stack("demo").unwrap().unwrap(), expected);
    assert!(state.store.load_all().unwrap().contains_key(&external.id));
    assert_eq!(calls.lock().unwrap().len(), count);
}

#[tokio::test]
async fn stack_network_replacement_rejects_external_overlap_before_deleting_dependents() {
    let root = tempfile::tempdir().unwrap();
    let (router, state, vm, calls) = owned(root.path()).await;
    let mut external = state.store.list_micro_networks().unwrap().pop().unwrap();
    external.id = Uuid::new_v4();
    external.name = "external".into();
    external.subnet_cidr = "172.31.100.0/24".into();
    state.store.insert_micro_network(&external).unwrap();
    let mut stack = desired(&vm);
    stack["microNetworks"]["app"]["subnetCidr"] = json!(external.subnet_cidr);
    stack["lifecycle"] = json!({"replace":true});
    let body = planned(&router, &stack, Uuid::new_v4()).await;
    let (code, error) = request(&router, "POST", "/api/stacks/demo/apply", body).await;
    assert_eq!(code, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "stack_external_dependency");
    assert!(calls.lock().unwrap().is_empty());
    assert!(state.store.load_all().unwrap().contains_key(&vm.id));
    assert_eq!(state.store.list_micro_networks().unwrap().len(), 2);
}

#[tokio::test]
async fn stack_replacement_failure_preserves_exact_bindings_and_never_replays_deletion() {
    let root = tempfile::tempdir().unwrap();
    let (_, state, vm, _) = owned(root.path()).await;
    let socket = root.path().join("failing-helper.sock");
    let (_, calls) = crate::network::test_support::spawn_recording_helper(
        &socket,
        Some("remove_micro_network_bridge"),
    );
    let state = state.with_test_network(crate::network::NetworkClient::with_socket_path(socket));
    let router = router(&state);
    let mut stack = desired(&vm);
    stack["microNetworks"]["app"]["name"] = json!("next-network");
    stack["lifecycle"] = json!({"replace":true});
    let id = Uuid::new_v4();
    let body = planned(&router, &stack, id).await;
    assert_eq!(
        request(&router, "POST", "/api/stacks/demo/apply", body.clone())
            .await
            .0,
        StatusCode::ACCEPTED
    );
    let status = terminal(&router, id).await;
    assert_eq!(status.state, ApplyState::Failed, "{status:?}");
    assert_eq!(status.changes[0].state, StepState::Succeeded);
    assert_eq!(status.changes[1].state, StepState::Failed);
    assert!(
        status.changes[2..]
            .iter()
            .all(|s| s.state == StepState::Skipped)
    );
    let bindings = state.store.stack("demo").unwrap().unwrap().bindings;
    assert!(
        bindings
            .iter()
            .any(|b| b.kind == ResourceKind::Network && b.resource_id == vm.micro_network_id)
    );
    let next_vm = status
        .changes
        .iter()
        .find(|s| s.replacement_for == Some(vm.id))
        .unwrap()
        .change
        .resource_id
        .unwrap();
    assert!(bindings.iter().any(|b| b.resource_id == next_vm));
    assert!(!state.store.load_all().unwrap().contains_key(&next_vm));
    assert!(
        VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id)
            .rootfs(vm.disk_generation.unwrap())
            .exists()
    );
    let count = calls.lock().unwrap().len();
    assert_eq!(
        request(&router, "POST", "/api/stacks/demo/apply", body)
            .await
            .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(calls.lock().unwrap().len(), count);
}

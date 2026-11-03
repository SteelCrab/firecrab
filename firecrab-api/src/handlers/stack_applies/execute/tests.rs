use super::*;
use firecrab_api_types::{MicroServiceDeployment, MicroServiceResponse, MicroServiceSpec};

async fn fixture() -> (tempfile::TempDir, AppState, MicroServiceDeployment) {
    let root = tempfile::tempdir().unwrap();
    let templates =
        crate::templates::TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap();
    let state = AppState::with_db_file(templates, root.path().join("state.db"))
        .await
        .unwrap();
    let service_id = Uuid::new_v4();
    let spec: MicroServiceSpec = serde_json::from_value(serde_json::json!({
        "name":"web", "image":"ghcr.io/team/app:v1", "port":8080, "ram":128, "cpu":1,
    }))
    .unwrap();
    state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id: service_id,
            spec: spec.clone(),
        })
        .unwrap();
    let deployment = MicroServiceDeployment {
        recovery: None,
        started_at: 0,
        finished_at: None,
        update: None,
        id: Uuid::new_v4(),
        service_id,
        spec,
        request: DeployMicroServiceRequest {
            micro_network_id: None,
            host_port: 30100,
        },
        phase: ServicePhase::Queued,
        domain_route_owned: false,
        internal_route_owned: false,
        digest: None,
        instances: vec![],
        log: vec![],
        error: None,
    };
    (root, state, deployment)
}

async fn recorded_stack_service() -> (
    tempfile::TempDir,
    AppState,
    MicroServiceDeployment,
    Uuid,
    Uuid,
) {
    use firecrab_stack::{ApplyStatus, Binding, ResourceKind};
    let (root, state, mut deployment) = fixture().await;
    state.store.register_stack("service-replay").unwrap();
    let registration = state
        .store
        .bind_stack_resource(
            "service-replay",
            &Binding {
                kind: ResourceKind::Service,
                key: "app".into(),
                resource_id: deployment.service_id,
            },
        )
        .unwrap();
    let apply = Uuid::new_v4();
    let network = Uuid::new_v4();
    let status:ApplyStatus=serde_json::from_value(serde_json::json!({"operationId":apply,"stackId":registration.id,
        "name":registration.name,"state":"queued","changes":[{"kind":"service","key":"app","resourceId":deployment.service_id,
        "action":"deploy","fields":[],"reason":null,"state":"running"}],"error":null})).unwrap();
    state
        .store
        .begin_stack_apply(&registration, "original-desired-hash", false, &status, None)
        .unwrap();
    deployment.id = deploy_operation_id(apply, deployment.service_id);
    deployment.request.micro_network_id = Some(network);
    deployment.phase = ServicePhase::Running;
    deployment
        .instances
        .push(firecrab_api_types::ServiceInstance {
            vm_id: Uuid::new_v4(),
            ipv4: "127.0.0.1".into(),
            host_port: 30100,
            state: VmState::Running,
            healthy: true,
        });
    state.store.save_service_deployment(&deployment).unwrap();
    (root, state, deployment, apply, network)
}

#[tokio::test]
async fn iac_stack_service_lost_ack_reuses_original_child_before_stop_or_deploy() {
    let (_root, state, mut original, apply, network) = recorded_stack_service().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    original.spec.port = listener.local_addr().unwrap().port();
    state.store.save_service_deployment(&original).unwrap();
    let health = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().fallback(|| async { axum::http::StatusCode::OK }),
        )
        .await
        .unwrap();
    });
    let vm_id = original.instances[0].vm_id;
    let mut vm = vms::test_support::record("original-replica", vm_id);
    vm.state = VmState::Running;
    state.vms.lock().unwrap().insert(vm_id, vm);
    let stack:Stack=serde_json::from_value(serde_json::json!({"apiVersion":"firecrab.dev/v1alpha1","kind":"Stack","name":"service-replay",
        "microNetworks":{"private":{"name":"private","subnetCidr":"172.31.20.0/24"}},
        "microServices":{"app":{"network":"private","hostPort":30100,"config":original.spec}}})).unwrap();
    let change = Change {
        kind: ResourceKind::Service,
        key: "app".into(),
        resource_id: Some(original.service_id),
        action: Action::Deploy,
        fields: vec![],
        reason: None,
    };
    execute(
        &state,
        &stack,
        &change,
        &BTreeMap::from([("private", network)]),
        &PlanInputs::default(),
        false,
        apply,
    )
    .await
    .unwrap();
    let current = state
        .store
        .service_deployment(original.service_id)
        .unwrap()
        .unwrap();
    assert_eq!(current.id, original.id);
    assert_eq!(current.spec, original.spec);
    assert_eq!(current.instances, original.instances);
    assert_eq!(state.vms.lock().unwrap().len(), 1);
    health.abort();
    assert_ne!(
        deploy_operation_id(apply, original.service_id),
        deploy_operation_id(apply, Uuid::new_v4())
    );
}

#[tokio::test]
async fn iac_stack_service_recorded_child_rejects_changed_inputs_and_foreign_parent() {
    let (_root, state, original, apply, network) = recorded_stack_service().await;
    let mut changed = original.spec.clone();
    changed.image = "ghcr.io/team/other:v2".into();
    assert_eq!(
        resume_recorded_service(&state, apply, original.service_id, &changed, network, 30100)
            .await
            .unwrap_err()
            .code(),
        "stack_deployment_identity_conflict"
    );
    assert_eq!(
        resume_recorded_service(
            &state,
            apply,
            original.service_id,
            &original.spec,
            network,
            30101
        )
        .await
        .unwrap_err()
        .code(),
        "stack_deployment_identity_conflict"
    );
    let foreign = Uuid::new_v4();
    let mut colliding = original.clone();
    colliding.id = deploy_operation_id(foreign, original.service_id);
    state.store.save_service_deployment(&colliding).unwrap();
    assert_eq!(
        resume_recorded_service(
            &state,
            foreign,
            original.service_id,
            &original.spec,
            network,
            30100
        )
        .await
        .unwrap_err()
        .code(),
        "stack_source_managed"
    );
    assert_eq!(
        state
            .store
            .service_deployment(original.service_id)
            .unwrap()
            .unwrap()
            .id,
        colliding.id
    );
}

#[tokio::test]
async fn iac_stack_service_failed_original_child_is_not_replaced_by_another_deployment() {
    let (_root, state, mut original, apply, network) = recorded_stack_service().await;
    original.phase = ServicePhase::Failed;
    state.store.save_service_deployment(&original).unwrap();
    assert_eq!(
        resume_recorded_service(
            &state,
            apply,
            original.service_id,
            &original.spec,
            network,
            30100
        )
        .await
        .unwrap_err()
        .code(),
        "stack_deployment_failed"
    );
    assert_eq!(
        state
            .store
            .service_deployment(original.service_id)
            .unwrap()
            .unwrap()
            .id,
        original.id
    );
    assert!(state.vms.lock().unwrap().is_empty());
}

#[tokio::test]
async fn service_wait_rejects_missing_replaced_and_failed_deployments_and_requires_full_cleanup() {
    let (_root, state, mut deployment) = fixture().await;
    let id = Uuid::new_v4();
    let wait = |deployment_id, running| {
        wait_service(
            &state,
            deployment.service_id,
            deployment_id,
            running,
            Duration::from_millis(30),
            id,
        )
    };
    assert_eq!(
        wait(deployment.id, true).await.unwrap_err().code(),
        "stack_deployment_missing"
    );
    state.store.save_service_deployment(&deployment).unwrap();
    assert_eq!(
        wait(Uuid::new_v4(), true).await.unwrap_err().code(),
        "stack_deployment_changed"
    );
    assert_eq!(
        wait(deployment.id, true).await.unwrap_err().code(),
        "stack_deployment_timeout"
    );
    deployment.phase = ServicePhase::Failed;
    state.store.save_service_deployment(&deployment).unwrap();
    assert_eq!(
        wait(deployment.id, true).await.unwrap_err().code(),
        "stack_deployment_failed"
    );
    deployment.phase = ServicePhase::Stopped;
    deployment.domain_route_owned = true;
    state.store.save_service_deployment(&deployment).unwrap();
    assert_eq!(
        wait(deployment.id, false).await.unwrap_err().code(),
        "stack_deployment_failed"
    );
    assert_eq!(
        service_stop_needed(&state, deployment.service_id, false, id)
            .await
            .unwrap_err()
            .code(),
        "stack_stop_permission_required"
    );
    assert!(
        service_stop_needed(&state, deployment.service_id, true, id)
            .await
            .unwrap()
    );
    deployment.domain_route_owned = false;
    state.store.save_service_deployment(&deployment).unwrap();
    wait(deployment.id, false).await.unwrap();
    assert!(
        !service_stop_needed(&state, deployment.service_id, false, id)
            .await
            .unwrap()
    );
    deployment.phase = ServicePhase::Starting;
    state.store.save_service_deployment(&deployment).unwrap();
    assert_eq!(
        service_stop_needed(&state, deployment.service_id, true, id)
            .await
            .unwrap_err()
            .code(),
        "service_busy"
    );
}

#[tokio::test]
async fn vm_edits_reject_missing_internal_and_transitional_instances_even_with_stop_permission() {
    let (_root, state, _) = fixture().await;
    let id = Uuid::new_v4();
    let vm_id = Uuid::new_v4();
    assert_eq!(
        editable_vm(&state, vm_id, true, id).unwrap_err().code(),
        "not_found"
    );
    let mut vm = vms::test_support::record("worker", vm_id);
    vm.purpose = VmPurpose::Builder;
    state.vms.lock().unwrap().insert(vm_id, vm.clone());
    assert_eq!(
        editable_vm(&state, vm_id, true, id).unwrap_err().code(),
        "protected_stack_vm"
    );
    vm.purpose = VmPurpose::Instance;
    for phase in [VmState::Starting, VmState::Stopping, VmState::Paused] {
        vm.state = phase;
        state.vms.lock().unwrap().insert(vm_id, vm.clone());
        assert_eq!(
            editable_vm(&state, vm_id, true, id).unwrap_err().code(),
            "stack_vm_not_editable"
        );
    }
}

#[tokio::test]
async fn configuration_noop_does_not_report_an_unhealthy_service_as_successful() {
    let (_root, state, mut deployment) = fixture().await;
    let stack: Stack = serde_json::from_value(
        serde_json::json!({"apiVersion":"firecrab.dev/v1alpha1","kind":"Stack","name":"demo"}),
    )
    .unwrap();
    let change = Change {
        kind: ResourceKind::Service,
        key: "web".into(),
        resource_id: Some(deployment.service_id),
        action: Action::Noop,
        fields: vec![],
        reason: None,
    };
    for phase in [
        None,
        Some(ServicePhase::Failed),
        Some(ServicePhase::Running),
    ] {
        if let Some(phase) = phase {
            deployment.phase = phase;
            state.store.save_service_deployment(&deployment).unwrap();
        }
        assert_eq!(
            execute(
                &state,
                &stack,
                &change,
                &BTreeMap::new(),
                &PlanInputs::default(),
                false,
                Uuid::new_v4()
            )
            .await
            .unwrap_err()
            .code(),
            "stack_service_not_healthy"
        );
    }
}

#[tokio::test]
async fn domain_updates_use_deploy_only_when_the_endpoint_and_network_are_stable() {
    let (_root, state, mut deployment) = fixture().await;
    let network = Uuid::new_v4();
    deployment.phase = ServicePhase::Running;
    deployment.domain_route_owned = true;
    deployment.spec.domain = Some("web.example.com".into());
    deployment.request.micro_network_id = Some(network);
    state.store.save_service_deployment(&deployment).unwrap();
    let mut desired = deployment.spec.clone();
    desired.image = "ghcr.io/team/app:v2".into();
    let id = Uuid::new_v4();
    assert!(
        supports_live_update(&state, deployment.service_id, &desired, network, id)
            .await
            .unwrap()
    );
    assert!(
        !supports_live_update(&state, deployment.service_id, &desired, Uuid::new_v4(), id)
            .await
            .unwrap()
    );
    desired.domain = Some("another.example.com".into());
    assert!(
        !supports_live_update(&state, deployment.service_id, &desired, network, id)
            .await
            .unwrap()
    );
    assert_eq!(
        deploy_operation_id(id, deployment.service_id),
        deploy_operation_id(id, deployment.service_id)
    );
    assert_ne!(
        deploy_operation_id(id, deployment.service_id),
        deploy_operation_id(Uuid::new_v4(), deployment.service_id)
    );
}

#[tokio::test]
async fn iac_service_child_ownership_blocks_stack_delete_before_stop_and_inside_admission() {
    use crate::persistence::PersistenceError;
    use firecrab_stack::{Binding, ChangeResourceOwner, ManagedResource, Manager, ResourceOwner};
    let (_root, state, mut deployment) = fixture().await;
    deployment.phase = ServicePhase::Running;
    deployment.domain_route_owned = true;
    state.store.save_service_deployment(&deployment).unwrap();
    let stack = state.store.register_stack("retained").unwrap();
    state
        .store
        .bind_stack_resource(
            "retained",
            &Binding {
                kind: ResourceKind::Service,
                key: "web".into(),
                resource_id: deployment.service_id,
            },
        )
        .unwrap();
    let current = state.store.stack("retained").unwrap().unwrap();
    let desired: Stack = serde_json::from_value(
        serde_json::json!({"apiVersion":"firecrab.dev/v1alpha1","kind":"Stack","name":"retained"}),
    )
    .unwrap();
    let status:ApplyStatus=serde_json::from_value(serde_json::json!({"operationId":Uuid::new_v4(),"stackId":stack.id,"name":"retained","state":"queued","lifecycle":{"prune":true},"changes":[{"kind":"service","key":"web","resourceId":deployment.service_id,"action":"delete","fields":[],"reason":null,"state":"pending"}],"error":null})).unwrap();
    for kind in ["service_auto_deploy", "service_otlp"] {
        let target = ManagedResource {
            kind: kind.into(),
            id: deployment.service_id.to_string(),
        };
        let before = state.store.iac_resource_owner(&target).unwrap();
        let owned = state
            .store
            .change_iac_resource_owner(&ChangeResourceOwner {
                operation_id: Uuid::new_v4(),
                expected_endpoint_id: before.endpoint_id,
                resource: target,
                expected_generation: before.generation,
                expected_owner: before.owner,
                owner: Some(ResourceOwner {
                    manager: Manager::Terraform,
                    id: Uuid::new_v4(),
                    key: "firecrab_service_settings.web".into(),
                }),
            })
            .unwrap();
        assert!(
            super::super::lifecycle::preflight(
                &state,
                &desired,
                &status.changes,
                true,
                status.operation_id
            )
            .await
            .is_err()
        );
        assert!(matches!(
            state
                .store
                .begin_stack_apply(&current, "test", true, &status, None),
            Err(PersistenceError::IacManagedDependent)
        ));
        assert!(
            state
                .store
                .stored_stack_apply(status.operation_id)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            state
                .store
                .service_deployment(deployment.service_id)
                .unwrap()
                .unwrap(),
            deployment
        );
        state
            .store
            .change_iac_resource_owner(&ChangeResourceOwner {
                operation_id: Uuid::new_v4(),
                expected_endpoint_id: owned.endpoint_id,
                resource: owned.resource,
                expected_generation: owned.generation,
                expected_owner: owned.owner,
                owner: None,
            })
            .unwrap();
    }
}

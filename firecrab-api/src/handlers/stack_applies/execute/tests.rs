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

use super::*;
use crate::secrets::{Secret, SecretSummary};
use firecrab_api_types::{MicroServiceResponse, ServicePhase};
use firecrab_stack::{ApplyState, Binding, StepState};

async fn fixture() -> (
    tempfile::TempDir,
    AppState,
    Stack,
    ApplyStatus,
    MicroServiceDeployment,
) {
    let root = tempfile::tempdir().unwrap();
    let templates =
        crate::templates::TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap();
    let state = AppState::with_db_file(templates, root.path().join("state.db"))
        .await
        .unwrap();
    let secret = Uuid::new_v4();
    state
        .secrets
        .write(|contents| {
            contents.secrets.push(Secret {
                summary: SecretSummary {
                    id: secret,
                    name: "original".into(),
                    description: String::new(),
                    version: 1,
                    created_at: 0,
                    updated_at: 0,
                    source: None,
                },
                value: "DO-NOT-PRINT-original-pinned-value".into(),
            });
            Ok(())
        })
        .unwrap();
    let stack:Stack=serde_json::from_value(serde_json::json!({"apiVersion":"firecrab.dev/v1alpha1","kind":"Stack","name":"credentials",
        "microNetworks":{"private":{"name":"private","subnetCidr":"172.31.20.0/24"}},
        "microServices":{"app":{"network":"private","hostPort":30100,"config":{"name":"app","image":format!("ghcr.io/team/app@sha256:{}","a".repeat(64)),
            "port":8080,"cpu":1,"ram":128,"secrets":{"TOKEN":secret}}}}})).unwrap();
    let service = Uuid::new_v4();
    let spec = stack.micro_services["app"].config.clone();
    state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id: service,
            spec: spec.clone(),
        })
        .unwrap();
    state.store.register_stack(&stack.name).unwrap();
    let record = state
        .store
        .bind_stack_resource(
            &stack.name,
            &Binding {
                kind: ResourceKind::Service,
                key: "app".into(),
                resource_id: service,
            },
        )
        .unwrap();
    let inventory = with_inventory(&state, &stack, Ok).unwrap();
    let plan = firecrab_stack::plan(&stack, &record, &inventory).unwrap();
    let apply = Uuid::new_v4();
    let status:ApplyStatus=serde_json::from_value(serde_json::json!({"operationId":apply,"stackId":record.id,"name":stack.name,
        "state":"queued","inputs":plan.inputs,"changes":[{"kind":"service","key":"app","resourceId":service,"action":"deploy","fields":[],"state":"running"}],"error":null})).unwrap();
    let deployment:MicroServiceDeployment=serde_json::from_value(serde_json::json!({
        "id":crate::service_runtime::stack_deployment_id(apply,service),"serviceId":service,"spec":spec,
        "request":{"microNetworkId":Uuid::new_v4(),"hostPort":0},"phase":"queued","domainRouteOwned":false,"internalRouteOwned":false,
        "digest":null,"instances":[],"log":[],"error":null})).unwrap();
    (root, state, stack, status, deployment)
}

fn admit(state: &AppState, stack: &Stack, status: &ApplyStatus) {
    let record = state.store.stack(&stack.name).unwrap().unwrap();
    with_inventory(state, stack, |_| {
        state.store.begin_stack_apply(
            &record,
            &firecrab_stack::desired_hash(stack),
            false,
            status,
            None,
        )
    })
    .unwrap();
}

#[tokio::test]
async fn iac_stack_secret_original_inputs_survive_rotation_deletion_and_sqlite_reopen() {
    let (root, state, stack, status, mut deployment) = fixture().await;
    prepare(&state, &stack, &status).unwrap();
    // Private pre-admission data alone cannot authorize a deployment.
    assert!(
        preflight(
            &state,
            status.operation_id,
            deployment.service_id,
            deployment.id,
            &deployment.spec,
            Uuid::new_v4()
        )
        .await
        .is_err()
    );
    admit(&state, &stack, &status);
    state
        .secrets
        .write(|contents| {
            contents.secrets.clear();
            Ok(())
        })
        .unwrap();
    let mut reopened = state.clone();
    reopened.store = Store::open(&root.path().join("state.db")).unwrap();
    reopened.secrets = std::sync::Arc::new(Vault::new(root.path().join("state.secrets")));
    preflight(
        &reopened,
        status.operation_id,
        deployment.service_id,
        deployment.id,
        &deployment.spec,
        Uuid::new_v4(),
    )
    .await
    .unwrap();
    super::super::capture(&reopened, &deployment).await.unwrap();
    deployment.phase = ServicePhase::Queued;
    let mut vm = crate::handlers::vms::test_support::record("replica", Uuid::new_v4());
    deployment
        .instances
        .push(firecrab_api_types::ServiceInstance {
            vm_id: vm.id,
            ipv4: "127.0.0.1".into(),
            host_port: 0,
            state: firecrab_api_types::VmState::Created,
            healthy: false,
        });
    state.store.save_service_deployment(&deployment).unwrap();
    vm.env = deployment.spec.env.clone();
    assert_eq!(
        super::super::guest_env(&reopened, &vm).await.unwrap()["TOKEN"],
        "DO-NOT-PRINT-original-pinned-value"
    );
    super::super::capture(&reopened, &deployment).await.unwrap();
    let public = serde_json::to_string(&status).unwrap();
    assert!(!public.contains("DO-NOT-PRINT"));
    for path in [
        root.path().join("state.db"),
        root.path().join("state.secrets/vault.bin"),
    ] {
        let bytes = std::fs::read(path).unwrap();
        assert!(
            !bytes
                .windows(b"DO-NOT-PRINT".len())
                .any(|chunk| chunk == b"DO-NOT-PRINT")
        );
    }
    super::super::remove(&reopened, &deployment).await.unwrap();
    assert!(super::super::capture(&reopened, &deployment).await.is_err());
    reopened
        .secrets
        .read(|contents| {
            assert!(
                contents.stack_service_credentials[&deployment.id]
                    .values
                    .is_none()
            );
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn iac_stack_secret_scope_rejects_changed_spec_child_parent_and_retired_authority() {
    let (_root, state, stack, mut status, deployment) = fixture().await;
    prepare(&state, &stack, &status).unwrap();
    admit(&state, &stack, &status);
    let mut changed = deployment.clone();
    changed.spec.image = "ghcr.io/team/other:v2".into();
    assert!(super::super::capture(&state, &changed).await.is_err());
    changed = deployment.clone();
    changed.id = Uuid::new_v4();
    // A missing scope during an active Stack step must not fall back to current values.
    assert!(super::super::capture(&state, &changed).await.is_err());
    assert!(
        preflight(
            &state,
            Uuid::new_v4(),
            deployment.service_id,
            deployment.id,
            &deployment.spec,
            Uuid::new_v4()
        )
        .await
        .is_err()
    );
    status.state = ApplyState::Succeeded;
    status.changes[0].state = StepState::Succeeded;
    state.store.save_stack_apply(&status).unwrap();
    assert!(super::super::capture(&state, &deployment).await.is_err());
    state
        .secrets
        .read(|contents| {
            assert!(contents.service_environments.is_empty());
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn iac_stack_secret_prepare_rejects_rotated_inputs_and_preserves_original_private_snapshot() {
    let (_root, state, stack, status, deployment) = fixture().await;
    prepare(&state, &stack, &status).unwrap();
    state
        .secrets
        .write(|contents| {
            contents.secrets[0].summary.version += 1;
            contents.secrets[0].value = "DO-NOT-PRINT-rotated".into();
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        prepare(&state, &stack, &status),
        Err(PersistenceError::StaleStackPlan)
    ));
    state
        .secrets
        .read(|contents| {
            assert_eq!(
                contents.stack_service_credentials[&deployment.id]
                    .values
                    .as_ref()
                    .unwrap()["TOKEN"],
                "DO-NOT-PRINT-original-pinned-value"
            );
            assert!(contents.service_environments.is_empty());
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn iac_stack_secret_completed_parent_retry_needs_no_live_vault_or_current_references() {
    let (root, state, stack, mut status, _deployment) = fixture().await;
    let fingerprint = with_inventory(&state, &stack, |inventory| {
        let registration = state.store.stack(&stack.name)?.unwrap();
        Ok(firecrab_stack::fingerprint(
            &stack,
            &registration,
            &inventory,
        ))
    })
    .unwrap();
    prepare(&state, &stack, &status).unwrap();
    admit(&state, &stack, &status);
    status.state = ApplyState::Succeeded;
    status.changes[0].state = StepState::Succeeded;
    state.store.save_stack_apply(&status).unwrap();
    std::fs::write(root.path().join("state.secrets/vault.bin"), b"unavailable").unwrap();
    let original = crate::handlers::stack_applies::admit(
        state,
        firecrab_stack::ApplyRequest {
            stack,
            expected_fingerprint: fingerprint,
            operation_id: status.operation_id,
            allow_stop: false,
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(original).unwrap(),
        serde_json::to_value(status).unwrap()
    );
}

use super::*;
use firecrab_api_types::SandboxEnvironmentState;

#[tokio::test]
async fn iac_environment_transfer_checks_controller_work_and_keeps_child_authority() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let app = app.layer(axum::Extension(crate::ai_sandbox::access::LocalOperator));
    let mut environment = generated_environment(&state, Uuid::new_v4());
    let resource = ManagedResource {
        kind: "sandbox_environment".into(),
        id: environment.id.to_string(),
    };
    let change = claim_configuration(&state, &resource);
    let operation = Uuid::parse_str(change["operationId"].as_str().unwrap()).unwrap();
    let (status, error) = call(&app, "POST", "/api/iac/owners/transition", change.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "iac_resource_busy");
    assert!(
        state
            .store
            .iac_ownership_operation(operation)
            .unwrap()
            .is_none()
    );
    environment.state = SandboxEnvironmentState::Ready;
    state.store.save_sandbox_environment(&environment).unwrap();
    let vm = environment.roles[0].vm_id;
    let mut record = crate::handlers::vms::test_support::record("generated-worker", vm);
    record.state = crate::model::VmState::Starting;
    state.store.insert(&record).unwrap();
    assert_eq!(
        call(&app, "POST", "/api/iac/owners/transition", change.clone())
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert!(
        state
            .store
            .iac_ownership_operation(operation)
            .unwrap()
            .is_none()
    );
    record.state = crate::model::VmState::Stopped;
    state.store.update(&record).unwrap();
    let (status, owned) = call(&app, "POST", "/api/iac/owners/transition", change).await;
    assert_eq!(status, StatusCode::OK, "{owned}");
    assert_eq!(
        state
            .store
            .sandbox_environment(environment.id)
            .unwrap()
            .unwrap()
            .generation,
        environment.generation
    );
    let prefix = format!(
        "/api/ai-sandbox/projects/{}/environments/{}",
        environment.project_id,
        environment.id.to_string().replace('-', "%2d")
    );
    for (method, suffix) in [
        ("PUT", ""),
        ("DELETE", ""),
        ("POST", "/stop"),
        ("POST", "/resume"),
        ("POST", "/reconcile"),
    ] {
        let (status, error) = call(&app, method, &format!("{prefix}{suffix}"), json!({})).await;
        assert_eq!(status, StatusCode::CONFLICT, "{suffix}: {error}");
        assert_eq!(error["error"]["code"], "iac_resource_managed");
    }
    // Controller observations still checkpoint the immutable parent generation.
    environment.updated_at += 1;
    environment.error = Some("fixture observation".into());
    state.store.save_sandbox_environment(&environment).unwrap();
    environment.state = SandboxEnvironmentState::CleanupPending;
    state.store.save_sandbox_environment(&environment).unwrap();
    let release = json!({"operationId":Uuid::new_v4(),"expectedEndpointId":owned["endpointId"],
        "resource":resource,"expectedGeneration":1,"expectedOwner":owned["owner"],"owner":null});
    let (status, error) = call(&app, "POST", "/api/iac/owners/transition", release.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "iac_resource_busy");
    environment.state = SandboxEnvironmentState::Deleted;
    state.store.save_sandbox_environment(&environment).unwrap();
    let (status, released) = call(&app, "POST", "/api/iac/owners/transition", release).await;
    assert_eq!(status, StatusCode::OK, "{released}");
    assert_eq!(released["generation"], 2);
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/iac/owners/transition",
            claim_configuration(&state, &resource)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let child = ManagedResource {
        kind: "vm".into(),
        id: vm.to_string(),
    };
    let (status, error) = call(
        &app,
        "POST",
        "/api/iac/owners/transition",
        claim_configuration(&state, &child),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "iac_parent_managed");
}

#[tokio::test]
async fn iac_environment_transfer_waits_for_the_existing_controller_lock() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let app = app.layer(axum::Extension(crate::ai_sandbox::access::LocalOperator));
    let mut environment = generated_environment(&state, Uuid::new_v4());
    environment.state = SandboxEnvironmentState::Stopped;
    state.store.save_sandbox_environment(&environment).unwrap();
    let resource = ManagedResource {
        kind: "sandbox_environment".into(),
        id: environment.id.to_string(),
    };
    let change = claim_configuration(&state, &resource);
    let held = state.lock_vm(environment.id).await;
    let mut transfer =
        tokio::spawn(async move { call(&app, "POST", "/api/iac/owners/transition", change).await });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), &mut transfer)
            .await
            .is_err()
    );
    assert_eq!(
        state
            .store
            .iac_resource_owner(&resource)
            .unwrap()
            .generation,
        0
    );
    drop(held);
    assert_eq!(transfer.await.unwrap().0, StatusCode::OK);
}

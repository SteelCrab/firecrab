use super::*;
use crate::persistence::{PersistenceError, SourceTrigger};
use firecrab_stack::{ConfigureGitSource, DeleteGitSource, GitCheckState};

fn config(generation: u64) -> ConfigureGitSource {
    serde_json::from_value(json!({"expectedGeneration":generation,"source":{"repository":"https://github.com/example/infra","ref":"main","manifestPath":"stack.yaml","paused":true}})).unwrap()
}

fn removal(state: &AppState, stack: Uuid, generation: u64) -> DeleteGitSource {
    DeleteGitSource {
        operation_id: Uuid::new_v4(),
        expected_endpoint_id: state.store.iac_endpoint_id().unwrap(),
        expected_stack_id: stack,
        expected_generation: generation,
    }
}

fn finish(state: &AppState, source: &firecrab_stack::GitSourceStatus) {
    let mut check = source.latest.clone().unwrap();
    check.state = GitCheckState::Failed;
    check.finished_at = Some(check.started_at + 1);
    check.error = Some("fixture_no_network".into());
    state
        .store
        .finish_stack_check(source.stack_id, &check, check.started_at + 1)
        .unwrap();
}

#[tokio::test]
async fn iac_source_transfer_preserves_config_and_keeps_controller_observation_available() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let stack = state.store.register_stack("source-owner").unwrap();
    state
        .store
        .configure_stack_source(&stack.name, &config(0), 1)
        .unwrap();
    let resource = ManagedResource {
        kind: "stack_source".into(),
        id: stack.id.to_string(),
    };
    let active = state
        .store
        .begin_stack_check(&stack.name, 1, 1, SourceTrigger::Check)
        .unwrap()
        .unwrap()
        .0;
    let change = claim_configuration(&state, &resource);
    assert!(matches!(
        state
            .store
            .change_iac_resource_owner(&serde_json::from_value(change.clone()).unwrap()),
        Err(PersistenceError::StackSourceBusy)
    ));
    finish(&state, &active);
    let before = state.store.stack_source(&stack.name).unwrap();
    let (status, owned) = call(&app, "POST", "/api/iac/owners/transition", change).await;
    assert_eq!(status, StatusCode::OK, "{owned}");
    assert_eq!(state.store.stack_source(&stack.name).unwrap(), before);
    let (status, error) = call(
        &app,
        "PUT",
        "/api/stacks/source-owner/source",
        serde_json::to_value(config(1)).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "iac_ownership_conflict");
    let request = removal(&state, stack.id, 1);
    assert!(matches!(
        state.store.remove_stack_source(&stack.name, &request),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    assert!(
        state
            .store
            .iac_native_operation(request.operation_id)
            .unwrap()
            .is_none()
    );
    let active = state
        .store
        .begin_stack_check(&stack.name, 1, 3, SourceTrigger::Check)
        .unwrap()
        .unwrap()
        .0;
    finish(&state, &active);
    assert_eq!(state.store.stack_source(&stack.name).unwrap().generation, 1);
    assert_eq!(
        state.store.stack_check_history(&stack.name).unwrap().len(),
        2
    );
}

#[tokio::test]
async fn iac_source_removal_retries_preserve_recreated_config_bindings_and_history() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let app = app.layer(axum::Extension(crate::ai_sandbox::access::LocalOperator));
    let stack = state.store.register_stack("source-remove").unwrap();
    state
        .store
        .bind_stack_resource(
            &stack.name,
            &Binding {
                kind: ResourceKind::Network,
                key: "retained".into(),
                resource_id: network,
            },
        )
        .unwrap();
    assert_eq!(
        state
            .store
            .stack_source_generation(&stack.name)
            .unwrap()
            .generation,
        0
    );
    state
        .store
        .configure_stack_source(&stack.name, &config(0), 1)
        .unwrap();
    let active = state
        .store
        .begin_stack_check(&stack.name, 1, 1, SourceTrigger::Check)
        .unwrap()
        .unwrap()
        .0;
    let request = removal(&state, stack.id, 1);
    let body = serde_json::to_value(&request).unwrap();
    let path = "/api/stacks/source-remove/source";
    let (status, error) = call(&app, "DELETE", path, body.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "stack_source_busy");
    assert!(
        state
            .store
            .iac_native_operation(request.operation_id)
            .unwrap()
            .is_none()
    );
    finish(&state, &active);
    let (status, removed) = call(&app, "DELETE", path, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert_eq!(removed["state"], "succeeded");
    assert_eq!(removed["configurationGeneration"], 1);
    assert_eq!(
        call(&app, "GET", path, Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/stacks/source-remove/source/generation",
            Value::Null
        )
        .await
        .1["generation"],
        2
    );
    let history = call(
        &app,
        "GET",
        "/api/stacks/source-remove/source/history",
        Value::Null,
    )
    .await;
    assert_eq!(history.0, StatusCode::OK, "{history:?}");
    assert_eq!(history.1.as_array().unwrap().len(), 1);
    let reopened = crate::persistence::Store::open(&root.path().join("state.db")).unwrap();
    assert_eq!(
        reopened
            .stack_source_generation(&stack.name)
            .unwrap()
            .generation,
        2
    );
    assert!(matches!(
        reopened.configure_stack_source(&stack.name, &config(0), 3),
        Err(PersistenceError::StaleStackSource)
    ));
    assert_eq!(
        reopened
            .configure_stack_source(&stack.name, &config(2), 3)
            .unwrap()
            .generation,
        3
    );
    assert_eq!(call(&app, "DELETE", path, body.clone()).await.1, removed);
    assert_eq!(state.store.stack_source(&stack.name).unwrap().generation, 3);
    let stale = removal(&state, stack.id, 1);
    assert!(matches!(
        state.store.remove_stack_source(&stack.name, &stale),
        Err(PersistenceError::StaleStackSource)
    ));
    assert!(
        state
            .store
            .iac_native_operation(stale.operation_id)
            .unwrap()
            .is_none()
    );
    let mut changed = request;
    changed.expected_generation = 3;
    assert!(matches!(
        state.store.remove_stack_source(&stack.name, &changed),
        Err(PersistenceError::IacOperationIdentity)
    ));
    let retry = format!(
        "/api/iac/native-operations/{}/recover",
        changed.operation_id
    );
    assert_eq!(call(&app, "POST", &retry, json!({})).await.1, removed);
    assert_eq!(state.store.stack_source(&stack.name).unwrap().generation, 3);
    assert_eq!(
        state.store.stack(&stack.name).unwrap().unwrap().bindings[0].resource_id,
        network
    );
    assert!(state.store.micro_network(network).unwrap().is_some());
    assert_eq!(
        state.store.stack_check_history(&stack.name).unwrap().len(),
        1
    );
    state
        .store
        .remove_stack_source(&stack.name, &removal(&state, stack.id, 3))
        .unwrap();
    assert_eq!(
        state
            .store
            .stack_source_generation(&stack.name)
            .unwrap()
            .generation,
        4
    );
    assert_eq!(
        state
            .store
            .configure_stack_source(&stack.name, &config(4), 4)
            .unwrap()
            .generation,
        5
    );
}

#[tokio::test]
async fn iac_source_removal_validates_endpoint_registration_and_typed_operation_identity() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let stack = state.store.register_stack("source-scope").unwrap();
    state
        .store
        .configure_stack_source(&stack.name, &config(0), 1)
        .unwrap();
    let mut request = removal(&state, stack.id, 1);
    request.expected_endpoint_id = Uuid::new_v4();
    assert!(matches!(
        state.store.remove_stack_source(&stack.name, &request),
        Err(PersistenceError::IacEndpointConflict)
    ));
    request.expected_endpoint_id = state.store.iac_endpoint_id().unwrap();
    request.expected_stack_id = Uuid::new_v4();
    assert!(matches!(
        state.store.remove_stack_source(&stack.name, &request),
        Err(PersistenceError::IacOperationIdentity)
    ));
    request.expected_stack_id = stack.id;
    assert!(matches!(
        state.store.remove_stack_source("absent", &request),
        Err(PersistenceError::MissingStack)
    ));
    let project = configuration_resource(&state, root.path(), network, "sandbox_project");
    state
        .store
        .begin_sandbox_project_delete(Uuid::parse_str(&project.id).unwrap(), request.operation_id)
        .unwrap();
    assert!(matches!(
        state.store.remove_stack_source(&stack.name, &request),
        Err(PersistenceError::IacOperationIdentity)
    ));
    request.operation_id = Uuid::nil();
    assert!(matches!(
        state.store.remove_stack_source(&stack.name, &request),
        Err(PersistenceError::IacOperationIdentity)
    ));
    assert_eq!(state.store.stack_source(&stack.name).unwrap().generation, 1);
}

#[tokio::test]
async fn iac_source_remove_and_reconfigure_have_one_winner_across_sqlite_connections() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let stack = state.store.register_stack("source-race").unwrap();
    state
        .store
        .configure_stack_source(&stack.name, &config(0), 1)
        .unwrap();
    let request = removal(&state, stack.id, 1);
    let other = crate::persistence::Store::open(&root.path().join("state.db")).unwrap();
    let start = std::sync::Arc::new(std::sync::Barrier::new(2));
    let ready = start.clone();
    let delete = std::thread::spawn(move || {
        ready.wait();
        other.remove_stack_source("source-race", &request)
    });
    let store = state.store.clone();
    let configure = std::thread::spawn(move || {
        let mut updated = config(1);
        updated.source.paused = false;
        start.wait();
        store.configure_stack_source("source-race", &updated, 2)
    });
    let deleted = delete.join().unwrap();
    let configured = configure.join().unwrap();
    assert_ne!(
        deleted.is_ok(),
        configured.is_ok(),
        "exactly one mutation may consume generation 1"
    );
    assert_eq!(
        state
            .store
            .stack_source_generation(&stack.name)
            .unwrap()
            .generation,
        2
    );
}

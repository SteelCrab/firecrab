use super::*;
use crate::persistence::PersistenceError;
use firecrab_api_types::{SandboxPrincipal, SandboxProject};
use firecrab_stack::{NativeOperation, NativeOperationState};

fn project(state: &AppState) -> SandboxProject {
    let project = SandboxProject {
        id: Uuid::new_v4(),
        owner: SandboxPrincipal::Local,
        created_at: 1,
        spec: serde_json::from_value(
            json!({"name":"IaC recovery fixture","sourcePath":null,"vmId":null,"selection":null}),
        )
        .unwrap(),
    };
    state.store.save_sandbox_project(&project).unwrap();
    project
}

fn resource(project: &SandboxProject) -> ManagedResource {
    ManagedResource {
        kind: "sandbox_project".into(),
        id: project.id.to_string(),
    }
}

fn task(project: &SandboxProject) -> crate::ai_sandbox::SavedTask {
    serde_json::from_value(
        json!({"project":project.spec,"workspace":null,"guest":null,"guest_pending":false,
        "task":{"id":Uuid::new_v4(),"projectId":project.id,"attemptId":Uuid::new_v4(),
            "request":{"kind":"chat","input":"retained conversation"},
            "selection":{"connectionId":Uuid::new_v4(),"model":"fixture"},"state":"result",
            "createdAt":1,"updatedAt":1,"result":"retained result","events":[],"changes":[]}}),
    )
    .unwrap()
}

#[tokio::test]
async fn iac_native_project_fence_blocks_new_work_but_preserves_existing_observations() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let project = project(&state);
    let mut existing = task(&project);
    existing.task.state = firecrab_api_types::SandboxTaskState::InProgress;
    state.store.save_sandbox_task(&existing).unwrap();
    let id = Uuid::new_v4();
    assert!(matches!(
        state.store.begin_sandbox_project_delete(project.id, id),
        Err(PersistenceError::IacResourceBusy)
    ));
    assert!(state.store.iac_native_operation(id).unwrap().is_none());
    let change: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &resource(&project))).unwrap();
    assert!(matches!(
        state.store.change_iac_resource_owner(&change),
        Err(PersistenceError::IacResourceBusy)
    ));
    existing.task.state = firecrab_api_types::SandboxTaskState::Result;
    state.store.save_sandbox_task(&existing).unwrap();
    state
        .store
        .begin_sandbox_project_delete(project.id, id)
        .unwrap();
    let new = task(&project);
    assert!(matches!(
        state.store.save_sandbox_task(&new),
        Err(PersistenceError::IacResourceBusy)
    ));
    assert!(state.store.sandbox_task(new.task.id).unwrap().is_none());
    existing.task.updated_at += 1;
    state.store.save_sandbox_task(&existing).unwrap();
    assert!(
        state
            .store
            .sandbox_task(existing.task.id)
            .unwrap()
            .is_some()
    );
    existing.task.state = firecrab_api_types::SandboxTaskState::InProgress;
    assert!(matches!(
        state.store.save_sandbox_task(&existing),
        Err(PersistenceError::IacResourceBusy)
    ));
    existing.task.state = firecrab_api_types::SandboxTaskState::Result;
    let mut environment = generated_environment(&state, Uuid::new_v4());
    environment.project_id = project.id;
    assert!(matches!(
        state.store.reserve_sandbox_environment(&environment),
        Err(PersistenceError::IacResourceBusy)
    ));
    state.store.fail_native_project_delete(id).unwrap();
    assert!(matches!(
        state.store.save_sandbox_task(&new),
        Err(PersistenceError::IacResourceBusy)
    ));
    state.store.recover_native_project_delete(id).unwrap();
    state.store.finish_native_project_delete(id).unwrap();
    assert!(
        state
            .store
            .sandbox_task(existing.task.id)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        state.store.save_sandbox_task(&new),
        Err(PersistenceError::IacOperationIdentity)
    ));
    assert!(matches!(
        state.store.reserve_sandbox_environment(&environment),
        Err(PersistenceError::IacOperationIdentity)
    ));
}

#[tokio::test]
async fn iac_project_transfer_fences_encoded_http_paths_and_native_configuration_commits() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let app = app.layer(axum::Extension(crate::ai_sandbox::access::LocalOperator));
    let project = project(&state);
    let resource = resource(&project);
    let (status, owned) = call(
        &app,
        "POST",
        "/api/iac/owners/transition",
        claim_configuration(&state, &resource),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{owned}");
    let url = format!(
        "/api/ai-sandbox/projects/{}",
        project.id.to_string().replace('-', "%2d")
    );
    for method in ["PUT", "DELETE"] {
        let (status, error) = call(&app, method, &url, json!({})).await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["error"]["code"], "iac_resource_managed");
    }
    assert!(matches!(
        state.store.save_sandbox_project(&project),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    assert!(matches!(
        state
            .store
            .begin_sandbox_project_delete(project.id, Uuid::new_v4()),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    assert!(state.store.sandbox_project(project.id).unwrap().is_some());
}

#[tokio::test]
async fn iac_native_journal_reserves_original_identity_until_recovery_finishes() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let project = project(&state);
    let resource = resource(&project);
    let id = Uuid::new_v4();
    let original = state
        .store
        .begin_sandbox_project_delete(project.id, id)
        .unwrap();
    assert_eq!(original.state, NativeOperationState::Running);
    assert_eq!(
        state
            .store
            .begin_sandbox_project_delete(project.id, id)
            .unwrap(),
        original
    );
    assert!(matches!(
        state.store.begin_sandbox_project_delete(Uuid::new_v4(), id),
        Err(PersistenceError::IacOperationIdentity)
    ));
    assert!(matches!(
        state
            .store
            .begin_sandbox_project_delete(project.id, Uuid::new_v4()),
        Err(PersistenceError::IacResourceBusy)
    ));
    assert!(matches!(
        state.store.save_sandbox_project(&project),
        Err(PersistenceError::IacResourceBusy)
    ));
    let change: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &resource)).unwrap();
    assert!(matches!(
        state.store.change_iac_resource_owner(&change),
        Err(PersistenceError::IacResourceBusy)
    ));
    assert!(matches!(
        state.store.recover_native_project_delete(id),
        Err(PersistenceError::IacResourceBusy)
    ));

    // A second SQLite connection does not interrupt a live worker merely by opening.
    let reopened = crate::persistence::Store::open(&root.path().join("state.db")).unwrap();
    assert_eq!(
        reopened.iac_native_operation(id).unwrap(),
        Some(original.clone())
    );
    reopened.interrupt_native_iac_operations().unwrap();
    let interrupted = reopened.iac_native_operation(id).unwrap().unwrap();
    assert_eq!(interrupted.state, NativeOperationState::NeedsRecovery);
    assert_eq!(interrupted.error_code.as_deref(), Some("server_restarted"));
    assert!(matches!(
        reopened.change_iac_resource_owner(&change),
        Err(PersistenceError::IacResourceBusy)
    ));
    let recovered = reopened.recover_native_project_delete(id).unwrap();
    assert_eq!(recovered.operation_id, id);
    assert_eq!(recovered.ownership, original.ownership);
    let completed = reopened.finish_native_project_delete(id).unwrap();
    assert_eq!(completed.state, NativeOperationState::Succeeded);
    assert!(reopened.sandbox_project(project.id).unwrap().is_none());
    assert!(matches!(
        reopened.save_sandbox_project(&project),
        Err(PersistenceError::IacOperationIdentity)
    ));
    reopened.fail_native_project_delete(id).unwrap();
    reopened.interrupt_native_iac_operations().unwrap();
    assert_eq!(
        reopened.finish_native_project_delete(id).unwrap(),
        completed
    );
    assert_eq!(
        reopened.recover_native_project_delete(id).unwrap(),
        completed
    );
    assert_eq!(
        reopened.iac_native_operations(&resource).unwrap(),
        vec![completed]
    );
    assert!(matches!(
        reopened.fail_native_project_delete(Uuid::new_v4()),
        Err(PersistenceError::IacOperationIdentity)
    ));
    assert!(
        reopened
            .begin_sandbox_project_delete(Uuid::nil(), Uuid::new_v4())
            .is_err()
    );
}

#[tokio::test]
async fn iac_native_failed_cleanup_is_discoverable_and_recovers_without_touching_borrowed_data() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let unauthenticated = app.clone();
    let app = app.layer(axum::Extension(crate::ai_sandbox::access::LocalOperator));
    let mut project = project(&state);
    let borrowed = root.path().join("borrowed-private-source");
    std::fs::create_dir(&borrowed).unwrap();
    std::fs::write(borrowed.join("retain.txt"), "borrowed contents").unwrap();
    project.spec.source_path = Some(borrowed.to_string_lossy().into());
    project.spec.instructions = "private prompt absent from status".into();
    state.store.save_sandbox_project(&project).unwrap();
    std::fs::create_dir_all(&state.sandbox.root).unwrap();
    let owned = state.sandbox.root.join(project.id.to_string());
    // A file in place of the private directory produces a real filesystem failure.
    std::fs::write(&owned, "private workspace needs repair").unwrap();
    let other = state.sandbox.root.join(Uuid::new_v4().to_string());
    std::fs::create_dir(&other).unwrap();
    std::fs::write(other.join("retain.txt"), "other project").unwrap();
    let (status, error) = call(
        &app,
        "DELETE",
        &format!("/api/ai-sandbox/projects/{}", project.id),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{error}");
    let url = format!(
        "/api/iac/native-operations/resources/sandbox_project/{}",
        project.id
    );
    let (status, operations) = call(&app, "GET", &url, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{operations}");
    let operation: NativeOperation = serde_json::from_value(operations[0].clone()).unwrap();
    assert_eq!(operation.state, NativeOperationState::NeedsRecovery);
    assert_eq!(
        operation.error_code.as_deref(),
        Some("project_cleanup_incomplete")
    );
    let status_url = format!("/api/iac/native-operations/{}", operation.operation_id);
    assert_eq!(
        call(&app, "GET", &status_url, Value::Null).await.1,
        operations[0]
    );
    assert!(!operations.to_string().contains("private"));
    assert!(
        !operations
            .to_string()
            .contains(&borrowed.to_string_lossy().to_string())
    );
    let (status, error) = call(
        &app,
        "POST",
        "/api/iac/owners/transition",
        claim_configuration(&state, &resource(&project)),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "iac_resource_busy");
    let recover_url = format!("{status_url}/recover");
    assert_eq!(
        call(&unauthenticated, "POST", &recover_url, json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    std::fs::remove_file(&owned).unwrap();
    std::fs::create_dir(&owned).unwrap();
    std::fs::write(owned.join("private.txt"), "remove only this file").unwrap();
    let (status, completed) = call(&app, "POST", &recover_url, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(completed["operationId"], operation.operation_id.to_string());
    assert_eq!(completed["ownership"], operations[0]["ownership"]);
    assert_eq!(completed["state"], "succeeded");
    assert_eq!(
        call(&app, "POST", &recover_url, json!({})).await.1,
        completed
    );
    assert!(!owned.exists());
    assert!(state.store.sandbox_project(project.id).unwrap().is_none());
    assert_eq!(
        std::fs::read_to_string(borrowed.join("retain.txt")).unwrap(),
        "borrowed contents"
    );
    assert_eq!(
        std::fs::read_to_string(other.join("retain.txt")).unwrap(),
        "other project"
    );
    let missing = format!("/api/iac/native-operations/{}", Uuid::new_v4());
    assert_eq!(
        call(&app, "GET", &missing, Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, "POST", &format!("{missing}/recover"), json!({}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn iac_native_worker_survives_its_http_waiter_being_dropped() {
    use std::{future::Future, task::Poll};
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let project = project(&state);
    let id = Uuid::new_v4();
    let held = state.sandbox.admission.lock().await;
    let mut waiter = Box::pin(crate::handlers::ai_sandbox::delete_project(
        State(state.clone()),
        Extension(RequestId(id)),
        Default::default(),
        Path(project.id),
    ));
    // Poll the handler through spawn, with its server worker queued behind admission.
    assert!(
        std::future::poll_fn(|cx| Poll::Ready(matches!(waiter.as_mut().poll(cx), Poll::Pending)))
            .await
    );
    drop(waiter);
    drop(held);
    let completed = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(operation) = state.store.iac_native_operation(id).unwrap()
                && operation.state == NativeOperationState::Succeeded
            {
                break operation;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached worker must persist completion");
    assert_eq!(completed.ownership.resource, resource(&project));
    assert!(state.store.sandbox_project(project.id).unwrap().is_none());
}

#[tokio::test]
async fn iac_native_cleanup_unlinks_a_workspace_symlink_without_following_it() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let app = app.layer(axum::Extension(crate::ai_sandbox::access::LocalOperator));
    let project = project(&state);
    let borrowed = root.path().join("borrowed");
    std::fs::create_dir(&borrowed).unwrap();
    std::fs::write(borrowed.join("retain.txt"), "borrowed contents").unwrap();
    std::fs::create_dir_all(&state.sandbox.root).unwrap();
    let owned = state.sandbox.root.join(project.id.to_string());
    std::os::unix::fs::symlink(&borrowed, &owned).unwrap();
    let (status, body) = call(
        &app,
        "DELETE",
        &format!("/api/ai-sandbox/projects/{}", project.id),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["deleted"], true);
    assert!(std::fs::symlink_metadata(owned).is_err());
    assert_eq!(
        std::fs::read_to_string(borrowed.join("retain.txt")).unwrap(),
        "borrowed contents"
    );
}

#[tokio::test]
async fn iac_native_recovery_rejects_changed_authority_before_filesystem_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let project = project(&state);
    let operation = state
        .store
        .begin_sandbox_project_delete(project.id, Uuid::new_v4())
        .unwrap();
    state.store.interrupt_native_iac_operations().unwrap();
    // Simulate an administrator replacing the endpoint identity outside the API.
    let connection = rusqlite::Connection::open(root.path().join("state.db")).unwrap();
    connection
        .execute(
            "UPDATE iac_endpoint SET endpoint_id=?1",
            [Uuid::new_v4().to_string()],
        )
        .unwrap();
    assert!(matches!(
        state
            .store
            .recover_native_project_delete(operation.operation_id),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert_eq!(
        state
            .store
            .iac_native_operation(operation.operation_id)
            .unwrap()
            .unwrap()
            .state,
        NativeOperationState::NeedsRecovery
    );
    assert!(state.store.sandbox_project(project.id).unwrap().is_some());
}

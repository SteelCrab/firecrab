use super::*;
use crate::{
    persistence::{
        PersistenceError,
        iac_database_operations::{Admission, DatabaseIntent},
    },
    pipe_databases::{
        DatabaseInput,
        maintenance::{Job, Kind},
    },
};
use firecrab_stack::NativeOperationState;

fn input() -> DatabaseInput {
    serde_json::from_value(json!({"name":"QA","provider":"local","uri":format!("postgresql://qa:{}@127.0.0.1:9/db",Uuid::new_v4()),"tlsMode":"disable","writeEnabled":true})).unwrap()
}
fn ready(admission: Admission, dispatch: bool) -> DatabaseIntent {
    let Admission::Ready(intent, actual) = admission else {
        panic!("expected durable admission")
    };
    assert_eq!(actual, dispatch);
    *intent
}

#[tokio::test]
async fn iac_database_connection_receipt_is_immutable_after_later_updates_and_transfer() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let raw = input();
    let op = Uuid::new_v4();
    let original = ready(
        state
            .store
            .begin_database_connection(None, op, raw.clone())
            .unwrap(),
        true,
    );
    state
        .store
        .begin_database_connection_effects(&original)
        .unwrap();
    state.store.finish_database_connection(&original).unwrap();
    let mut next = raw.clone();
    next.name = "later".into();
    let edit = ready(
        state
            .store
            .begin_database_connection(Some(original.id()), Uuid::new_v4(), next.clone())
            .unwrap(),
        true,
    );
    state.store.finish_database_connection(&edit).unwrap();
    let claim: ChangeResourceOwner = serde_json::from_value(claim_configuration(
        &state,
        &original.operation.ownership.resource,
    ))
    .unwrap();
    state.store.change_iac_resource_owner(&claim).unwrap();
    let retry = ready(
        state
            .store
            .begin_database_connection(None, op, raw.clone())
            .unwrap(),
        false,
    );
    assert_eq!(retry.id(), original.id());
    assert_eq!(retry.input().name, raw.name);
    assert_eq!(retry.operation.state, NativeOperationState::Succeeded);
    assert!(matches!(
        state.store.fail_database_connection(&retry),
        Err(PersistenceError::IacOperationIdentity)
    ));
    assert_eq!(
        state.store.iac_native_operation(op).unwrap().unwrap().state,
        NativeOperationState::Succeeded
    );
    assert_eq!(
        state
            .store
            .pipe_database(original.id())
            .unwrap()
            .unwrap()
            .input
            .name,
        next.name
    );
    assert!(matches!(
        state.store.begin_database_connection(None, op, next),
        Err(PersistenceError::IacOperationIdentity)
    ));
    let public = serde_json::to_string(&retry.operation).unwrap();
    assert!(!public.contains(&raw.uri));
}

#[tokio::test]
async fn iac_database_connection_unknown_outcome_keeps_target_alias_and_owner_fences_until_recovery()
 {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let resource = configuration_resource(&state, root.path(), network, "database_connection");
    let id = Uuid::parse_str(&resource.id).unwrap();
    let record = state.store.pipe_database(id).unwrap().unwrap();
    let mut alias = record.clone();
    alias.id = Uuid::new_v4();
    state
        .store
        .save_pipe_database(alias.id, &alias.input)
        .unwrap();
    let old = ready(
        state
            .store
            .begin_database_connection(Some(id), Uuid::new_v4(), record.input.clone())
            .unwrap(),
        true,
    );
    state.store.begin_database_connection_effects(&old).unwrap();
    state.store.fail_database_connection(&old).unwrap();
    let second = crate::persistence::Store::open(&root.path().join("state.db")).unwrap();
    assert!(matches!(
        second.begin_database_connection(Some(alias.id), Uuid::new_v4(), alias.input.clone()),
        Err(PersistenceError::IacResourceBusy)
    ));
    let job = Job::new(&alias, Uuid::new_v4(), Kind::Backup).unwrap();
    assert!(matches!(
        second.save_database_job(&job),
        Err(PersistenceError::IacResourceBusy)
    ));
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &resource)).unwrap();
    assert!(matches!(
        second.change_iac_resource_owner(&claim),
        Err(PersistenceError::IacResourceBusy)
    ));
    let (recovered, dispatch) = second
        .recover_database_connection(old.operation.operation_id)
        .unwrap();
    assert!(dispatch);
    assert_eq!(recovered.id(), old.id());
    assert_eq!(recovered.operation.execution_generation, 1);
    assert!(matches!(
        state.store.finish_database_connection(&old),
        Err(PersistenceError::IacOperationIdentity)
    ));
    assert!(matches!(
        state.store.fail_database_connection(&old),
        Err(PersistenceError::IacOperationIdentity)
    ));
    second.finish_database_connection(&recovered).unwrap();
    second.save_database_job(&job).unwrap();
    assert_eq!(
        state
            .store
            .iac_native_operation(old.operation.operation_id)
            .unwrap()
            .unwrap()
            .state,
        NativeOperationState::Succeeded
    );
}

#[tokio::test]
async fn iac_database_connection_verification_failure_releases_only_its_target_fence() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let raw = input();
    let original = ready(
        state
            .store
            .begin_database_connection(None, Uuid::new_v4(), raw.clone())
            .unwrap(),
        true,
    );
    state.store.fail_database_connection(&original).unwrap();
    assert!(state.store.pipe_database(original.id()).unwrap().is_none());
    let (failed, dispatch) = state
        .store
        .recover_database_connection(original.operation.operation_id)
        .unwrap();
    assert!(!dispatch);
    assert_eq!(failed.operation.state, NativeOperationState::Failed);
    let corrected = ready(
        state
            .store
            .begin_database_connection(None, Uuid::new_v4(), raw)
            .unwrap(),
        true,
    );
    assert_ne!(original.id(), corrected.id());
    assert!(matches!(
        state
            .store
            .begin_database_connection(None, Uuid::new_v4(), corrected.input().clone()),
        Err(PersistenceError::IacResourceBusy)
    ));
}

#[tokio::test]
async fn iac_database_connection_restart_retains_original_identity_and_rejects_old_completion() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let original = ready(
        state
            .store
            .begin_database_connection(None, Uuid::new_v4(), input())
            .unwrap(),
        true,
    );
    state
        .store
        .begin_database_connection_effects(&original)
        .unwrap();
    state.store.interrupt_native_iac_operations().unwrap();
    let second = crate::persistence::Store::open(&root.path().join("state.db")).unwrap();
    let (recovery, dispatch) = second
        .recover_database_connection(original.operation.operation_id)
        .unwrap();
    assert!(dispatch);
    assert_eq!(recovery.id(), original.id());
    assert_eq!(recovery.input().uri, original.input().uri);
    assert!(matches!(
        state.store.begin_database_connection_effects(&original),
        Err(PersistenceError::IacOperationIdentity)
    ));
    second.finish_database_connection(&recovery).unwrap();
    assert_eq!(second.list_pipe_databases().unwrap().len(), 1);
}

#[tokio::test]
async fn iac_database_connection_http_disconnect_does_not_cancel_the_admitted_worker() {
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut raw = input();
    raw.uri = format!(
        "postgresql://qa:private-fixture-password@{}/db",
        listener.local_addr().unwrap()
    );
    let operation = Uuid::new_v4();
    let request = Request::builder()
        .method("POST")
        .uri("/api/pipe/databases")
        .header("content-type", "application/json")
        .header("x-firecrab-operation-id", operation.to_string())
        .body(Body::from(serde_json::to_vec(&raw).unwrap()))
        .unwrap();
    let http = tokio::spawn(app.oneshot(request));
    let (connection, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        state
            .store
            .iac_native_operation(operation)
            .unwrap()
            .unwrap()
            .state,
        NativeOperationState::Running
    );
    http.abort();
    assert!(http.await.unwrap_err().is_cancelled());
    drop(connection);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if state
                .store
                .iac_native_operation(operation)
                .unwrap()
                .unwrap()
                .state
                == NativeOperationState::Failed
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let retry = ready(
        state
            .store
            .begin_database_connection(None, operation, raw)
            .unwrap(),
        false,
    );
    assert!(state.store.pipe_database(retry.id()).unwrap().is_none());
}

#[tokio::test]
async fn iac_database_access_retry_reads_original_receipt_before_current_policy_and_owner() {
    use crate::persistence::iac_database_operations::DatabaseChange;
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let resource = configuration_resource(&state, root.path(), network, "database_connection");
    let id = Uuid::parse_str(&resource.id).unwrap();
    let operation = Uuid::new_v4();
    let request = || {
        Request::builder()
            .method("PUT")
            .uri(format!("/api/pipe/databases/{id}/access"))
            .header("content-type", "application/json")
            .header("x-firecrab-operation-id", operation.to_string())
            .body(Body::from(r#"{"writeEnabled":false}"#))
            .unwrap()
    };
    let response = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-firecrab-operation-id"],
        operation.to_string()
    );
    let body = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let original: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(original["writeEnabled"], false);
    let mut later = state.store.pipe_database(id).unwrap().unwrap().input;
    later.name = "newer settings".into();
    let edit = ready(
        state
            .store
            .begin_database_change(Uuid::new_v4(), DatabaseChange::Update(id, later))
            .unwrap(),
        true,
    );
    state.store.finish_database_connection(&edit).unwrap();
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &resource)).unwrap();
    state.store.change_iac_resource_owner(&claim).unwrap();
    let retry = app.oneshot(request()).await.unwrap();
    assert_eq!(retry.status(), StatusCode::OK);
    let body = axum::body::to_bytes(retry.into_body(), 65536)
        .await
        .unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), original);
    assert!(matches!(
        state.store.delete_pipe_database(id),
        Err(PersistenceError::IacOwnershipConflict)
    ));
}

#[tokio::test]
async fn iac_database_unknown_update_blocks_alias_deletion_and_old_maintenance_entry_points() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let resource = configuration_resource(&state, root.path(), network, "database_connection");
    let id = Uuid::parse_str(&resource.id).unwrap();
    let record = state.store.pipe_database(id).unwrap().unwrap();
    let alias = Uuid::new_v4();
    state
        .store
        .save_pipe_database(alias, &record.input)
        .unwrap();
    let operation = ready(
        state
            .store
            .begin_database_connection(Some(id), Uuid::new_v4(), record.input.clone())
            .unwrap(),
        true,
    );
    state
        .store
        .begin_database_connection_effects(&operation)
        .unwrap();
    state.store.fail_database_connection(&operation).unwrap();
    assert!(matches!(
        state.store.delete_pipe_database(alias),
        Err(PersistenceError::IacResourceBusy)
    ));
    assert!(state.store.database_maintenance_active(alias).unwrap());
    assert!(state.store.pipe_database(alias).unwrap().is_some());
}

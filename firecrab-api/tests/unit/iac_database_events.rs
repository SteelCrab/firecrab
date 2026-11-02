use super::*;
use crate::{
    persistence::{
        PersistenceError,
        iac_database_events::{Admission, EventChange, EventIntent},
    },
    pipe_databases::{
        DatabaseRecord,
        bindings::{Access, BindingInput, BindingRecord},
        events::{RabbitMQInput, Trigger, TriggerInput},
        maintenance::{Job, Kind},
        write::TableRequest,
    },
};
use firecrab_stack::NativeOperationState;

fn setup(
    state: &AppState,
    root: &std::path::Path,
    network: Uuid,
) -> (DatabaseRecord, Trigger, RabbitMQInput) {
    let resource = configuration_resource(state, root, network, "database_connection");
    let id = Uuid::parse_str(&resource.id).unwrap();
    let mut record = state.store.pipe_database(id).unwrap().unwrap();
    record.input.write_enabled = true;
    state.store.update_pipe_database(id, &record.input).unwrap();
    let service = Uuid::new_v4();
    state.store.insert_micro_service(&serde_json::from_value(json!({"id":service,"name":format!("event-{}",service.simple()),"image":"ghcr.io/qa/app:1","port":8080,"cpu":1,"ram":512})).unwrap()).unwrap();
    let id = Uuid::new_v4();
    let trigger = Trigger {
        id,
        database_id: record.id,
        input: TriggerInput {
            request_id: id,
            schema: "public".into(),
            table: "orders".into(),
            service_id: service,
            path: "/events/database".into(),
        },
        paused: false,
        installed: true,
        detached: false,
    };
    let broker:RabbitMQInput=serde_json::from_value(json!({"brokers":["127.0.0.1:9"],"tls":false,"username":"fixture","password":Uuid::new_v4().to_string(),"development":true})).unwrap();
    state.store.save_database_rabbitmq(&broker).unwrap();
    (record, trigger, broker)
}
fn ready(admission: Admission, dispatch: bool) -> EventIntent {
    let Admission::Ready(intent, actual) = admission else {
        panic!("expected native event admission")
    };
    assert_eq!(actual, dispatch);
    *intent
}
fn create(state: &AppState, value: &Trigger) -> EventIntent {
    ready(
        state
            .store
            .begin_database_events(
                Uuid::new_v4(),
                EventChange::Create {
                    database: value.database_id,
                    input: value.input.clone(),
                },
            )
            .unwrap(),
        true,
    )
}
fn resource(value: &Trigger) -> ManagedResource {
    ManagedResource {
        kind: "database_events".into(),
        id: format!("{}:{}", value.database_id, value.id),
    }
}

#[tokio::test]
#[ignore = "manual local event recovery screenshot fixture; no SQL or broker runs"]
async fn iac_database_events_browser_fixture() {
    let info = std::path::PathBuf::from(std::env::var_os("FIRECRAB_IAC_QA_INFO").unwrap());
    let assets = std::path::PathBuf::from(std::env::var_os("FIRECRAB_IAC_QA_ASSETS").unwrap());
    assert!(assets.join("index.html").is_file());
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let (mut record, trigger, _) = setup(&state, root.path(), network);
    let database = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    record.input.uri = format!(
        "postgresql://fixture:private-event-fixture@{}/iac_events",
        database.local_addr().unwrap()
    );
    state
        .store
        .update_pipe_database(record.id, &record.input)
        .unwrap();
    // Only the local capture record and an uncertain checkpoint are seeded.
    state.store.save_database_trigger(&trigger).unwrap();
    let operation_id = Uuid::new_v4();
    let intent = ready(
        state
            .store
            .begin_database_events(operation_id, EventChange::Detach { id: trigger.id })
            .unwrap(),
        true,
    );
    state.store.prepare_database_events(&intent).unwrap();
    state.store.checkpoint_database_events(&intent).unwrap();
    state.store.fail_database_events(&intent).unwrap();
    let router = build_router(
        state,
        &HttpConfig {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 128,
            request_timeout: std::time::Duration::from_secs(10),
            static_root: Some(assets),
        },
    )
    .layer(axum::Extension(crate::ai_sandbox::access::LocalOperator));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let finished = std::sync::Arc::new(tokio::sync::Notify::new());
    let shutdown = finished.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { shutdown.notified().await })
            .await
            .unwrap();
    });
    std::fs::write(&info, serde_json::to_vec_pretty(&json!({"fixture":true,"url":url,"operationId":operation_id,
        "databaseId":record.id,"triggerId":trigger.id,"scope":"seeded local capture record and injected uncertain checkpoint; nonresponsive fixture-owned TCP endpoint; no SQL or RabbitMQ execution"})).unwrap()).unwrap();
    let stop = info.with_extension("stop");
    let started = std::time::Instant::now();
    while !stop.exists() {
        assert!(started.elapsed() < std::time::Duration::from_secs(600));
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    finished.notify_one();
    server.await.unwrap();
    std::fs::remove_file(info).unwrap();
    std::fs::remove_file(stop).unwrap();
}

#[tokio::test]
async fn iac_database_events_http_disconnect_keeps_original_intent_and_cleans_unverified_staging() {
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let (mut record, value, _) = setup(&state, root.path(), network);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    record.input.uri = format!(
        "postgresql://fixture:private-event-root@{}/db",
        listener.local_addr().unwrap()
    );
    state
        .store
        .update_pipe_database(record.id, &record.input)
        .unwrap();
    let request = || {
        Request::builder()
            .method("POST")
            .uri(format!("/api/pipe/databases/{}/triggers", record.id))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&value.input).unwrap()))
            .unwrap()
    };
    let http = tokio::spawn(app.clone().oneshot(request()));
    let (connection, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        state
            .store
            .iac_native_operation(value.id)
            .unwrap()
            .unwrap()
            .state,
        NativeOperationState::Running
    );
    assert!(
        state
            .store
            .database_trigger(value.id)
            .unwrap()
            .unwrap()
            .paused
    );
    http.abort();
    assert!(http.await.unwrap_err().is_cancelled());
    drop(connection);
    tokio::time::timeout(Duration::from_secs(3), async {
        while state
            .store
            .iac_native_operation(value.id)
            .unwrap()
            .unwrap()
            .state
            != NativeOperationState::Failed
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(state.store.database_trigger(value.id).unwrap().is_none());
    assert!(state.store.database_bindings().unwrap().is_empty());
    let response = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        response.headers()["x-firecrab-operation-id"],
        value.id.to_string()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(60), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn iac_database_events_broker_verification_cannot_overwrite_a_later_configuration() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let (_, _, original) = setup(&state, root.path(), network);
    let stale = state.store.database_broker_authority().unwrap();
    let current = state.store.database_broker_authority().unwrap();
    let mut changed = original.clone();
    changed.username = "new-verification".into();
    state
        .store
        .configure_external_database_broker(&changed, &current)
        .unwrap();
    assert!(matches!(
        state
            .store
            .configure_external_database_broker(&original, &stale),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert_eq!(
        state.store.database_rabbitmq().unwrap().unwrap().username,
        changed.username
    );
}

#[tokio::test]
async fn iac_database_events_qualified_transfer_preserves_definition_and_cached_pause_receipt() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let (_, mut value, _) = setup(&state, root.path(), network);
    state.store.save_database_trigger(&value).unwrap();
    let operation = Uuid::new_v4();
    let request = || {
        Request::builder()
            .method("PUT")
            .uri(format!("/api/pipe/database-triggers/{}", value.id))
            .header("content-type", "application/json")
            .header("x-firecrab-operation-id", operation.to_string())
            .body(Body::from(r#"{"paused":true}"#))
            .unwrap()
    };
    let response = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let original = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    value.paused = true;
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &resource(&value))).unwrap();
    state.store.change_iac_resource_owner(&claim).unwrap();
    let retry = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(retry.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(retry.into_body(), 65536)
            .await
            .unwrap(),
        original
    );
    let (status, error) = call(
        &app,
        "PUT",
        &format!("/api/pipe/database-triggers/{}", value.id),
        json!({"paused":false}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "iac_ownership_conflict");
    assert!(
        state
            .store
            .database_trigger(value.id)
            .unwrap()
            .unwrap()
            .paused
    );
    let wrong = ManagedResource {
        kind: "database_events".into(),
        id: format!("{}:{}", Uuid::new_v4(), value.id),
    };
    let request: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &wrong)).unwrap();
    assert!(matches!(
        state.store.change_iac_resource_owner(&request),
        Err(PersistenceError::InvalidStackResource)
    ));
}

#[tokio::test]
async fn iac_database_events_no_effect_failure_restores_preparation_and_releases_broker_target() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let (record, value, broker) = setup(&state, root.path(), network);
    let authority = state.store.database_broker_authority().unwrap();
    let intent = create(&state, &value);
    state.store.prepare_database_events(&intent).unwrap();
    assert!(
        state
            .store
            .database_trigger(value.id)
            .unwrap()
            .unwrap()
            .paused
    );
    assert!(matches!(
        state
            .store
            .configure_external_database_broker(&broker, &authority),
        Err(PersistenceError::IacResourceBusy)
    ));
    state.store.fail_database_events(&intent).unwrap();
    assert!(state.store.database_trigger(value.id).unwrap().is_none());
    assert_eq!(
        state
            .store
            .iac_native_operation(intent.operation.operation_id)
            .unwrap()
            .unwrap()
            .state,
        NativeOperationState::Failed
    );
    let (_, dispatch) = state
        .store
        .recover_database_events(intent.operation.operation_id)
        .unwrap();
    assert!(!dispatch);
    state
        .store
        .configure_external_database_broker(&broker, &authority)
        .unwrap();
    let job = Job::new(&record, Uuid::new_v4(), Kind::Backup).unwrap();
    state.store.save_database_job(&job).unwrap();
}

#[tokio::test]
async fn iac_database_events_unknown_setup_keeps_the_same_private_inbox_and_rejects_old_attempts() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let (record, value, _) = setup(&state, root.path(), network);
    let original = create(&state, &value);
    state.store.prepare_database_events(&original).unwrap();
    state.store.checkpoint_database_events(&original).unwrap();
    let mut connection = record.input.clone();
    connection.uri = "postgresql://fixture:private-original-inbox@127.0.0.1:9/db".into();
    let mut binding = BindingRecord {
        id: value.id,
        database_id: record.id,
        input: BindingInput {
            request_id: value.id,
            service_id: value.input.service_id,
            access: Access::Read,
            env_name: "DATABASE_URL".into(),
        },
        connection,
        role: format!("fc{}", value.id.simple()),
        tables: vec![TableRequest {
            schema: "public".into(),
            table: "orders".into(),
        }],
        ready: false,
        event_inbox: true,
    };
    state
        .store
        .prepare_event_binding(&original, &binding)
        .unwrap();
    state.store.fail_database_events(&original).unwrap();
    let second = crate::persistence::Store::open(&root.path().join("state.db")).unwrap();
    let (recovered, dispatch) = second
        .recover_database_events(original.operation.operation_id)
        .unwrap();
    assert!(dispatch);
    assert_eq!(recovered.operation.execution_generation, 1);
    assert_eq!(
        second
            .event_binding(&recovered)
            .unwrap()
            .unwrap()
            .connection
            .uri,
        binding.connection.uri
    );
    assert!(matches!(
        state.store.checkpoint_database_events(&original),
        Err(PersistenceError::IacOperationIdentity)
    ));
    binding.ready = true;
    second.prepare_event_binding(&recovered, &binding).unwrap();
    second
        .finish_database_events(&recovered, Some(&value))
        .unwrap();
    assert_eq!(second.database_bindings().unwrap().len(), 1);
    assert_eq!(
        second.database_bindings().unwrap()[0].connection.uri,
        binding.connection.uri
    );
    assert!(matches!(
        second.fail_database_events(&recovered),
        Err(PersistenceError::IacOperationIdentity)
    ));
}

#[tokio::test]
async fn iac_database_events_not_drained_delete_restores_delivery_without_weakening_unknown_effects()
 {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let (_, mut value, _) = setup(&state, root.path(), network);
    value.detached = true;
    state.store.save_database_trigger(&value).unwrap();
    let intent = ready(
        state
            .store
            .begin_database_events(Uuid::new_v4(), EventChange::Delete { id: value.id })
            .unwrap(),
        true,
    );
    state.store.prepare_database_events(&intent).unwrap();
    state.store.database_events_not_drained(&intent).unwrap();
    assert!(
        !state
            .store
            .database_trigger(value.id)
            .unwrap()
            .unwrap()
            .paused
    );
    let op = state
        .store
        .iac_native_operation(intent.operation.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(op.state, NativeOperationState::Failed);
    assert_eq!(
        op.error_code.as_deref(),
        Some("database_events_not_drained")
    );
    let unknown = ready(
        state
            .store
            .begin_database_events(Uuid::new_v4(), EventChange::Delete { id: value.id })
            .unwrap(),
        true,
    );
    state.store.prepare_database_events(&unknown).unwrap();
    state.store.checkpoint_database_events(&unknown).unwrap();
    assert!(matches!(
        state.store.database_events_not_drained(&unknown),
        Err(PersistenceError::IacOperationIdentity)
    ));
    state.store.fail_database_events(&unknown).unwrap();
    assert!(
        state
            .store
            .database_trigger(value.id)
            .unwrap()
            .unwrap()
            .paused
    );
    assert_eq!(
        state
            .store
            .iac_native_operation(unknown.operation.operation_id)
            .unwrap()
            .unwrap()
            .state,
        NativeOperationState::NeedsRecovery
    );
}

#[tokio::test]
async fn iac_database_events_deleted_identity_cannot_be_recreated_and_delete_retry_is_read_only() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let (_, mut value, _) = setup(&state, root.path(), network);
    value.detached = true;
    state.store.save_database_trigger(&value).unwrap();
    let original = ready(
        state
            .store
            .begin_database_events(Uuid::new_v4(), EventChange::Delete { id: value.id })
            .unwrap(),
        true,
    );
    state.store.prepare_database_events(&original).unwrap();
    state.store.checkpoint_database_events(&original).unwrap();
    state.store.finish_database_events(&original, None).unwrap();
    let retry = ready(
        state
            .store
            .begin_database_events(
                original.operation.operation_id,
                EventChange::Delete { id: value.id },
            )
            .unwrap(),
        false,
    );
    assert_eq!(retry.operation.state, NativeOperationState::Succeeded);
    assert!(state.store.database_trigger(value.id).unwrap().is_none());
    assert!(matches!(
        state.store.begin_database_events(
            Uuid::new_v4(),
            EventChange::Create {
                database: value.database_id,
                input: value.input.clone()
            }
        ),
        Err(PersistenceError::IacOperationIdentity)
    ));
}

#[tokio::test]
async fn iac_database_events_maintenance_observes_only_detachment_under_original_execution() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let (record, value, _) = setup(&state, root.path(), network);
    state.store.save_database_trigger(&value).unwrap();
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &resource(&value))).unwrap();
    state.store.change_iac_resource_owner(&claim).unwrap();
    let queued = Job::new(&record, Uuid::new_v4(), Kind::Migration).unwrap();
    state.store.save_database_job(&queued).unwrap();
    let job = state
        .store
        .claim_database_job(queued.id, 0)
        .unwrap()
        .unwrap();
    let mut after = value.clone();
    after.detached = true;
    state
        .store
        .save_database_trigger_observation(&job, &value, &after)
        .unwrap();
    let mut changed = after.clone();
    changed.input.path = "/wrong-handler".into();
    assert!(matches!(
        state
            .store
            .save_database_trigger_observation(&job, &after, &changed),
        Err(PersistenceError::IacOperationIdentity)
    ));
    state
        .store
        .interrupt_database_job(job.id, job.execution_generation)
        .unwrap();
    assert!(matches!(
        state
            .store
            .save_database_trigger_observation(&job, &after, &after),
        Err(PersistenceError::StaleIacOwner)
    ));
}

#[tokio::test]
async fn iac_database_events_invalid_input_is_rejected_before_any_native_or_broker_side_effect() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let (record, mut value, _) = setup(&state, root.path(), network);
    value.input.path = "//untrusted.example/events".into();
    let (status, error) = call(
        &app,
        "POST",
        &format!("/api/pipe/databases/{}/triggers", record.id),
        json!(value.input),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert!(state.store.database_trigger(value.id).unwrap().is_none());
    assert!(
        state
            .store
            .iac_native_operation(value.id)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn iac_database_events_receipt_failure_keeps_prepared_metadata_and_original_fences() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let (record, mut value, _) = setup(&state, root.path(), network);
    state.store.save_database_trigger(&value).unwrap();
    let intent = ready(
        state
            .store
            .begin_database_events(Uuid::new_v4(), EventChange::Detach { id: value.id })
            .unwrap(),
        true,
    );
    state.store.prepare_database_events(&intent).unwrap();
    state.store.checkpoint_database_events(&intent).unwrap();
    value.detached = true;
    let conn = rusqlite::Connection::open(root.path().join("state.db")).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_event_receipt BEFORE UPDATE ON iac_native_operations WHEN NEW.state='succeeded' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(
        state
            .store
            .finish_database_events(&intent, Some(&value))
            .is_err()
    );
    let current = state.store.database_trigger(value.id).unwrap().unwrap();
    assert!(current.paused);
    assert!(!current.detached);
    state.store.fail_database_events(&intent).unwrap();
    assert!(matches!(
        state
            .store
            .save_database_job(&Job::new(&record, Uuid::new_v4(), Kind::Backup).unwrap()),
        Err(PersistenceError::IacResourceBusy)
    ));
    conn.execute_batch("DROP TRIGGER reject_event_receipt;")
        .unwrap();
    let (recovered, _) = state
        .store
        .recover_database_events(intent.operation.operation_id)
        .unwrap();
    state
        .store
        .finish_database_events(&recovered, Some(&value))
        .unwrap();
    assert!(
        state
            .store
            .database_trigger(value.id)
            .unwrap()
            .unwrap()
            .detached
    );
}

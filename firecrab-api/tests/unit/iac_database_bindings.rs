use super::*;
use crate::{
    persistence::{
        PersistenceError,
        iac_database_bindings::{Admission, BindingChange, BindingIntent},
    },
    pipe_databases::{
        DatabaseRecord,
        bindings::{Access, BindingInput, BindingRecord},
        maintenance::{Job, Kind},
        write::TableRequest,
    },
};
use firecrab_stack::NativeOperationState;

fn ready(admission: Admission, dispatch: bool) -> BindingIntent {
    let Admission::Ready(intent, actual) = admission else {
        panic!("expected binding admission")
    };
    assert_eq!(actual, dispatch);
    *intent
}
fn setup(
    state: &AppState,
    root: &std::path::Path,
    network: Uuid,
) -> (DatabaseRecord, BindingRecord) {
    let database = configuration_resource(state, root, network, "database_connection");
    let record = state
        .store
        .pipe_database(Uuid::parse_str(&database.id).unwrap())
        .unwrap()
        .unwrap();
    let service = Uuid::new_v4();
    state.store.insert_micro_service(&serde_json::from_value(json!({"id":service,"name":format!("binding-{}",service.simple()),"image":"ghcr.io/qa/app:1","port":8080,"cpu":1,"ram":512})).unwrap()).unwrap();
    let id = Uuid::new_v4();
    let mut connection = record.input.clone();
    connection.uri = format!(
        "postgresql://fc{}:private-binding-password@127.0.0.1:9/db",
        id.simple()
    );
    let binding = BindingRecord {
        id,
        database_id: record.id,
        input: BindingInput {
            request_id: id,
            service_id: service,
            access: Access::Read,
            env_name: "DATABASE_URL".into(),
        },
        connection,
        role: format!("fc{}", id.simple()),
        tables: vec![TableRequest {
            schema: "public".into(),
            table: "orders".into(),
        }],
        ready: false,
        event_inbox: false,
    };
    (record, binding)
}
fn create(state: &AppState, record: &DatabaseRecord, binding: &BindingRecord) -> BindingIntent {
    ready(
        state
            .store
            .begin_database_binding(
                Uuid::new_v4(),
                BindingChange::Create {
                    database: record.id,
                    input: binding.input.clone(),
                },
            )
            .unwrap(),
        true,
    )
}
fn binding_resource(id: Uuid) -> ManagedResource {
    ManagedResource {
        kind: "database_binding".into(),
        id: id.to_string(),
    }
}

#[tokio::test]
async fn iac_database_binding_recovery_keeps_generated_credentials_and_blocks_parent_writers() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let (record, binding) = setup(&state, root.path(), network);
    let authority = state
        .store
        .database_binding_configuration_authority(binding.id, &record, binding.input.service_id)
        .unwrap();
    let original = create(&state, &record, &binding);
    state
        .store
        .prepare_database_binding(&original, &binding)
        .unwrap();
    state.store.fail_database_binding(&original).unwrap();
    assert!(matches!(
        state.store.update_pipe_database(record.id, &record.input),
        Err(PersistenceError::IacResourceBusy)
    ));
    assert!(matches!(
        state
            .store
            .save_database_binding_configuration(&binding, &authority),
        Err(PersistenceError::IacResourceBusy)
    ));
    assert!(matches!(
        state.store.delete_micro_service(binding.input.service_id),
        Err(PersistenceError::IacResourceBusy)
    ));
    let second = crate::persistence::Store::open(&root.path().join("state.db")).unwrap();
    let (recovered, dispatch) = second
        .recover_database_binding(original.operation.operation_id)
        .unwrap();
    assert!(dispatch);
    let saved = second
        .database_binding_prepared(&recovered)
        .unwrap()
        .unwrap();
    assert_eq!(saved.connection.uri, binding.connection.uri);
    assert_eq!(saved.role, binding.role);
    assert!(matches!(
        state.store.finish_database_binding(&original),
        Err(PersistenceError::IacOperationIdentity)
    ));
    second.finish_database_binding(&recovered).unwrap();
    assert_eq!(second.database_bindings().unwrap().len(), 1);
    assert!(second.database_bindings().unwrap()[0].ready);
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &binding_resource(binding.id))).unwrap();
    state.store.change_iac_resource_owner(&claim).unwrap();
    assert_eq!(
        state.store.database_bindings().unwrap()[0].connection.uri,
        binding.connection.uri
    );
}

#[tokio::test]
async fn iac_database_binding_completed_retry_survives_account_removal_and_cannot_downgrade_success()
 {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let (record, binding) = setup(&state, root.path(), network);
    let original = create(&state, &record, &binding);
    state
        .store
        .prepare_database_binding(&original, &binding)
        .unwrap();
    state.store.finish_database_binding(&original).unwrap();
    let deletion = ready(
        state
            .store
            .begin_database_binding(Uuid::new_v4(), BindingChange::Delete { id: binding.id })
            .unwrap(),
        true,
    );
    state
        .store
        .begin_database_binding_effects(&deletion)
        .unwrap();
    state.store.finish_database_binding(&deletion).unwrap();
    let retry = ready(
        state
            .store
            .begin_database_binding(
                original.operation.operation_id,
                BindingChange::Create {
                    database: record.id,
                    input: binding.input.clone(),
                },
            )
            .unwrap(),
        false,
    );
    assert_eq!(retry.operation.state, NativeOperationState::Succeeded);
    assert!(
        state
            .store
            .database_binding_receipt(&retry)
            .unwrap()
            .unwrap()
            .ready
    );
    assert!(matches!(
        state.store.fail_database_binding(&retry),
        Err(PersistenceError::IacOperationIdentity)
    ));
    assert!(matches!(
        state.store.begin_database_binding(
            Uuid::new_v4(),
            BindingChange::Create {
                database: record.id,
                input: binding.input.clone()
            }
        ),
        Err(PersistenceError::IacOperationIdentity)
    ));
    let (_, dispatch) = state
        .store
        .recover_database_binding(deletion.operation.operation_id)
        .unwrap();
    assert!(!dispatch);
    assert!(state.store.database_bindings().unwrap().is_empty());
}

#[tokio::test]
async fn iac_database_binding_transfer_guards_both_http_mutators_and_legacy_inbox_writes() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let (record, mut binding) = setup(&state, root.path(), network);
    binding.ready = true;
    state.store.save_database_binding(&binding).unwrap();
    let authority = state
        .store
        .database_binding_configuration_authority(binding.id, &record, binding.input.service_id)
        .unwrap();
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &binding_resource(binding.id))).unwrap();
    state.store.change_iac_resource_owner(&claim).unwrap();
    for (method, path, body) in [
        (
            "DELETE",
            format!("/api/pipe/database-bindings/{}", binding.id),
            Value::Null,
        ),
        (
            "POST",
            format!("/api/pipe/databases/{}/bindings", record.id),
            json!(binding.input),
        ),
    ] {
        let (status, error) = call(&app, method, &path, body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["error"]["code"], "iac_ownership_conflict");
    }
    let mut broadened = binding.clone();
    broadened.event_inbox = true;
    assert!(matches!(
        state
            .store
            .save_database_binding_configuration(&broadened, &authority),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert!(matches!(
        state.store.delete_micro_service(binding.input.service_id),
        Err(PersistenceError::IacManagedDependent)
    ));
    assert_eq!(
        state.store.database_bindings().unwrap()[0].connection.uri,
        binding.connection.uri
    );
    assert!(!state.store.database_bindings().unwrap()[0].event_inbox);
}

#[tokio::test]
async fn iac_database_binding_maintenance_observation_changes_only_tables_under_its_current_attempt()
 {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let (record, mut binding) = setup(&state, root.path(), network);
    binding.ready = true;
    state.store.save_database_binding(&binding).unwrap();
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &binding_resource(binding.id))).unwrap();
    state.store.change_iac_resource_owner(&claim).unwrap();
    let queued = Job::new(&record, Uuid::new_v4(), Kind::Migration).unwrap();
    state.store.save_database_job(&queued).unwrap();
    let job = state
        .store
        .claim_database_job(queued.id, 0)
        .unwrap()
        .unwrap();
    let mut after = binding.clone();
    after.tables.push(TableRequest {
        schema: "public".into(),
        table: "customers".into(),
    });
    state
        .store
        .save_database_binding_observation(&job, &binding, &after)
        .unwrap();
    let mut escalation = after.clone();
    escalation.input.access = Access::Write;
    assert!(matches!(
        state
            .store
            .save_database_binding_observation(&job, &after, &escalation),
        Err(PersistenceError::IacOperationIdentity)
    ));
    state
        .store
        .interrupt_database_job(job.id, job.execution_generation)
        .unwrap();
    assert!(matches!(
        state
            .store
            .save_database_binding_observation(&job, &after, &after),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert_eq!(state.store.database_bindings().unwrap()[0].tables.len(), 2);
}

#[tokio::test]
async fn iac_database_binding_event_setup_rejects_implicit_privilege_change_before_broker_connection()
 {
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let (mut record, mut binding) = setup(&state, root.path(), network);
    record.input.write_enabled = true;
    state
        .store
        .update_pipe_database(record.id, &record.input)
        .unwrap();
    binding.ready = true;
    state.store.save_database_binding(&binding).unwrap();
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &binding_resource(binding.id))).unwrap();
    state.store.change_iac_resource_owner(&claim).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let broker=serde_json::from_value(json!({"brokers":[listener.local_addr().unwrap().to_string()],"tls":false,"username":"qa","password":"fixture-private-rabbitmq","development":true})).unwrap();
    state.store.save_database_rabbitmq(&broker).unwrap();
    let (status,error)=call(&app,"POST",&format!("/api/pipe/databases/{}/triggers",record.id),json!({"requestId":Uuid::new_v4(),"schema":"public","table":"orders","serviceId":binding.input.service_id})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "iac_resource_managed");
    assert!(state.store.database_triggers().unwrap().is_empty());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(60), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn iac_database_binding_failed_completion_keeps_pending_credentials_and_alias_reservations() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let (record, binding) = setup(&state, root.path(), network);
    let original = create(&state, &record, &binding);
    state
        .store
        .prepare_database_binding(&original, &binding)
        .unwrap();
    rusqlite::Connection::open(root.path().join("state.db")).unwrap().execute_batch("CREATE TRIGGER reject_binding_receipt BEFORE UPDATE ON iac_native_operations WHEN NEW.state='succeeded' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(state.store.finish_database_binding(&original).is_err());
    assert!(!state.store.database_bindings().unwrap()[0].ready);
    state.store.fail_database_binding(&original).unwrap();
    let queued = Job::new(&record, Uuid::new_v4(), Kind::Backup).unwrap();
    assert!(matches!(
        state.store.save_database_job(&queued),
        Err(PersistenceError::IacResourceBusy)
    ));
    rusqlite::Connection::open(root.path().join("state.db"))
        .unwrap()
        .execute_batch("DROP TRIGGER reject_binding_receipt;")
        .unwrap();
    let (recovered, _) = state
        .store
        .recover_database_binding(original.operation.operation_id)
        .unwrap();
    state.store.finish_database_binding(&recovered).unwrap();
    state.store.save_database_job(&queued).unwrap();
    assert_eq!(
        state.store.database_bindings().unwrap()[0].connection.uri,
        binding.connection.uri
    );
}

#[tokio::test]
async fn iac_database_binding_inbox_writer_cannot_cross_release_reclaim_history() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let (record, mut binding) = setup(&state, root.path(), network);
    binding.ready = true;
    state.store.save_database_binding(&binding).unwrap();
    let captured = state
        .store
        .database_binding_configuration_authority(binding.id, &record, binding.input.service_id)
        .unwrap();
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &binding_resource(binding.id))).unwrap();
    let owned = state.store.change_iac_resource_owner(&claim).unwrap();
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
    binding.event_inbox = true;
    assert!(matches!(
        state
            .store
            .save_database_binding_configuration(&binding, &captured),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert!(!state.store.database_bindings().unwrap()[0].event_inbox);
    let current = state
        .store
        .database_binding_configuration_authority(binding.id, &record, binding.input.service_id)
        .unwrap();
    state
        .store
        .save_database_binding_configuration(&binding, &current)
        .unwrap();
    assert!(state.store.database_bindings().unwrap()[0].event_inbox);
}

#[tokio::test]
async fn iac_database_binding_admitted_creation_survives_http_disconnect_without_allocating_an_account_twice()
 {
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let (mut record, binding) = setup(&state, root.path(), network);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    record.input.uri = format!(
        "postgresql://fixture:private-db-root@{}/db",
        listener.local_addr().unwrap()
    );
    state
        .store
        .update_pipe_database(record.id, &record.input)
        .unwrap();
    let input = json!(binding.input);
    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/pipe/databases/{}/bindings", record.id))
        .header("content-type", "application/json")
        .body(Body::from(input.to_string()))
        .unwrap();
    let http = tokio::spawn(app.clone().oneshot(request));
    let (connection, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        state
            .store
            .iac_native_operation(binding.id)
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
                .iac_native_operation(binding.id)
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
    let (status, error) = call(
        &app,
        "POST",
        &format!("/api/pipe/databases/{}/bindings", record.id),
        input,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{error}");
    assert_eq!(error["error"]["code"], "database_binding_unverified");
    assert!(state.store.database_bindings().unwrap().is_empty());
    assert!(
        tokio::time::timeout(Duration::from_millis(60), listener.accept())
            .await
            .is_err()
    );
}

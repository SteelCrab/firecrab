use super::*;
use crate::{
    persistence::{PersistenceError, Store},
    pipe_databases::{
        events::RabbitMQInput,
        managed_rabbitmq::{Cluster, default_image},
    },
};

pub(super) fn cluster() -> Cluster {
    serde_json::from_value(json!({"id":Uuid::new_v4(),"enabled":true,"image":default_image(),"development":true,"phase":"ready","error":null,"brokers":[{"id":Uuid::new_v4(),"ipv4":"127.0.0.1","env":{},"files":{}}],"config":{"brokers":["127.0.0.1:9"],"tls":false,"username":"fixture","password":"private-broker-fixture","development":true}})).unwrap()
}
pub(super) fn install(state: &AppState, cluster: &Cluster) -> crate::persistence::BrokerWatch {
    let authority = state.store.database_broker_authority().unwrap();
    state
        .store
        .configure_managed_database_broker(cluster, &authority)
        .unwrap();
    let mut watch = state.store.database_broker_watch(cluster).unwrap();
    state
        .store
        .save_database_broker_watch(&mut watch, cluster, true)
        .unwrap();
    watch
}

fn external(
    state: &AppState,
    id: Uuid,
    input: RabbitMQInput,
    dispatch: bool,
) -> crate::persistence::iac_database_brokers::BrokerIntent {
    let crate::persistence::iac_database_brokers::Admission::Ready(intent, actual) =
        state.store.begin_database_broker(id, input).unwrap()
    else {
        panic!("expected broker verification admission")
    };
    assert_eq!(actual, dispatch);
    *intent
}

#[tokio::test]
async fn iac_database_broker_original_success_is_immutable_after_later_configuration() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let input = cluster().config.unwrap();
    let id = Uuid::new_v4();
    let first = external(&state, id, input.clone(), true);
    state.store.finish_database_broker(&first).unwrap();
    let mut changed = input.clone();
    changed.username = "new-configuration".into();
    let second = external(&state, Uuid::new_v4(), changed.clone(), true);
    state.store.finish_database_broker(&second).unwrap();
    let original = external(&state, id, input, false);
    assert_eq!(
        original.operation.state,
        firecrab_stack::NativeOperationState::Succeeded
    );
    assert_eq!(
        state.store.database_rabbitmq().unwrap().unwrap().username,
        changed.username
    );
    assert!(state.store.begin_database_broker(id, changed).is_err());
    assert!(state.store.fail_database_broker(&first).is_err());
}

#[tokio::test]
async fn iac_database_broker_restart_recovery_retains_credentials_and_fences_the_old_attempt() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let input = cluster().config.unwrap();
    let original = external(&state, Uuid::new_v4(), input.clone(), true);
    state.store.interrupt_native_iac_operations().unwrap();
    let reopened = Store::open(&root.path().join("state.db")).unwrap();
    let (recovered, dispatch) = reopened
        .recover_database_broker(original.operation.operation_id)
        .unwrap();
    assert!(dispatch);
    assert_eq!(recovered.operation.execution_generation, 1);
    assert_eq!(recovered.input().password, input.password);
    assert!(state.store.finish_database_broker(&original).is_err());
    assert!(
        !serde_json::to_string(&recovered.operation)
            .unwrap()
            .contains(&input.password)
    );
    reopened.finish_database_broker(&recovered).unwrap();
    assert_eq!(
        reopened.database_rabbitmq().unwrap().unwrap().password,
        input.password
    );
}

#[tokio::test]
async fn iac_database_broker_verification_preserves_late_health_and_disables_supervision_atomically()
 {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let mut managed = cluster();
    let mut watch = install(&state, &managed);
    let original = external(
        &state,
        Uuid::new_v4(),
        managed.config.clone().unwrap(),
        true,
    );
    assert!(matches!(
        state.store.check_database_broker_watch(&watch, true),
        Err(PersistenceError::IacResourceBusy)
    ));
    managed.phase = "degraded".into();
    state
        .store
        .save_database_broker_watch(&mut watch, &managed, false)
        .unwrap();
    let injection = rusqlite::Connection::open(root.path().join("state.db")).unwrap();
    injection.execute_batch("CREATE TRIGGER reject_broker_success BEFORE UPDATE ON iac_native_operations WHEN NEW.state='succeeded' BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;").unwrap();
    assert!(state.store.finish_database_broker(&original).is_err());
    assert!(
        state
            .store
            .managed_database_rabbitmq()
            .unwrap()
            .unwrap()
            .enabled
    );
    assert_eq!(
        state
            .store
            .iac_native_operation(original.operation.operation_id)
            .unwrap()
            .unwrap()
            .state,
        firecrab_stack::NativeOperationState::Running
    );
    state.store.check_database_broker(&original).unwrap();
    injection
        .execute_batch("DROP TRIGGER reject_broker_success")
        .unwrap();
    state.store.finish_database_broker(&original).unwrap();
    let saved = state.store.managed_database_rabbitmq().unwrap().unwrap();
    assert!(!saved.enabled);
    assert_eq!(saved.phase, "degraded");
    assert!(
        state
            .store
            .check_database_broker_watch(&watch, false)
            .is_err()
    );
}

#[tokio::test]
async fn iac_database_broker_failed_handshake_survives_http_disconnect_without_reconfiguring() {
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut input = cluster().config.unwrap();
    input.brokers = vec![listener.local_addr().unwrap().to_string()];
    let id = Uuid::new_v4();
    let request = || {
        Request::builder()
            .method("PUT")
            .uri("/api/pipe/database-rabbitmq")
            .header("content-type", "application/json")
            .header("x-firecrab-operation-id", id.to_string())
            .body(Body::from(serde_json::to_vec(&input).unwrap()))
            .unwrap()
    };
    let http = tokio::spawn(app.clone().oneshot(request()));
    let (connection, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        state.store.iac_native_operation(id).unwrap().unwrap().state,
        firecrab_stack::NativeOperationState::Running
    );
    http.abort();
    assert!(http.await.unwrap_err().is_cancelled());
    drop(connection);
    tokio::time::timeout(Duration::from_secs(3), async {
        while state.store.iac_native_operation(id).unwrap().unwrap().state
            != firecrab_stack::NativeOperationState::Failed
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let retry = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(retry.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(retry.headers()["x-firecrab-operation-id"], id.to_string());
    assert!(state.store.database_rabbitmq().unwrap().is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(60), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn iac_database_broker_vm_insertion_requires_the_original_private_epoch() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let mut value = cluster();
    value.brokers[0].ipv4.clear();
    value.config = None;
    let authority = state.store.database_broker_authority().unwrap();
    state
        .store
        .configure_managed_database_broker(&value, &authority)
        .unwrap();
    let watch = state.store.database_broker_watch(&value).unwrap();
    let id = value.brokers[0].id;
    let grant = state.store.database_broker_vm_grant(&watch, id).unwrap();
    let vm:crate::model::VmRecord = serde_json::from_value(json!({"id":id,"name":"reserved-broker","template":"fixture","state":"created","cpu":1,"ram":1024,"micro_network_id":network})).unwrap();
    assert!(matches!(
        state.store.insert(&vm),
        Err(PersistenceError::IacParentManaged)
    ));
    let latest = state.store.database_broker_authority().unwrap();
    state
        .store
        .configure_managed_database_broker(&value, &latest)
        .unwrap();
    assert!(matches!(
        state.store.insert_database_broker_vm(&vm, &grant),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert!(state.store.load_all().unwrap().is_empty());
    let current = state.store.database_broker_watch(&value).unwrap();
    let grant = state.store.database_broker_vm_grant(&current, id).unwrap();
    state.store.insert_database_broker_vm(&vm, &grant).unwrap();
    assert_eq!(state.store.load_all().unwrap().len(), 1);
    assert!(state.store.insert_database_broker_vm(&vm, &grant).is_err());
    assert_eq!(state.store.load_all().unwrap().len(), 1);
}

#[tokio::test]
async fn iac_database_broker_watch_rejects_configuration_aba_and_preserves_newer_client_settings() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let original = cluster();
    let mut old_watch = install(&state, &original);
    let old_authority = state.store.database_broker_authority().unwrap();
    state
        .store
        .configure_external_database_broker(original.config.as_ref().unwrap(), &old_authority)
        .unwrap();
    let now = state.store.database_broker_authority().unwrap();
    state
        .store
        .configure_managed_database_broker(&original, &now)
        .unwrap();
    assert!(matches!(
        state.store.check_database_broker_watch(&old_watch, false),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert!(matches!(
        state
            .store
            .configure_external_database_broker(original.config.as_ref().unwrap(), &old_authority),
        Err(PersistenceError::StaleIacOwner)
    ));
    let mut late = original.clone();
    late.phase = "degraded".into();
    assert!(matches!(
        state
            .store
            .save_database_broker_watch(&mut old_watch, &late, true),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert_eq!(
        state
            .store
            .managed_database_rabbitmq()
            .unwrap()
            .unwrap()
            .phase,
        "ready"
    );
    let fresh = state.store.database_broker_watch(&original).unwrap();
    state
        .store
        .check_database_broker_watch(&fresh, true)
        .unwrap();
}

#[tokio::test]
async fn iac_database_broker_activation_and_controller_checkpoint_commit_together() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let mut value = cluster();
    let authority = state.store.database_broker_authority().unwrap();
    state
        .store
        .configure_managed_database_broker(&value, &authority)
        .unwrap();
    let mut watch = state.store.database_broker_watch(&value).unwrap();
    value.phase = "starting".into();
    let injection = rusqlite::Connection::open(root.path().join("state.db")).unwrap();
    injection.execute_batch("CREATE TRIGGER reject_broker_receipt BEFORE UPDATE ON database_managed_rabbitmq BEGIN SELECT RAISE(ABORT,'injected checkpoint failure'); END;").unwrap();
    assert!(
        state
            .store
            .save_database_broker_watch(&mut watch, &value, true)
            .is_err()
    );
    assert!(state.store.database_rabbitmq().unwrap().is_none());
    state
        .store
        .check_database_broker_watch(&watch, false)
        .unwrap();
    injection
        .execute_batch("DROP TRIGGER reject_broker_receipt")
        .unwrap();
    state
        .store
        .save_database_broker_watch(&mut watch, &value, true)
        .unwrap();
    assert_eq!(
        state.store.database_rabbitmq().unwrap().unwrap().username,
        "fixture"
    );
    state
        .store
        .check_database_broker_watch(&watch, false)
        .unwrap();
}

#[tokio::test]
async fn iac_database_broker_runtime_lock_excludes_other_api_handles_and_releases_after_drop() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let second = Store::open(&root.path().join("state.db")).unwrap();
    let runtime = store.database_broker_runtime_lock().unwrap();
    assert!(matches!(
        second.database_broker_runtime_lock(),
        Err(PersistenceError::IacResourceBusy)
    ));
    drop(runtime);
    let recovered = second.database_broker_runtime_lock().unwrap();
    drop(recovered);
}

#[tokio::test]
async fn iac_database_broker_reserved_vm_cannot_be_adopted_or_edited_before_insertion() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let value = cluster();
    install(&state, &value);
    let resource = ManagedResource {
        kind: "vm".into(),
        id: value.brokers[0].id.to_string(),
    };
    let change: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &resource)).unwrap();
    assert!(matches!(
        state.store.change_iac_resource_owner(&change),
        Err(PersistenceError::IacParentManaged)
    ));
    let (status, error) = call(
        &app,
        "PUT",
        &format!("/api/vms/{}", resource.id),
        json!({"ram":2048}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "iac_parent_managed");
}

#[tokio::test]
async fn iac_database_broker_event_read_lease_permits_health_but_fences_runtime_repair() {
    use crate::{
        persistence::iac_database_events::{Admission, EventChange},
        pipe_databases::events::TriggerInput,
    };
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let mut value = cluster();
    let mut watch = install(&state, &value);
    let resource = configuration_resource(&state, root.path(), network, "database_connection");
    let database = Uuid::parse_str(&resource.id).unwrap();
    let mut record = state.store.pipe_database(database).unwrap().unwrap();
    record.input.write_enabled = true;
    state
        .store
        .update_pipe_database(database, &record.input)
        .unwrap();
    let service = Uuid::new_v4();
    state.store.insert_micro_service(&serde_json::from_value(json!({"id":service,"name":"broker-event-handler","image":"ghcr.io/qa/app:1","port":8080,"cpu":1,"ram":512})).unwrap()).unwrap();
    let Admission::Ready(intent, true) = state
        .store
        .begin_database_events(
            Uuid::new_v4(),
            EventChange::Create {
                database,
                input: TriggerInput {
                    request_id: Uuid::new_v4(),
                    schema: "public".into(),
                    table: "orders".into(),
                    service_id: service,
                    path: "/events/database".into(),
                },
            },
        )
        .unwrap()
    else {
        panic!("expected event admission")
    };
    state
        .store
        .check_database_broker_watch(&watch, false)
        .unwrap();
    assert!(matches!(
        state.store.check_database_broker_watch(&watch, true),
        Err(PersistenceError::IacResourceBusy)
    ));
    value.phase = "degraded".into();
    state
        .store
        .save_database_broker_watch(&mut watch, &value, false)
        .unwrap();
    state.store.fail_database_events(&intent).unwrap();
    state
        .store
        .check_database_broker_watch(&watch, true)
        .unwrap();
    let config: RabbitMQInput = state.store.database_rabbitmq().unwrap().unwrap();
    assert_eq!(config.username, "fixture");
}

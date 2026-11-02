use super::*;
use crate::{
    persistence::{PersistenceError, Store},
    pipe_databases::{
        events::RabbitMQInput,
        managed_rabbitmq::{Cluster, default_image},
    },
};

fn cluster() -> Cluster {
    serde_json::from_value(json!({"id":Uuid::new_v4(),"enabled":true,"image":default_image(),"development":true,"phase":"ready","error":null,"brokers":[{"id":Uuid::new_v4(),"ipv4":"127.0.0.1","env":{},"files":{}}],"config":{"brokers":["127.0.0.1:9"],"tls":false,"username":"fixture","password":"private-broker-fixture","development":true}})).unwrap()
}
fn install(state: &AppState, cluster: &Cluster) -> crate::persistence::BrokerWatch {
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

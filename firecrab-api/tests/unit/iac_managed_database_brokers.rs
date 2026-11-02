use super::database_broker_tests::{cluster, install};
use super::*;
use crate::{
    persistence::{
        PersistenceError,
        iac_managed_database_brokers::{Admission, ManagedBrokerCommand, ManagedBrokerIntent},
    },
    pipe_databases::managed_rabbitmq::Cluster,
};
use firecrab_stack::NativeOperationState;

fn ready(admission: Admission, expected: bool) -> ManagedBrokerIntent {
    let Admission::Ready(intent, dispatch) = admission else {
        panic!("expected managed broker admission")
    };
    assert_eq!(dispatch, expected);
    *intent
}
fn prepare(state: &AppState, id: Uuid) -> ManagedBrokerIntent {
    ready(
        state
            .store
            .begin_managed_database_broker(
                id,
                ManagedBrokerCommand::Prepare { development: true },
                None,
            )
            .unwrap(),
        true,
    )
}
fn prepared(
    state: &AppState,
    intent: &ManagedBrokerIntent,
) -> (Cluster, crate::persistence::BrokerWatch) {
    let value = intent.cluster().unwrap();
    let watch = state.store.native_database_broker_watch(intent).unwrap();
    (value, watch)
}

#[tokio::test]
async fn iac_managed_database_broker_no_effect_failure_restores_configuration_and_terminal_identity()
 {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let id = Uuid::new_v4();
    let intent = prepare(&state, id);
    let ids: Vec<_> = intent
        .cluster()
        .unwrap()
        .brokers
        .iter()
        .map(|b| b.id)
        .collect();
    assert_eq!(ids.len(), 1);
    state.store.fail_managed_database_broker(&intent).unwrap();
    assert!(state.store.managed_database_rabbitmq().unwrap().is_none());
    let cached = ready(
        state
            .store
            .begin_managed_database_broker(
                id,
                ManagedBrokerCommand::Prepare { development: true },
                None,
            )
            .unwrap(),
        false,
    );
    assert_eq!(cached.operation.state, NativeOperationState::Failed);
    assert_eq!(cached.cluster().unwrap().brokers[0].id, ids[0]);
    let second = prepare(&state, Uuid::new_v4());
    assert_ne!(second.cluster().unwrap().brokers[0].id, ids[0]);
}

#[tokio::test]
async fn iac_managed_database_broker_unknown_setup_retains_original_tls_and_rejects_old_attempts() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let original = prepare(&state, Uuid::new_v4());
    state
        .store
        .managed_database_broker_effects(&original)
        .unwrap();
    let (mut value, mut watch) = prepared(&state, &original);
    value.brokers[0].ipv4 = "127.0.0.1".into();
    value.brokers[0]
        .env
        .insert("PRIVATE_INBOX_COOKIE".into(), Uuid::new_v4().to_string());
    value.config = cluster().config;
    state
        .store
        .save_database_broker_watch(&mut watch, &value, false)
        .unwrap();
    state.store.fail_managed_database_broker(&original).unwrap();
    let second = crate::persistence::Store::open(&root.path().join("state.db")).unwrap();
    let (recovered, dispatch) = second
        .recover_managed_database_broker(original.operation.operation_id)
        .unwrap();
    assert!(dispatch);
    assert_eq!(recovered.operation.execution_generation, 1);
    let saved = recovered.cluster().unwrap();
    assert_eq!(saved.brokers[0].id, value.brokers[0].id);
    assert_eq!(saved.brokers[0].env, value.brokers[0].env);
    assert_eq!(
        saved.config.as_ref().unwrap().password,
        value.config.as_ref().unwrap().password
    );
    assert!(
        state
            .store
            .save_database_broker_watch(&mut watch, &value, false)
            .is_err()
    );
    let mut current = second.native_database_broker_watch(&recovered).unwrap();
    let mut changed = saved.clone();
    changed.config.as_mut().unwrap().password = "replacement-rejected".into();
    assert!(
        second
            .save_database_broker_watch(&mut current, &changed, false)
            .is_err()
    );
    let mut completed = saved;
    completed.phase = "ready".into();
    completed.error = None;
    second
        .save_database_broker_watch(&mut current, &completed, true)
        .unwrap();
    assert_eq!(
        second
            .iac_native_operation(original.operation.operation_id)
            .unwrap()
            .unwrap()
            .state,
        NativeOperationState::Succeeded
    );
    assert_eq!(
        second
            .managed_database_broker_receipt(original.operation.operation_id)
            .unwrap()
            .unwrap()["id"],
        completed.id.to_string()
    );
    assert!(state.store.fail_managed_database_broker(&original).is_err());
}

#[tokio::test]
async fn iac_managed_database_broker_receipt_failure_rolls_back_client_activation_and_keeps_fence()
{
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let intent = prepare(&state, Uuid::new_v4());
    state
        .store
        .managed_database_broker_effects(&intent)
        .unwrap();
    let (mut value, mut watch) = prepared(&state, &intent);
    value.brokers[0].ipv4 = "127.0.0.1".into();
    value.config = cluster().config;
    value.phase = "ready".into();
    let injection = rusqlite::Connection::open(root.path().join("state.db")).unwrap();
    injection.execute_batch("CREATE TRIGGER reject_managed_receipt BEFORE UPDATE ON iac_native_operations WHEN NEW.state='succeeded' BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;").unwrap();
    assert!(
        state
            .store
            .save_database_broker_watch(&mut watch, &value, true)
            .is_err()
    );
    assert!(state.store.database_rabbitmq().unwrap().is_none());
    assert_eq!(
        state
            .store
            .iac_native_operation(intent.operation.operation_id)
            .unwrap()
            .unwrap()
            .state,
        NativeOperationState::Running
    );
    assert!(matches!(
        state
            .store
            .begin_database_broker(Uuid::new_v4(), cluster().config.unwrap()),
        Err(PersistenceError::IacResourceBusy)
    ));
    injection
        .execute_batch("DROP TRIGGER reject_managed_receipt")
        .unwrap();
    state
        .store
        .save_database_broker_watch(&mut watch, &value, true)
        .unwrap();
    assert!(state.store.database_rabbitmq().unwrap().is_some());
}

#[tokio::test]
async fn iac_managed_database_broker_disconnection_retries_do_not_clear_a_newer_client_selection() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let id = Uuid::new_v4();
    let request = || {
        Request::builder()
            .method("DELETE")
            .uri("/api/pipe/database-rabbitmq")
            .header("x-firecrab-operation-id", id.to_string())
            .body(Body::empty())
            .unwrap()
    };
    let response = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while state.store.iac_native_operation(id).unwrap().unwrap().state
            != NativeOperationState::Succeeded
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let external = cluster().config.unwrap();
    let crate::persistence::iac_database_brokers::Admission::Ready(selection, true) = state
        .store
        .begin_database_broker(Uuid::new_v4(), external.clone())
        .unwrap()
    else {
        panic!("external selection admission")
    };
    state.store.finish_database_broker(&selection).unwrap();
    let retry = app.oneshot(request()).await.unwrap();
    assert_eq!(retry.status(), StatusCode::OK);
    assert_eq!(
        state.store.database_rabbitmq().unwrap().unwrap().password,
        external.password
    );
}

#[tokio::test]
async fn iac_managed_database_broker_repair_lease_blocks_credential_changes_before_guest_dispatch()
{
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let value = cluster();
    let watch = install(&state, &value);
    let intent = ready(
        state
            .store
            .begin_managed_database_broker(
                Uuid::new_v4(),
                ManagedBrokerCommand::Repair {
                    cluster: value.id,
                    restart_vm: None,
                },
                Some(&watch),
            )
            .unwrap(),
        true,
    );
    assert!(matches!(
        state
            .store
            .begin_database_broker(Uuid::new_v4(), value.config.clone().unwrap()),
        Err(PersistenceError::IacResourceBusy)
    ));
    assert!(
        state
            .store
            .check_database_broker_watch(&watch, false)
            .is_err()
    );
    let private = state.store.native_database_broker_watch(&intent).unwrap();
    state
        .store
        .check_database_broker_watch(&private, true)
        .unwrap();
    state.store.fail_managed_database_broker(&intent).unwrap();
    assert_eq!(
        state
            .store
            .managed_database_rabbitmq()
            .unwrap()
            .unwrap()
            .phase,
        value.phase
    );
}

#[tokio::test]
async fn iac_managed_database_broker_frozen_repair_can_restore_transport_for_uncertain_event_work()
{
    use crate::{
        persistence::iac_database_events::{Admission as EventAdmission, EventChange},
        pipe_databases::events::TriggerInput,
    };
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let mut value = cluster();
    value.image = format!("docker.io/library/rabbitmq@sha256:{}", "a".repeat(64));
    let watch = install(&state, &value);
    let resource = configuration_resource(&state, root.path(), network, "database_connection");
    let database = Uuid::parse_str(&resource.id).unwrap();
    let mut record = state.store.pipe_database(database).unwrap().unwrap();
    record.input.write_enabled = true;
    state
        .store
        .update_pipe_database(database, &record.input)
        .unwrap();
    let service = Uuid::new_v4();
    state.store.insert_micro_service(&serde_json::from_value(json!({"id":service,"name":"repair-event-handler","image":"ghcr.io/qa/app:1","port":8080,"cpu":1,"ram":512})).unwrap()).unwrap();
    let EventAdmission::Ready(event, true) = state
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
    state.store.prepare_database_events(&event).unwrap();
    state.store.checkpoint_database_events(&event).unwrap();
    state.store.fail_database_events(&event).unwrap();
    assert!(matches!(
        state
            .store
            .begin_database_broker(Uuid::new_v4(), value.config.clone().unwrap()),
        Err(PersistenceError::IacResourceBusy)
    ));
    let repair = ready(
        state
            .store
            .begin_managed_database_broker(
                Uuid::new_v4(),
                ManagedBrokerCommand::Repair {
                    cluster: value.id,
                    restart_vm: None,
                },
                Some(&watch),
            )
            .unwrap(),
        true,
    );
    state
        .store
        .managed_database_broker_effects(&repair)
        .unwrap();
    let (mut progress, mut private) = prepared(&state, &repair);
    progress.phase = "ready".into();
    state
        .store
        .save_database_broker_watch(&mut private, &progress, true)
        .unwrap();
    let observation = state.store.database_broker_watch(&progress).unwrap();
    state
        .store
        .check_database_broker_watch(&observation, false)
        .unwrap();
    assert!(
        state
            .store
            .check_database_broker_watch(&observation, true)
            .is_err()
    );
    let (recovered, dispatch) = state
        .store
        .recover_database_events(event.operation.operation_id)
        .unwrap();
    assert!(dispatch);
    assert_eq!(recovered.operation.execution_generation, 1);
    assert_eq!(
        state.store.database_rabbitmq().unwrap().unwrap().password,
        value.config.unwrap().password
    );
}

#[tokio::test]
async fn iac_managed_database_broker_transfer_preserves_credentials_and_rejects_public_writers() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let input = cluster().config.unwrap();
    let crate::persistence::iac_database_brokers::Admission::Ready(selection, true) = state
        .store
        .begin_database_broker(Uuid::new_v4(), input.clone())
        .unwrap()
    else {
        panic!("external admission")
    };
    state.store.finish_database_broker(&selection).unwrap();
    let resource = ManagedResource {
        kind: "database_rabbitmq".into(),
        id: "default".into(),
    };
    let change: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &resource)).unwrap();
    let owned = state.store.change_iac_resource_owner(&change).unwrap();
    assert_eq!(owned.resource, resource);
    assert_eq!(
        state.store.database_rabbitmq().unwrap().unwrap().password,
        input.password
    );
    let (status, error) = call(&app, "PUT", "/api/pipe/database-rabbitmq", json!(input)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    let (status, error) = call(
        &app,
        "POST",
        "/api/pipe/database-rabbitmq/managed",
        json!({"development":true}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    let (status, error) = call(&app, "DELETE", "/api/pipe/database-rabbitmq", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert!(state.store.managed_database_rabbitmq().unwrap().is_none());
    assert_eq!(
        state.store.database_rabbitmq().unwrap().unwrap().password,
        input.password
    );
    let alias = ManagedResource {
        kind: "database_rabbitmq".into(),
        id: Uuid::new_v4().to_string(),
    };
    let request: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &alias)).unwrap();
    assert!(matches!(
        state.store.change_iac_resource_owner(&request),
        Err(PersistenceError::InvalidStackResource)
    ));
}

#[tokio::test]
async fn iac_managed_database_broker_handover_invalidates_the_old_controller_before_repair() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let value = cluster();
    let old = install(&state, &value);
    let resource = ManagedResource {
        kind: "database_rabbitmq".into(),
        id: "default".into(),
    };
    let change: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &resource)).unwrap();
    let owned = state.store.change_iac_resource_owner(&change).unwrap();
    assert!(
        state
            .store
            .check_database_broker_watch(&old, false)
            .is_err()
    );
    assert!(state.store.database_broker_watch(&value).is_err());
    assert!(
        state
            .store
            .begin_managed_database_broker(
                Uuid::new_v4(),
                ManagedBrokerCommand::Repair {
                    cluster: value.id,
                    restart_vm: None
                },
                Some(&old)
            )
            .is_err()
    );
    let mut release = change.clone();
    release.operation_id = Uuid::new_v4();
    release.expected_generation = owned.generation;
    release.expected_owner = owned.owner;
    release.owner = None;
    state.store.change_iac_resource_owner(&release).unwrap();
    assert!(
        state
            .store
            .check_database_broker_watch(&old, false)
            .is_err()
    );
    let current = state.store.database_broker_watch(&value).unwrap();
    state
        .store
        .check_database_broker_watch(&current, true)
        .unwrap();
}

#[tokio::test]
async fn iac_managed_database_broker_recovery_rejects_changed_disk_identity_before_activation() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let intent = prepare(&state, Uuid::new_v4());
    state
        .store
        .managed_database_broker_effects(&intent)
        .unwrap();
    let (mut value, mut watch) = prepared(&state, &intent);
    let grant = state
        .store
        .database_broker_vm_grant(&watch, value.brokers[0].id)
        .unwrap();
    let mut vm =
        crate::handlers::vms::test_support::record("retained-broker-disk", value.brokers[0].id);
    vm.micro_network_id = network;
    vm.disk_generation = Some(Uuid::new_v4());
    state.store.insert_database_broker_vm(&vm, &grant).unwrap();
    value.brokers[0].disk_generation = vm.disk_generation;
    value.brokers[0].ipv4 = "127.0.0.1".into();
    value.config = cluster().config;
    state
        .store
        .save_database_broker_watch(&mut watch, &value, false)
        .unwrap();
    state.store.fail_managed_database_broker(&intent).unwrap();
    let (recovered, dispatch) = state
        .store
        .recover_managed_database_broker(intent.operation.operation_id)
        .unwrap();
    assert!(dispatch);
    let mut current = state
        .store
        .native_database_broker_watch(&recovered)
        .unwrap();
    let mut final_value = recovered.cluster().unwrap();
    final_value.phase = "ready".into();
    let injection = rusqlite::Connection::open(root.path().join("state.db")).unwrap();
    // Even a restored VM row with the same UUID must retain its original disk.
    injection
        .execute(
            "UPDATE vms SET disk_generation=?1 WHERE id=?2",
            rusqlite::params![Uuid::new_v4().to_string(), vm.id.to_string()],
        )
        .unwrap();
    assert!(
        state
            .store
            .save_database_broker_watch(&mut current, &final_value, true)
            .is_err()
    );
    assert!(state.store.database_rabbitmq().unwrap().is_none());
    assert_eq!(
        state
            .store
            .iac_native_operation(intent.operation.operation_id)
            .unwrap()
            .unwrap()
            .state,
        NativeOperationState::Running
    );
    injection
        .execute(
            "UPDATE vms SET disk_generation=NULL WHERE id=?1",
            [vm.id.to_string()],
        )
        .unwrap();
    assert!(
        state
            .store
            .save_database_broker_watch(&mut current, &final_value, true)
            .is_err()
    );
    injection
        .execute(
            "UPDATE vms SET disk_generation=?1 WHERE id=?2",
            rusqlite::params![vm.disk_generation.unwrap().to_string(), vm.id.to_string()],
        )
        .unwrap();
    state
        .store
        .save_database_broker_watch(&mut current, &final_value, true)
        .unwrap();
    assert_eq!(
        state
            .store
            .iac_native_operation(intent.operation.operation_id)
            .unwrap()
            .unwrap()
            .execution_generation,
        1
    );
    assert!(state.store.insert_database_broker_vm(&vm, &grant).is_err());
}

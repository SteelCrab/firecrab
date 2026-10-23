use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use firecrab_stack::{Binding, ResourceKind, Stack};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;
use crate::server::{HttpConfig, build_router};
use crate::templates::TemplateRegistry;

async fn fixture(
    root: &std::path::Path,
    fail: Option<&'static str>,
) -> (Router, AppState, Arc<Mutex<Vec<&'static str>>>) {
    let templates = TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
    let state = AppState::with_db_file(templates, root.join("state.db"))
        .await
        .unwrap();
    let socket = root.join("helper.sock");
    let (_, calls) = crate::network::test_support::spawn_recording_helper(&socket, fail);
    let state = state.with_test_network(crate::network::NetworkClient::with_socket_path(socket));
    let router = build_router(
        state.clone(),
        &HttpConfig {
            bind_addr: "127.0.0.1:5523".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 128,
            request_timeout: std::time::Duration::from_secs(10),
            static_root: None,
        },
    );
    state.store.register_stack("demo").unwrap();
    (router, state, calls)
}

fn declaration() -> Value {
    json!({"apiVersion":"firecrab.dev/v1alpha1","kind":"Stack","name":"demo","microNetworks":{"app":{"name":"demo-net","subnetCidr":"172.31.20.0/24"}}})
}

async fn request(router: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}

async fn planned(router: &Router, stack: &Value, operation_id: Uuid) -> Value {
    let (code, plan) = request(router, "POST", "/api/stacks/demo/plan", stack.clone()).await;
    assert_eq!(code, StatusCode::OK, "{plan}");
    json!({"stack":stack,"expectedFingerprint":plan["fingerprint"],"operationId":operation_id,"allowStop":false})
}

async fn terminal(router: &Router, id: Uuid) -> ApplyStatus {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let (code, status) = request(
                router,
                "GET",
                &format!("/api/stacks/demo/applies/{id}"),
                Value::Null,
            )
            .await;
            assert_eq!(code, StatusCode::OK, "{status}");
            let status: ApplyStatus = serde_json::from_value(status).unwrap();
            if status.state.is_terminal() {
                return status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("apply reached a terminal state")
}

#[tokio::test]
async fn stack_apply_creates_updates_and_repeats_without_duplicate_effects() {
    let root = tempfile::tempdir().unwrap();
    let (router, state, calls) = fixture(root.path(), None).await;
    let id = Uuid::new_v4();
    let body = planned(&router, &declaration(), id).await;
    let (a, b) = tokio::join!(
        request(&router, "POST", "/api/stacks/demo/apply", body.clone()),
        request(&router, "POST", "/api/stacks/demo/apply", body.clone())
    );
    assert_eq!(a.0, StatusCode::ACCEPTED, "{}", a.1);
    assert_eq!(b.0, StatusCode::ACCEPTED, "{}", b.1);
    let result = terminal(&router, id).await;
    assert_eq!(result.state, ApplyState::Succeeded, "{result:?}");
    let network = state.store.list_micro_networks().unwrap().pop().unwrap();
    assert_eq!(result.changes[0].change.resource_id, Some(network.id));
    assert_eq!(state.store.stack("demo").unwrap().unwrap().revision, 1);
    assert!(
        calls
            .lock()
            .unwrap()
            .contains(&"ensure_micro_network_bridge")
    );
    let before = calls.lock().unwrap().len();
    assert_eq!(
        request(&router, "POST", "/api/stacks/demo/apply", body.clone())
            .await
            .1,
        serde_json::to_value(&result).unwrap()
    );
    assert_eq!(calls.lock().unwrap().len(), before);
    let mut changed = declaration();
    changed["microNetworks"]["app"]["internetEnabled"] = json!(false);
    let change_id = Uuid::new_v4();
    let update = planned(&router, &changed, change_id).await;
    assert_eq!(
        request(&router, "POST", "/api/stacks/demo/apply", update)
            .await
            .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        terminal(&router, change_id).await.state,
        ApplyState::Succeeded
    );
    let updated = state.store.micro_network(network.id).unwrap().unwrap();
    assert!(!updated.internet_enabled);
    assert_eq!(updated.id, network.id);
    let noop_id = Uuid::new_v4();
    let noop = planned(&router, &changed, noop_id).await;
    let count = calls.lock().unwrap().len();
    request(&router, "POST", "/api/stacks/demo/apply", noop).await;
    let result = terminal(&router, noop_id).await;
    assert_eq!(result.state, ApplyState::Succeeded);
    assert_eq!(result.changes[0].change.action, Action::Noop);
    assert_eq!(calls.lock().unwrap().len(), count);
    let mut conflict = body;
    conflict["stack"] = changed;
    assert_eq!(
        request(&router, "POST", "/api/stacks/demo/apply", conflict)
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn stack_apply_rejects_stale_plans_replacement_and_implicit_adoption_before_reserving() {
    let root = tempfile::tempdir().unwrap();
    let (router, state, calls) = fixture(root.path(), None).await;
    let body = planned(&router, &declaration(), Uuid::new_v4()).await;
    let network: firecrab_api_types::MicroNetworkResponse = serde_json::from_value(json!({"id":Uuid::new_v4(),"name":"demo-net","subnetCidr":"172.31.20.0/24","gateway":"172.31.20.1","internetEnabled":true})).unwrap();
    state.store.insert_micro_network(&network).unwrap();
    let rejected = request(&router, "POST", "/api/stacks/demo/apply", body).await;
    assert_eq!(rejected.0, StatusCode::CONFLICT);
    assert_eq!(rejected.1["error"]["code"], "stale_stack_plan");
    let blocked = planned(&router, &declaration(), Uuid::new_v4()).await;
    assert_eq!(
        request(&router, "POST", "/api/stacks/demo/apply", blocked)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert!(
        state
            .store
            .stack("demo")
            .unwrap()
            .unwrap()
            .bindings
            .is_empty()
    );
    state
        .store
        .bind_stack_resource(
            "demo",
            &Binding {
                kind: ResourceKind::Network,
                key: "app".into(),
                resource_id: network.id,
            },
        )
        .unwrap();
    let mut replacement = declaration();
    replacement["microNetworks"]["app"]["subnetCidr"] = json!("172.31.21.0/24");
    let body = planned(&router, &replacement, Uuid::new_v4()).await;
    assert_eq!(
        request(&router, "POST", "/api/stacks/demo/apply", body)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        state.store.micro_network(network.id).unwrap(),
        Some(network)
    );
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stack_apply_failure_retains_reserved_identity_and_does_not_replay_on_retry() {
    let root = tempfile::tempdir().unwrap();
    let (router, state, calls) = fixture(root.path(), Some("ensure_micro_network_bridge")).await;
    let id = Uuid::new_v4();
    let body = planned(&router, &declaration(), id).await;
    assert_eq!(
        request(&router, "POST", "/api/stacks/demo/apply", body.clone())
            .await
            .0,
        StatusCode::ACCEPTED
    );
    let failed = terminal(&router, id).await;
    assert_eq!(failed.state, ApplyState::Failed);
    assert_eq!(failed.changes[0].state, StepState::Failed);
    let stack = state.store.stack("demo").unwrap().unwrap();
    assert_eq!(
        failed.changes[0].change.resource_id,
        Some(stack.bindings[0].resource_id)
    );
    assert!(state.store.list_micro_networks().unwrap().is_empty());
    let count = calls.lock().unwrap().len();
    assert_eq!(
        request(&router, "POST", "/api/stacks/demo/apply", body)
            .await
            .1,
        serde_json::to_value(failed).unwrap()
    );
    assert_eq!(calls.lock().unwrap().len(), count);
    let (_, plan) = request(&router, "POST", "/api/stacks/demo/plan", declaration()).await;
    assert_eq!(plan["blocked"], true);
}

#[tokio::test]
async fn stack_apply_restart_preserves_reservations_guards_ownership_and_cleans_only_terminal_history()
 {
    let root = tempfile::tempdir().unwrap();
    let (router, state, _) = fixture(root.path(), None).await;
    let record = state.store.stack("demo").unwrap().unwrap();
    let id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let status = ApplyStatus {
        operation_id: id,
        stack_id: record.id,
        name: "demo".into(),
        state: ApplyState::Queued,
        changes: vec![ApplyStep {
            change: firecrab_stack::Change {
                kind: ResourceKind::Network,
                key: "app".into(),
                resource_id: Some(resource_id),
                action: Action::Create,
                fields: vec![],
                reason: None,
            },
            state: StepState::Pending,
        }],
        error: None,
    };
    let hash =
        firecrab_stack::desired_hash(&serde_json::from_value::<Stack>(declaration()).unwrap());
    assert!(
        state
            .store
            .begin_stack_apply(&record, &hash, false, &status)
            .unwrap()
            .1
    );
    assert!(
        !state
            .store
            .begin_stack_apply(&record, &hash, false, &status)
            .unwrap()
            .1
    );
    let binding = json!({"kind":"network","key":"app","resourceId":resource_id});
    assert_eq!(
        request(&router, "POST", "/api/stacks/demo/unbind", binding.clone())
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &router,
            "DELETE",
            &format!("/api/stacks/demo?id={}", record.id),
            Value::Null
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let other = state.store.register_stack("other").unwrap();
    let mut competing = status.clone();
    competing.operation_id = Uuid::new_v4();
    competing.stack_id = other.id;
    competing.name = other.name.clone();
    assert!(matches!(
        state
            .store
            .begin_stack_apply(&other, &hash, false, &competing),
        Err(PersistenceError::StackApplyBusy)
    ));
    assert!(matches!(
        state.store.begin_stack_apply(&record, &hash, true, &status),
        Err(PersistenceError::StackApplyIdentity)
    ));
    let templates = TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap();
    let restarted = AppState::with_db_file(templates, root.path().join("state.db"))
        .await
        .unwrap();
    let after = restarted
        .store
        .stored_stack_apply(id)
        .unwrap()
        .unwrap()
        .status;
    assert_eq!(after.state, ApplyState::Interrupted);
    assert_eq!(after.changes[0].change.resource_id, Some(resource_id));
    assert_eq!(
        restarted
            .store
            .stack("demo")
            .unwrap()
            .unwrap()
            .bindings
            .len(),
        1
    );
    assert_eq!(
        request(
            &router,
            "GET",
            &format!("/api/stacks/other/applies/{id}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&router, "POST", "/api/stacks/demo/unbind", binding)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &router,
            "DELETE",
            &format!("/api/stacks/demo?id={}", record.id),
            Value::Null
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert!(restarted.store.stored_stack_apply(id).unwrap().is_none());
    assert_eq!(restarted.store.stack("other").unwrap(), Some(other));
}

#[tokio::test]
async fn stack_apply_requires_stop_permission_before_any_other_step_or_reservation() {
    let root = tempfile::tempdir().unwrap();
    let (router, state, calls) = fixture(root.path(), None).await;
    let network =
        crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);
    state
        .store
        .bind_stack_resource(
            "demo",
            &Binding {
                kind: ResourceKind::Network,
                key: "app".into(),
                resource_id: network,
            },
        )
        .unwrap();
    let vm_id = Uuid::new_v4();
    let mut vm = crate::handlers::vms::test_support::record("worker", vm_id);
    vm.disk_gb = 1;
    vm.ram = 256;
    vm.micro_network_id = network;
    vm.state = firecrab_api_types::VmState::Running;
    vm.env.insert("MARKER".into(), "DO-NOT-PRINT".into());
    state.store.insert(&vm).unwrap();
    state.vms.lock().unwrap().insert(vm_id, vm.clone());
    state
        .store
        .bind_stack_resource(
            "demo",
            &Binding {
                kind: ResourceKind::Vm,
                key: "worker".into(),
                resource_id: vm_id,
            },
        )
        .unwrap();
    let mut stack = declaration();
    stack["microNetworks"]["app"] = json!({"name":"test-net","subnetCidr":"172.31.0.0/24"});
    stack["microNetworks"]["extra"] = json!({"name":"extra","subnetCidr":"172.31.22.0/24"});
    stack["microVMs"] = json!({"worker":{"name":"worker","template":vm.template,"network":"app","ram":256,"cpu":2,"diskGb":1,"env":{"MARKER":"NEW-DO-NOT-PRINT"}}});
    let body = planned(&router, &stack, Uuid::new_v4()).await;
    let (code, error) = request(&router, "POST", "/api/stacks/demo/apply", body).await;
    assert_eq!(code, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "stack_stop_permission_required");
    assert!(!error.to_string().contains("DO-NOT-PRINT"));
    assert_eq!(state.store.list_micro_networks().unwrap().len(), 1);
    assert_eq!(state.store.stack("demo").unwrap().unwrap().revision, 2);
    assert_eq!(state.vms.lock().unwrap()[&vm_id].cpu, 1);
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stack_apply_rejects_internal_or_missing_templates_before_creating_a_network() {
    let root = tempfile::tempdir().unwrap();
    let (router, state, calls) = fixture(root.path(), None).await;
    for template in ["__microboot", "missing-template"] {
        let mut stack = declaration();
        stack["microVMs"] = json!({"worker":{"name":"worker","template":template,"network":"app","ram":256,"cpu":1,"diskGb":1}});
        let body = planned(&router, &stack, Uuid::new_v4()).await;
        assert!(
            request(&router, "POST", "/api/stacks/demo/apply", body)
                .await
                .0
                .is_client_error()
        );
    }
    assert!(state.store.list_micro_networks().unwrap().is_empty());
    assert!(
        state
            .store
            .stack("demo")
            .unwrap()
            .unwrap()
            .bindings
            .is_empty()
    );
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stack_apply_updates_an_inactive_vm_preserves_policy_and_redacts_journal_then_retains_removed_declarations()
 {
    let root = tempfile::tempdir().unwrap();
    let (router, state, _) = fixture(root.path(), None).await;
    let network =
        crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);
    state
        .store
        .bind_stack_resource(
            "demo",
            &Binding {
                kind: ResourceKind::Network,
                key: "app".into(),
                resource_id: network,
            },
        )
        .unwrap();
    let group = firecrab_api_types::SecurityGroupResponse {
        id: Uuid::new_v4(),
        micro_network_id: network,
        name: "policy".into(),
        rules: vec![],
    };
    state.store.insert_security_group(&group).unwrap();
    let vm_id = Uuid::new_v4();
    let mut vm = crate::handlers::vms::test_support::record("worker", vm_id);
    vm.disk_gb = 1;
    vm.ram = 256;
    vm.micro_network_id = network;
    vm.security_group_id = Some(group.id);
    state.store.insert(&vm).unwrap();
    state.vms.lock().unwrap().insert(vm_id, vm.clone());
    state
        .store
        .bind_stack_resource(
            "demo",
            &Binding {
                kind: ResourceKind::Vm,
                key: "worker".into(),
                resource_id: vm_id,
            },
        )
        .unwrap();
    let mut stack = declaration();
    stack["microNetworks"]["app"] = json!({"name":"test-net","subnetCidr":"172.31.0.0/24"});
    stack["microVMs"] = json!({"worker":{"name":"worker","template":vm.template,"network":"app","ram":256,"cpu":2,"diskGb":1,"env":{"MARKER":"DO-NOT-PRINT-APPLY"}}});
    let id = Uuid::new_v4();
    let body = planned(&router, &stack, id).await;
    assert_eq!(
        request(&router, "POST", "/api/stacks/demo/apply", body)
            .await
            .0,
        StatusCode::ACCEPTED
    );
    let status = terminal(&router, id).await;
    assert_eq!(status.state, ApplyState::Succeeded, "{status:?}");
    let current = state.store.load_all().unwrap().remove(&vm_id).unwrap();
    assert_eq!(current.cpu, 2);
    assert_eq!(current.state, firecrab_api_types::VmState::Created);
    assert_eq!(current.security_group_id, Some(group.id));
    assert_eq!(current.env["MARKER"], "DO-NOT-PRINT-APPLY");
    assert!(
        !serde_json::to_string(&status)
            .unwrap()
            .contains("DO-NOT-PRINT")
    );
    let conn = rusqlite::Connection::open(root.path().join("state.db")).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT status FROM stack_applies WHERE operation_id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!raw.contains("DO-NOT-PRINT"));
    assert!(!raw.contains("microVMs"));
    stack.as_object_mut().unwrap().remove("microVMs");
    let retain_id = Uuid::new_v4();
    let body = planned(&router, &stack, retain_id).await;
    request(&router, "POST", "/api/stacks/demo/apply", body).await;
    let retained = terminal(&router, retain_id).await;
    assert_eq!(retained.state, ApplyState::Succeeded);
    assert!(
        retained
            .changes
            .iter()
            .any(|s| s.change.action == Action::Retain && s.change.resource_id == Some(vm_id))
    );
    assert_eq!(state.store.load_all().unwrap()[&vm_id].cpu, 2);
    assert_eq!(state.store.stack("demo").unwrap().unwrap().revision, 2);
}

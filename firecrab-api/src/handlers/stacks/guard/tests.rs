use super::*;
use axum::Router;
use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use firecrab_stack::ResourceKind;
use firecrab_stack::{ApplyState, ApplyStatus, Binding};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::server::{HttpConfig, build_router};
use crate::templates::TemplateRegistry;

async fn fixture(root: &std::path::Path) -> (AppState, Router, ApplyStatus, Uuid, Uuid) {
    let templates = TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
    let state = AppState::with_db_file(templates, root.join("state.db"))
        .await
        .unwrap();
    let store = &state.store;
    store.register_stack("demo").unwrap();
    let network = Uuid::new_v4();
    store
        .insert_micro_network(
            &serde_json::from_value(json!({
                "id":network,"name":"owned","subnetCidr":"172.31.20.0/24",
                "gateway":"172.31.20.1","internetEnabled":true
            }))
            .unwrap(),
        )
        .unwrap();
    let record = store
        .bind_stack_resource(
            "demo",
            &Binding {
                kind: ResourceKind::Network,
                key: "app".into(),
                resource_id: network,
            },
        )
        .unwrap();
    let vm = Uuid::new_v4();
    let replacement = Uuid::new_v4();
    let status: ApplyStatus = serde_json::from_value(json!({
        "operationId":Uuid::new_v4(),"stackId":record.id,"name":"demo","state":"queued",
        "lifecycle":{"replace":true},
        "changes":[
            {"kind":"network","key":"app","resourceId":network,"action":"delete","fields":[],"reason":null,"state":"pending"},
            {"kind":"network","key":"app","resourceId":replacement,"action":"create","fields":[],"reason":null,"state":"pending","replacementFor":network},
            {"kind":"vm","key":"worker","resourceId":vm,"action":"create","fields":[],"reason":null,"state":"pending"}
        ],"error":null
    })).unwrap();
    store
        .begin_stack_apply(&record, "desired-hash", false, &status, None)
        .unwrap();
    let app = build_router(
        state.clone(),
        &HttpConfig {
            bind_addr: "127.0.0.1:5523".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 128,
            request_timeout: std::time::Duration::from_secs(10),
            static_root: None,
        },
    );
    (state, app, status, network, vm)
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    header: Option<Uuid>,
) -> (StatusCode, Value) {
    let mut builder = HttpRequest::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(id) = header {
        builder = builder
            .header("x-request-id", id.to_string())
            .header("idempotency-key", id.to_string());
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from("{}")).unwrap())
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

#[tokio::test]
async fn stack_guard_blocks_bound_and_reserved_resources_but_not_observers_or_unrelated_vms() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, status, network, vm) = fixture(root.path()).await;
    let replacement = status
        .changes
        .iter()
        .find(|step| step.replacement_for.is_some())
        .unwrap()
        .change
        .resource_id
        .unwrap();
    assert!(
        !state
            .store
            .stack("demo")
            .unwrap()
            .unwrap()
            .bindings
            .iter()
            .any(|binding| binding.resource_id == replacement)
    );
    for (method, path) in [
        ("PATCH", format!("/api/micro-networks/{network}")),
        ("DELETE", format!("/api/micro-networks/{network}")),
        ("PATCH", format!("/api/micro-networks/{replacement}")),
        ("PUT", format!("/api/vms/{vm}")),
        ("POST", format!("/api/vms/{vm}/start")),
        ("POST", format!("/api/vms/{vm}/stop")),
        ("PUT", format!("/api/vms/{vm}/port-forwards")),
    ] {
        let (code, body) = request(&app, method, &path, Some(status.operation_id)).await;
        assert_eq!(code, StatusCode::CONFLICT, "{path}: {body}");
        assert_eq!(body["error"]["code"], "stack_apply_busy", "{body}");
    }
    assert!(
        state
            .store
            .micro_network(network)
            .unwrap()
            .unwrap()
            .internet_enabled
    );
    assert_eq!(
        request(&app, "GET", "/api/micro-networks", None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "GET",
            &format!("/api/stacks/demo/applies/{}", status.operation_id),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "POST",
            &format!("/api/vms/{}/start", Uuid::new_v4()),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn stack_guard_decodes_resource_ids_and_releases_terminal_operations() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, mut status, network, vm) = fixture(root.path()).await;
    let encoded = vm.to_string().replace('-', "%2D");
    assert_eq!(
        request(&app, "POST", &format!("/api/vms/{encoded}/start"), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    status.state = ApplyState::Running;
    state.store.save_stack_apply(&status).unwrap();
    assert_eq!(
        request(
            &app,
            "DELETE",
            &format!("/api/micro-networks/{network}"),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    status.state = ApplyState::Interrupted;
    state.store.save_stack_apply(&status).unwrap();
    assert!(
        !state
            .store
            .stack_resource_apply_busy(ResourceKind::Network, network)
            .unwrap()
    );
    assert!(
        !state
            .store
            .stack_resource_apply_busy(ResourceKind::Vm, vm)
            .unwrap()
    );
    assert_eq!(
        request(&app, "POST", &format!("/api/vms/{vm}/start"), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn stack_guard_waits_for_admission_and_then_rechecks_the_durable_owner() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _status, _network, vm) = fixture(root.path()).await;
    let admission = state.stack_apply_admission.lock().await;
    let mut task =
        tokio::spawn(
            async move { request(&app, "POST", &format!("/api/vms/{vm}/start"), None).await },
        );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut task)
            .await
            .is_err()
    );
    drop(admission);
    assert_eq!(task.await.unwrap().0, StatusCode::CONFLICT);
}

#[tokio::test]
async fn stack_guard_keeps_http_deadline_while_waiting_for_admission() {
    let root = tempfile::tempdir().unwrap();
    let (state, _app, _status, network, _vm) = fixture(root.path()).await;
    let app = build_router(
        state.clone(),
        &HttpConfig {
            bind_addr: "127.0.0.1:5523".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 128,
            request_timeout: std::time::Duration::from_millis(20),
            static_root: None,
        },
    );
    let admission = state.stack_apply_admission.lock().await;
    let (code, body) = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        request(
            &app,
            "DELETE",
            &format!("/api/micro-networks/{network}"),
            None,
        ),
    )
    .await
    .unwrap();
    assert_eq!(code, StatusCode::GATEWAY_TIMEOUT, "{body}");
    assert!(state.store.micro_network(network).unwrap().is_some());
    drop(admission);
}

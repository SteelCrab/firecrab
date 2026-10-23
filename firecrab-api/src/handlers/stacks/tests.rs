use axum::Router;
use axum::body::Body;
use axum::http::Request;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;
use crate::server::{HttpConfig, build_router};
use crate::templates::TemplateRegistry;

async fn app(root: &std::path::Path) -> (Router, Store) {
    let templates = TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
    let state = AppState::with_db_file(templates, root.join("state.db"))
        .await
        .unwrap();
    let store = state.store.clone();
    let config = HttpConfig {
        bind_addr: "127.0.0.1:3000".parse().unwrap(),
        allowed_origins: Vec::new(),
        max_concurrent_requests: 128,
        request_timeout: std::time::Duration::from_secs(10),
        static_root: None,
    };
    (build_router(state, &config), store)
}

async fn request(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
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
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap()
    };
    (status, value)
}

#[tokio::test]
async fn stacks_api_delete_requires_empty_bindings_and_the_exact_registration() {
    let root = tempfile::tempdir().unwrap();
    let (router, store) = app(root.path()).await;
    let stack = store.register_stack("demo").unwrap();
    let exact = format!("/api/stacks/demo?id={}", stack.id);
    let wrong = format!("/api/stacks/demo?id={}", Uuid::new_v4());
    assert_eq!(
        request(&router, "DELETE", &wrong, Value::Null).await.0,
        StatusCode::CONFLICT
    );
    let network: firecrab_api_types::MicroNetworkResponse = serde_json::from_value(json!({"id":Uuid::new_v4(),"name":"owned","subnetCidr":"172.31.20.0/24","gateway":"172.31.20.1","internetEnabled":true})).unwrap();
    store.insert_micro_network(&network).unwrap();
    let binding = Binding {
        kind: firecrab_stack::ResourceKind::Network,
        key: "app".to_owned(),
        resource_id: network.id,
    };
    store.bind_stack_resource("demo", &binding).unwrap();
    assert_eq!(
        request(&router, "DELETE", &exact, Value::Null).await.0,
        StatusCode::CONFLICT
    );
    store.unbind_stack_resource("demo", &binding).unwrap();
    assert_eq!(
        request(&router, "DELETE", &exact, Value::Null).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&router, "GET", "/api/stacks/demo", Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(store.micro_network(network.id).unwrap(), Some(network));
    let newer = store.register_stack("demo").unwrap();
    assert_ne!(newer.id, stack.id);
    assert_eq!(
        request(&router, "DELETE", &exact, Value::Null).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(store.stack("demo").unwrap(), Some(newer));
}

fn declaration() -> Value {
    json!({"apiVersion":"firecrab.dev/v1alpha1","kind":"Stack","name":"demo","microNetworks":{"app":{"name":"demo-app","subnetCidr":"172.31.20.0/24"}}})
}

#[tokio::test]
async fn stacks_api_registration_and_ownership_survive_restart_without_resource_changes() {
    let root = tempfile::tempdir().unwrap();
    let (router, store) = app(root.path()).await;
    let (status, registered) =
        request(&router, "POST", "/api/stacks", json!({"name":"demo"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        registered,
        request(&router, "POST", "/api/stacks", json!({"name":"demo"}))
            .await
            .1
    );
    let network: firecrab_api_types::MicroNetworkResponse=serde_json::from_value(json!({"id":Uuid::new_v4(),"name":"demo-app","subnetCidr":"172.31.20.0/24","gateway":"172.31.20.1","internetEnabled":true})).unwrap();
    store.insert_micro_network(&network).unwrap();
    let ownership = json!({"kind":"network","key":"app","resourceId":network.id});
    let (status, claimed) = request(
        &router,
        "POST",
        "/api/stacks/demo/bindings",
        ownership.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    assert_eq!(claimed["revision"], 1);
    assert_eq!(
        claimed,
        request(
            &router,
            "POST",
            "/api/stacks/demo/bindings",
            ownership.clone()
        )
        .await
        .1
    );
    let before = store.list_micro_networks().unwrap();
    let (status, plan) = request(&router, "POST", "/api/stacks/demo/plan", declaration()).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!(plan["changes"][0]["action"], "noop");
    assert_eq!(store.list_micro_networks().unwrap(), before);
    assert_eq!(store.stack("demo").unwrap().unwrap().revision, 1);
    drop(router);
    drop(store);
    let (router, store) = app(root.path()).await;
    assert_eq!(
        request(&router, "GET", "/api/stacks/demo", Value::Null)
            .await
            .1,
        claimed
    );
    assert_eq!(
        request(&router, "GET", "/api/stacks", Value::Null).await.1,
        json!([claimed])
    );
    assert_eq!(
        request(&router, "POST", "/api/stacks/demo/unbind", ownership)
            .await
            .1["revision"],
        2
    );
    assert_eq!(store.list_micro_networks().unwrap(), before);
}

#[tokio::test]
async fn stacks_api_plan_never_registers_or_adopts_resources_implicitly() {
    let root = tempfile::tempdir().unwrap();
    let (router, store) = app(root.path()).await;
    assert_eq!(
        request(&router, "POST", "/api/stacks/demo/plan", declaration())
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert!(store.list_stacks().unwrap().is_empty());
    request(&router, "POST", "/api/stacks", json!({"name":"demo"})).await;
    let (_, plan) = request(&router, "POST", "/api/stacks/demo/plan", declaration()).await;
    assert_eq!(plan["changes"][0]["action"], "create");
    assert!(store.list_micro_networks().unwrap().is_empty());
    let network=serde_json::from_value(json!({"id":Uuid::new_v4(),"name":"demo-app","subnetCidr":"172.31.20.0/24","gateway":"172.31.20.1","internetEnabled":true})).unwrap();
    store.insert_micro_network(&network).unwrap();
    let (_, plan) = request(&router, "POST", "/api/stacks/demo/plan", declaration()).await;
    assert_eq!(plan["blocked"], true);
    assert_eq!(plan["changes"][0]["action"], "blocked");
    assert!(store.stack("demo").unwrap().unwrap().bindings.is_empty());
}

#[tokio::test]
async fn stacks_api_rejects_invalid_input_and_conflicting_claims_without_values() {
    let root = tempfile::tempdir().unwrap();
    let (router, store) = app(root.path()).await;
    assert_eq!(
        request(&router, "POST", "/api/stacks", json!({"name":"Uppercase"}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &router,
            "POST",
            "/api/stacks",
            json!({"name":"demo","env":"DO-NOT-PRINT"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    request(&router, "POST", "/api/stacks", json!({"name":"demo"})).await;
    request(&router, "POST", "/api/stacks", json!({"name":"other"})).await;
    let id = Uuid::new_v4();
    let ownership = json!({"kind":"network","key":"app","resourceId":id});
    assert_eq!(
        request(
            &router,
            "POST",
            "/api/stacks/demo/bindings",
            ownership.clone()
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let network=serde_json::from_value(json!({"id":id,"name":"demo-app","subnetCidr":"172.31.20.0/24","gateway":"172.31.20.1","internetEnabled":true})).unwrap();
    store.insert_micro_network(&network).unwrap();
    assert_eq!(
        request(
            &router,
            "POST",
            "/api/stacks/demo/bindings",
            ownership.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&router, "POST", "/api/stacks/other/bindings", ownership)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let mut invalid = declaration();
    invalid["microVMs"] = json!({"worker":{"name":"worker","template":"alpine","network":"app","cpu":0,"ram":256,"diskGb":2,"env":{"TOKEN":"DO-NOT-PRINT"}}});
    let (status, error) = request(&router, "POST", "/api/stacks/demo/plan", invalid).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!error.to_string().contains("DO-NOT-PRINT"));
    assert_eq!(store.stack("demo").unwrap().unwrap().revision, 1);
}

#[tokio::test]
async fn stacks_api_redacts_a_valid_service_diff_and_keeps_saved_configuration() {
    let root = tempfile::tempdir().unwrap();
    let (router, store) = app(root.path()).await;
    store.register_stack("demo").unwrap();
    let image = format!("ghcr.io/example/web@sha256:{}", "a".repeat(64));
    let spec=serde_json::from_value(json!({"name":"web","image":image,"port":8080,"cpu":1,"ram":256,"env":{"TOKEN":"actual-DO-NOT-PRINT"}})).unwrap();
    let service = firecrab_api_types::MicroServiceResponse {
        id: Uuid::new_v4(),
        spec,
    };
    store.insert_micro_service(&service).unwrap();
    store
        .bind_stack_resource(
            "demo",
            &Binding {
                kind: firecrab_stack::ResourceKind::Service,
                key: "web".to_owned(),
                resource_id: service.id,
            },
        )
        .unwrap();
    let mut desired = declaration();
    desired["microServices"] = json!({"web":{"network":"app","hostPort":30100,"config":{"name":"web","image":image,"port":8080,"cpu":1,"ram":256,"env":{"TOKEN":"desired-DO-NOT-PRINT"}}}});
    let (status, plan) = request(&router, "POST", "/api/stacks/demo/plan", desired).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert!(!plan.to_string().contains("DO-NOT-PRINT"));
    assert_eq!(plan["changes"][1]["action"], "update");
    assert_eq!(store.micro_service(service.id).unwrap(), Some(service));
    assert_eq!(store.stack("demo").unwrap().unwrap().revision, 1);
}

#[tokio::test]
async fn stacks_api_plan_name_is_not_reserved_and_url_identity_must_match() {
    let root = tempfile::tempdir().unwrap();
    let (router, store) = app(root.path()).await;
    assert_eq!(
        request(&router, "POST", "/api/stacks", json!({"name":"plan"}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&router, "GET", "/api/stacks/plan", Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&router, "POST", "/api/stacks/plan/plan", declaration())
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut desired = declaration();
    desired["name"] = json!("plan");
    assert_eq!(
        request(&router, "POST", "/api/stacks/plan/plan", desired)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(store.stack("plan").unwrap().unwrap().revision, 0);
}

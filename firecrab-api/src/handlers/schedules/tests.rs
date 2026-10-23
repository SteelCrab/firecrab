use super::*;
use crate::server::{HttpConfig, build_router};
use crate::templates::TemplateRegistry;
use axum::{Router, body::Body, http::Request};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn app(root: &std::path::Path) -> (Router, AppState, Uuid) {
    let templates = TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
    let mut state = AppState::with_db_file(templates, root.join("test.db"))
        .await
        .unwrap();
    state.network =
        crate::network::NetworkClient::with_socket_path(root.join("unused-helper.sock"));
    let id = Uuid::new_v4();
    let vm = serde_json::from_value(json!({"id":id,"name":"test","state":"created","template":"unused","cpu":1,"ram":128,"micro_network_id":Uuid::new_v4()})).unwrap();
    state.vms.lock().unwrap().insert(id, vm);
    let config = HttpConfig {
        bind_addr: "127.0.0.1:5523".parse().unwrap(),
        allowed_origins: vec![],
        max_concurrent_requests: 128,
        request_timeout: std::time::Duration::from_secs(5),
        static_root: None,
    };
    (build_router(state.clone(), &config), state, id)
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
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
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

fn spec(vm: Uuid) -> Value {
    json!({"name":"weekday-start","vmId":vm,"action":"start","timing":{"kind":"cron","expression":"0 9 * * Mon-Fri","timeZone":"Asia/Seoul"}})
}

#[tokio::test]
async fn crud_enable_history_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let (router, state, vm) = app(root.path()).await;
    let (status, created) = request(&router, "POST", "/api/schedules", spec(vm)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["enabled"], true);
    let id = created["id"].as_str().unwrap();
    let path = format!("/api/schedules/{id}");
    assert_eq!(request(&router, "GET", &path, Value::Null).await.1, created);
    let reopened = crate::persistence::Store::open(&root.path().join("test.db")).unwrap();
    assert_eq!(
        reopened
            .schedule(id.parse().unwrap())
            .unwrap()
            .unwrap()
            .spec
            .name,
        "weekday-start"
    );
    assert_eq!(
        request(&router, "GET", &format!("{path}/runs"), Value::Null)
            .await
            .1,
        json!([])
    );
    assert_eq!(
        request(
            &router,
            "PUT",
            &format!("{path}/enabled"),
            json!({"enabled":false})
        )
        .await
        .1["nextRunAtMs"],
        Value::Null
    );
    let (status, enabled) = request(
        &router,
        "PUT",
        &format!("{path}/enabled"),
        json!({"enabled":true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(enabled["nextRunAtMs"].is_i64());
    let mut changed = spec(vm);
    changed["action"] = json!("stop");
    changed["name"] = json!("weekday-stop");
    changed["timing"]["expression"] = json!("0 18 * * Mon-Fri");
    assert_eq!(
        request(&router, "PUT", &path, changed).await.1["action"],
        "stop"
    );
    assert_eq!(state.store.list_schedules().unwrap().len(), 1);
    assert_eq!(
        request(&router, "DELETE", &path, Value::Null).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&router, "GET", &path, Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn validation_rejects_bad_timing_and_unknown_targets_without_writes() {
    let root = tempfile::tempdir().unwrap();
    let (router, state, vm) = app(root.path()).await;
    let mut invalid = vec![spec(Uuid::new_v4())];
    for timing in [
        json!({"kind":"once","at":"2020-01-01T00:00:00Z"}),
        json!({"kind":"once","at":"2030-01-01T00:00:00"}),
        json!({"kind":"cron","expression":"* * * * * *","timeZone":"UTC"}),
        json!({"kind":"cron","expression":"0 9 * * *","timeZone":"Unknown"}),
    ] {
        let mut body = spec(vm);
        body["timing"] = timing;
        invalid.push(body);
    }
    for body in invalid {
        assert_eq!(
            request(&router, "POST", "/api/schedules", body).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert!(state.store.list_schedules().unwrap().is_empty());
}

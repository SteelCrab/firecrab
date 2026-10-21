use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;
use crate::server::{HttpConfig, build_router};
use crate::templates::TemplateRegistry;

async fn app(root: &std::path::Path) -> Router {
    let templates = TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
    let state = AppState::with_db_file(templates, root.join("state.db"))
        .await
        .unwrap();
    let config = HttpConfig {
        bind_addr: "127.0.0.1:3000".parse().unwrap(),
        allowed_origins: vec![],
        max_concurrent_requests: 128,
        request_timeout: std::time::Duration::from_secs(10),
        static_root: None,
    };
    build_router(state, &config)
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
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, json)
}

fn minimal(name: &str) -> Value {
    json!({"name": name, "image": "ghcr.io/team/orders:1.0", "port": 8080, "ram": 512, "cpu": 1})
}

#[tokio::test]
async fn micro_services_crud_survives_restart_and_clears_optional_fields() {
    let directory = tempfile::tempdir().unwrap();
    let router = app(directory.path()).await;
    let mut spec = minimal("orders");
    let secret = Uuid::new_v4();
    spec["healthPath"] = json!("/actuator/health");
    spec["domain"] = json!("orders.example.com");
    spec["replicas"] = json!(3);
    spec["startupTimeout"] = json!(90);
    spec["drainTimeout"] = json!(45);
    spec["env"] = json!({"SPRING_PROFILES_ACTIVE": "prod", "EMPTY": ""});
    spec["secrets"] = json!({"DB_PASSWORD": secret});
    let (status, created) = request(&router, "POST", "/api/micro-services", spec.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["runtime"], "vm");
    assert_eq!(created["secrets"]["DB_PASSWORD"], secret.to_string());
    assert!(created.get("spec").is_none());
    let typed: MicroServiceResponse = serde_json::from_value(created.clone()).unwrap();
    assert_eq!(serde_json::to_value(typed).unwrap(), created);
    let path = format!("/api/micro-services/{}", created["id"].as_str().unwrap());
    drop(router);
    let router = app(directory.path()).await;
    assert_eq!(
        request(&router, "GET", &path, Value::Null).await,
        (StatusCode::OK, created.clone())
    );
    assert_eq!(
        request(&router, "GET", "/api/micro-services", Value::Null)
            .await
            .1,
        json!([created])
    );

    let mut replacement = minimal("orders-v2");
    replacement["image"] = json!(format!("ghcr.io/team/orders@sha256:{}", "a".repeat(64)));
    replacement["drainTimeout"] = json!(0);
    let (status, updated) = request(&router, "PUT", &path, replacement).await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["env"], json!({}));
    assert_eq!(updated["secrets"], json!({}));
    assert!(updated["domain"].is_null());
    assert_eq!(updated["healthPath"], "/health");
    assert_eq!(updated["startupTimeout"], 300);
    assert_eq!(updated["replicas"], 1);
    assert_eq!(
        request(&router, "DELETE", &path, Value::Null).await.0,
        StatusCode::NO_CONTENT
    );
    drop(router);
    let router = app(directory.path()).await;
    for method in ["GET", "PUT", "DELETE"] {
        assert_eq!(
            request(&router, method, &path, minimal("orders")).await.0,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        request(&router, "GET", "/api/micro-services", Value::Null)
            .await
            .1,
        json!([])
    );
}

#[tokio::test]
async fn micro_services_duplicate_and_invalid_updates_leave_previous_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let router = app(directory.path()).await;
    let (first, second) = tokio::join!(
        request(&router, "POST", "/api/micro-services", minimal("orders")),
        request(&router, "POST", "/api/micro-services", minimal("orders")),
    );
    assert!(
        (first.0 == StatusCode::CREATED && second.0 == StatusCode::BAD_REQUEST)
            || (second.0 == StatusCode::CREATED && first.0 == StatusCode::BAD_REQUEST)
    );
    let (_, billing) = request(&router, "POST", "/api/micro-services", minimal("billing")).await;
    let path = format!("/api/micro-services/{}", billing["id"].as_str().unwrap());
    let (status, error) = request(&router, "PUT", &path, minimal("orders")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["fields"]["name"], "is already registered");
    let mut invalid = minimal("billing");
    invalid["cpu"] = json!(0);
    assert_eq!(
        request(&router, "PUT", &path, invalid).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(request(&router, "GET", &path, Value::Null).await.1, billing);
    let rows = request(&router, "GET", "/api/micro-services", Value::Null)
        .await
        .1;
    assert_eq!(rows[0]["name"], "billing");
    assert_eq!(rows[1]["name"], "orders");
}

#[tokio::test]
async fn micro_services_reject_invalid_fields_and_secret_value_ambiguity() {
    let directory = tempfile::tempdir().unwrap();
    let router = app(directory.path()).await;
    let cases = [
        ("name", json!("bad_name")),
        ("name", json!("-orders")),
        ("name", json!("Orders")),
        ("name", json!("a".repeat(64))),
        ("image", json!("ghcr.io/team/orders:bad tag")),
        ("image", json!("ghcr.io/team/orders@sha256:abc")),
        ("port", json!(0)),
        ("ram", json!(513)),
        ("ram", json!(65536)),
        ("cpu", json!(33)),
        ("replicas", json!(0)),
        ("replicas", json!(101)),
        ("startupTimeout", json!(0)),
        ("startupTimeout", json!(3601)),
        ("drainTimeout", json!(3601)),
        ("healthPath", json!("http://host/health")),
        ("healthPath", json!("//host/health")),
        ("healthPath", json!("/health#fragment")),
        ("healthPath", json!("/health\n")),
        ("domain", json!("https://orders.example.com")),
        ("domain", json!("")),
        ("domain", json!("orders.example.com:443")),
        ("domain", json!("*.example.com")),
        ("env", json!({"BAD-NAME": "value"})),
        ("env", json!({"OK": "nul\0value"})),
        ("secrets", json!({"BAD-NAME": Uuid::new_v4()})),
        ("secrets", json!({"DB_PASSWORD": Uuid::nil()})),
    ];
    for (field, value) in cases {
        let mut spec = minimal("orders");
        spec[field] = value;
        let (status, error) = request(&router, "POST", "/api/micro-services", spec).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{field}: {error}");
        assert!(
            error["error"]["fields"].get(field).is_some(),
            "{field}: {error}"
        );
    }
    let mut spec = minimal("orders");
    spec["env"] = json!({"DB_PASSWORD": "value"});
    spec["secrets"] = json!({"DB_PASSWORD": Uuid::new_v4()});
    assert_eq!(
        request(&router, "POST", "/api/micro-services", spec)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut spec = minimal("orders");
    spec["env"] = serde_json::to_value(
        (0..64)
            .map(|i| (format!("VAR_{i}"), "x"))
            .collect::<BTreeMap<_, _>>(),
    )
    .unwrap();
    spec["secrets"] = json!({"DB_PASSWORD": Uuid::new_v4()});
    assert_eq!(
        request(&router, "POST", "/api/micro-services", spec)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(&router, "GET", "/api/micro-services", Value::Null)
            .await
            .1,
        json!([])
    );
}

#[tokio::test]
async fn micro_services_reject_unknown_fields_runtime_and_malformed_ids() {
    let directory = tempfile::tempdir().unwrap();
    let router = app(directory.path()).await;
    for (field, value) in [
        ("runtime", json!("container")),
        ("secrets", json!({"DB_PASSWORD": "secret-value"})),
        ("startupTimout", json!(123)),
    ] {
        let mut spec = minimal("orders");
        spec[field] = value;
        let (status, error) = request(&router, "POST", "/api/micro-services", spec).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error"]["code"], "invalid_json");
    }
    assert_eq!(
        request(&router, "GET", "/api/micro-services/bad-id", Value::Null)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

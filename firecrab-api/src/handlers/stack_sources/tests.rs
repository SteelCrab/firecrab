use super::*;
use crate::{
    server::{HttpConfig, build_router},
    templates::TemplateRegistry,
};
use axum::{Router, body::Body, http::Request};
use serde_json::{Value, json};
use tower::ServiceExt;

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
    let code = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (
        code,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}

#[tokio::test]
async fn source_api_guards_generations_and_records_missing_credentials_without_network_mutations() {
    let root = tempfile::tempdir().unwrap();
    let templates = TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap();
    let state = AppState::with_db_file(templates, root.path().join("state.db"))
        .await
        .unwrap();
    state.store.register_stack("demo").unwrap();
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
    let config = json!({"expectedGeneration":0,"source":{"repository":"https://github.com/example/infra","ref":"main","manifestPath":"stack.yaml","credentialRef":"github","paused":true}});
    assert_eq!(
        request(&app, "GET", "/api/stacks/demo/source", Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (code, status) = request(&app, "PUT", "/api/stacks/demo/source", config.clone()).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(status["generation"], 1);
    assert_eq!(
        request(&app, "PUT", "/api/stacks/demo/source", config.clone())
            .await
            .0,
        StatusCode::CONFLICT
    );
    let mut invalid = config.clone();
    invalid["source"]["repository"] = "https://secret@github.com/example/infra".into();
    let (code, error) = request(&app, "PUT", "/api/stacks/demo/source", invalid).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert!(!error.to_string().contains("secret@"));
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/stacks/demo/source/check",
            json!({"generation":0})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/stacks/demo/source/check",
            json!({"generation":1})
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status = state.store.stack_source("demo").unwrap();
            if status.latest.as_ref().unwrap().state != firecrab_stack::GitCheckState::Checking {
                assert_eq!(
                    status.latest.unwrap().error.as_deref(),
                    Some("credential_unavailable")
                );
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (code, history) =
        request(&app, "GET", "/api/stacks/demo/source/history", Value::Null).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(history.as_array().unwrap().len(), 1);
    assert_eq!(history[0]["state"], "failed");
    assert!(state.store.load_all().unwrap().is_empty());
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
}

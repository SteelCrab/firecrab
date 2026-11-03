use super::*;
use crate::{
    ai_sandbox::access::{ControllerIdentity, LocalOperator},
    server::{HttpConfig, build_router},
    tls::AuthenticatedHost,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::time::Duration;
use tower::ServiceExt;

fn router(state: &AppState) -> Router {
    build_router(
        state.clone(),
        &HttpConfig {
            bind_addr: "127.0.0.1:5523".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 32,
            request_timeout: Duration::from_secs(10),
            static_root: None,
        },
    )
}

async fn send(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 128 * 1024).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn iac_resource_plan_http_uses_verified_operator_identity_and_returns_no_supplied_values() {
    let root = tempfile::tempdir().unwrap();
    let templates =
        crate::templates::TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap();
    let state = AppState::with_db_file(templates, root.path().join("state.db"))
        .await
        .unwrap();
    let base = router(&state);
    let operator = base.clone().layer(Extension(LocalOperator));
    let endpoint = state.store.iac_endpoint_id().unwrap();
    let id = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let input = json!({"operationId":id,"expectedEndpointId":endpoint,"owner":{"manager":"terraform","id":owner,"key":"firecrab_shell.shared"},
        "resource":null,"desired":{"kind":"shell","configuration":{"name":"shared","content":"echo private-script-canary","description":"private-description-canary"}}});
    let (status, _) = send(&base, "POST", "/api/iac/resource-plans", input.clone()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let delegated = base
        .clone()
        .layer(Extension(ConnectInfo(AuthenticatedHost {
            host_id: Uuid::new_v4(),
        })))
        .layer(Extension(ControllerIdentity(Uuid::new_v4())));
    let (status, _) = send(&delegated, "POST", "/api/iac/resource-plans", input.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let mut invalid = input.clone();
    invalid["desired"]["kind"] = "private-invalid-kind-canary".into();
    let (status, error) = send(&operator, "POST", "/api/iac/resource-plans", invalid).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!error.to_string().contains("private-invalid-kind-canary"));
    assert!(state.store.list_shells().unwrap().is_empty());
    let (status, plan) = send(&operator, "POST", "/api/iac/resource-plans", input.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(plan["action"], "create");
    assert!(
        !plan.to_string().contains("private-script-canary")
            && !plan.to_string().contains("private-description-canary")
    );
    let apply = json!({"expectedEndpointId":endpoint,"planSha256":plan["planSha256"]});
    let path = format!("/api/iac/resource-plans/{id}/apply");
    let (status, receipt) = send(&operator, "POST", &path, apply.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(receipt["ownership"]["owner"]["id"], owner.to_string());
    assert_eq!(
        send(&operator, "POST", &path, apply).await,
        (StatusCode::OK, receipt.clone())
    );
    assert_eq!(
        send(
            &operator,
            "GET",
            &format!("/api/iac/resource-plans/{id}/receipt"),
            Value::Null
        )
        .await,
        (StatusCode::OK, receipt.clone())
    );
    assert_eq!(state.store.list_shells().unwrap().len(), 1);
    let (status, identity) = send(&operator, "GET", "/api/iac/identity", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(identity["resourcePlanKinds"], json!(["shell"]));
    let target = receipt["ownership"]["resource"]["id"].as_str().unwrap();
    let (status, _) = send(
        &operator,
        "POST",
        &format!("/api/shells/{target}/revisions"),
        json!({"content":"echo forbidden"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = send(
        &operator,
        "DELETE",
        &format!("/api/shells/{target}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

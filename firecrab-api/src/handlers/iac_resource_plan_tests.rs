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

#[tokio::test]
async fn iac_resource_plan_http_cancellation_retains_original_worker_while_waiting_for_admission() {
    let root = tempfile::tempdir().unwrap();
    let templates =
        crate::templates::TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap();
    let state = AppState::with_db_file(templates, root.path().join("state.db"))
        .await
        .unwrap();
    let app = router(&state).layer(Extension(LocalOperator));
    let input:PlanResource=serde_json::from_value(json!({"operationId":Uuid::new_v4(),"expectedEndpointId":state.store.iac_endpoint_id().unwrap(),
        "owner":{"manager":"terraform","id":Uuid::new_v4(),"key":"firecrab_shell.detached"},"resource":null,
        "desired":{"kind":"shell","configuration":{"name":"detached","content":"echo ready"}}})).unwrap();
    let plan = state
        .store
        .plan_resource(input, crate::secrets::now_ms())
        .unwrap();
    let body =
        json!({"expectedEndpointId":plan.ownership.endpoint_id,"planSha256":plan.plan_sha256});
    let held = state.pipe_ai_mutations.lock().await;
    let request = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/iac/resource-plans/{}/apply",
            plan.operation_id
        ))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let client = tokio::spawn(app.clone().oneshot(request));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if state.stack_apply_admission.try_lock().is_err() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "worker did not acquire admission"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        state
            .store
            .resource_plan_receipt(plan.operation_id)
            .unwrap()
            .is_none()
    );
    client.abort();
    assert!(client.await.unwrap_err().is_cancelled());
    drop(held);
    let receipt = loop {
        if let Some(receipt) = state
            .store
            .resource_plan_receipt(plan.operation_id)
            .unwrap()
        {
            break receipt;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "original worker was cancelled with HTTP waiter"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert_eq!(state.store.list_shells().unwrap().len(), 1);
    assert_eq!(
        send(
            &app,
            "POST",
            &format!("/api/iac/resource-plans/{}/apply", plan.operation_id),
            body
        )
        .await,
        (StatusCode::OK, serde_json::to_value(receipt).unwrap())
    );
}

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

#[tokio::test]
async fn iac_resource_model_http_applies_verified_reference_and_removes_only_the_default() {
    let root = tempfile::tempdir().unwrap();
    let templates =
        crate::templates::TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap();
    let state = AppState::with_db_file(templates, root.path().join("state.db"))
        .await
        .unwrap();
    let app = router(&state).layer(Extension(LocalOperator));
    let connection = Uuid::new_v4();
    let project = Uuid::new_v4();
    let endpoint = state.store.iac_endpoint_id().unwrap();
    let source:crate::pipe_ai::SavedConnection=serde_json::from_value(json!({
        "connection":{"id":connection,"name":"local-fixture","provider":"openai","authentication":"api_key",
            "endpoint":"https://api.openai.com/v1/responses","models":["fixture-verified"],"streaming":true,"toolCalls":true},
        "api_key":"private-http-model-canary","oauth":null})).unwrap();
    state.store.save_ai_connection(&source).unwrap();
    let mut input = json!({"operationId":Uuid::new_v4(),"expectedEndpointId":endpoint,
        "owner":{"manager":"terraform","id":Uuid::new_v4(),"key":"firecrab_ai_model.project"},"resource":null,
        "desired":{"kind":"ai_model","configuration":{"projectId":project,"selection":{"connectionId":connection,"model":"fixture-verified"}}}});
    let (status, plan) = send(&app, "POST", "/api/iac/resource-plans", input.clone()).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!(plan["ownership"]["resource"]["id"], project.to_string());
    assert_eq!(plan["dependencies"][0]["configurationGeneration"], 1);
    let original = plan["operationId"].as_str().unwrap();
    let command = json!({"expectedEndpointId":endpoint,"planSha256":plan["planSha256"]});
    let (status, created) = send(
        &app,
        "POST",
        &format!("/api/iac/resource-plans/{original}/apply"),
        command.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert!(!created.to_string().contains("private-http-model-canary"));
    assert_eq!(
        state.store.ai_default(project).unwrap().unwrap().model,
        "fixture-verified"
    );
    input["resource"] = json!({"kind":"ai_model","id":project});
    input["operationId"] = json!(Uuid::new_v4());
    let (status, noop) = send(&app, "POST", "/api/iac/resource-plans", input.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(noop["action"], "noop");
    input["operationId"] = json!(Uuid::new_v4());
    input["desired"] = Value::Null;
    let (status, deletion) = send(&app, "POST", "/api/iac/resource-plans", input).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deletion["dependencies"], Value::Null);
    let (status, deleted) = send(
        &app,
        "POST",
        &format!(
            "/api/iac/resource-plans/{}/apply",
            deletion["operationId"].as_str().unwrap()
        ),
        json!({"expectedEndpointId":endpoint,"planSha256":deletion["planSha256"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    assert!(state.store.ai_default(project).unwrap().is_none());
    assert_eq!(
        state
            .store
            .ai_connection(connection)
            .unwrap()
            .unwrap()
            .api_key,
        source.api_key
    );
    state.store.delete_ai_connection(connection).unwrap();
    assert_eq!(
        send(
            &app,
            "POST",
            &format!("/api/iac/resource-plans/{original}/apply"),
            command
        )
        .await,
        (StatusCode::OK, created)
    );
    assert!(state.store.ai_default(project).unwrap().is_none());
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
    assert_eq!(identity["resourcePlanKinds"], json!(["shell", "ai_model"]));
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

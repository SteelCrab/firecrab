//! Explicit opt-in only. Never read or print credential values in the test output.
use super::*;
use axum::{
    body::{Body, to_bytes},
    http::StatusCode,
};
use firecrab_api_types::*;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

async fn call(app: &Router, method: &str, path: &str, input: Value) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("host", "127.0.0.1:5523")
                .header("content-type", "application/json")
                .body(Body::from(input.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let doc: Value = serde_json::from_slice(
        &to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        status == StatusCode::OK || status == StatusCode::ACCEPTED,
        "Provider/REST validation failed: {status} {}",
        doc.get("error").unwrap_or(&Value::Null)
    );
    doc
}
async fn finish(app: &Router, id: Uuid) -> Value {
    for _ in 0..150 {
        let value = call(
            app,
            "GET",
            &format!("/api/ai-sandbox/tasks/{id}"),
            Value::Null,
        )
        .await;
        if !matches!(
            value["state"].as_str(),
            Some("waiting" | "accepted" | "in_progress")
        ) {
            assert_eq!(
                value["state"], "result",
                "Live task failed: {}",
                value["errorCode"]
            );
            return value;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    panic!("Live provider deadline exceeded");
}
#[tokio::test]
#[ignore = "requires explicit FIRECRAB_LIVE_CHATGPT_DB; sends real provider requests using the connected account"]
async fn saved_chatgpt_discovers_verifies_native_tools_and_completes_persistent_conversation() {
    let db = std::path::PathBuf::from(
        std::env::var("FIRECRAB_LIVE_CHATGPT_DB")
            .expect("Supply the existing private server DB path, never a token"),
    );
    let templates =
        crate::templates::TemplateRegistry::from_specs(db.parent().unwrap(), std::iter::empty())
            .unwrap();
    let state = AppState::with_db_file(templates, db).await.unwrap();
    let _ownership = state.store.scheduler_lock().unwrap();
    state.store.interrupt_sandbox_tasks().unwrap();
    let connection = state
        .store
        .ai_connections()
        .unwrap()
        .into_iter()
        .find(|c| c.connection.authentication == AiAuthentication::Chatgpt)
        .expect("Connect a ChatGPT account first")
        .connection
        .id;
    let app = build_router(
        state,
        &HttpConfig::from_values("127.0.0.1:5523", "http://localhost:8080").unwrap(),
    )
    .layer(Extension(crate::ai_sandbox::access::LocalOperator));
    let catalogue = call(
        &app,
        "GET",
        &format!("/api/ai-sandbox/connections/{connection}/models?discover=true"),
        Value::Null,
    )
    .await;
    let models = catalogue["models"].as_array().unwrap();
    let model = models
        .iter()
        .find(|m| m["id"] == "gpt-6.1-sol")
        .or_else(|| models.first())
        .expect("No visible models")["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let proof = call(
        &app,
        "POST",
        &format!("/api/ai-sandbox/connections/{connection}/verify"),
        json!({"model":model,"tools":true}),
    )
    .await;
    assert_eq!(proof["toolsVerified"], true);
    let project = Uuid::new_v4();
    let selected = json!({"connectionId":connection,"model":model});
    call(&app,"PUT",&format!("/api/ai-sandbox/projects/{project}"),json!({"name":"AI Sandbox live QA","sourcePath":null,"vmId":null,"selection":selected,"instructions":"Reply concisely."})).await;
    let first = Uuid::new_v4();
    call(
        &app,
        "PUT",
        &format!("/api/ai-sandbox/projects/{project}/tasks/{first}"),
        json!({"kind":"chat","input":"Remember this test marker: CRAB414. Reply OK."}),
    )
    .await;
    finish(&app, first).await;
    let second = Uuid::new_v4();
    call(&app,"PUT",&format!("/api/ai-sandbox/projects/{project}/tasks/{second}"),json!({"kind":"chat","input":"What test marker did I ask you to remember? Reply only with the marker."})).await;
    assert!(
        finish(&app, second).await["result"]
            .as_str()
            .unwrap()
            .contains("CRAB414")
    );
    call(
        &app,
        "DELETE",
        &format!("/api/ai-sandbox/projects/{project}"),
        Value::Null,
    )
    .await;
    eprintln!(
        "Live ChatGPT catalogue, native tool probe and two-turn persistent conversation passed with model {model}. No MicroVM was executed by this check."
    );
}

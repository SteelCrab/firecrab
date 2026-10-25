use super::*;
use axum::{
    Json,
    body::{Body, to_bytes},
    http::StatusCode,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;
use uuid::Uuid;

#[path = "server_ai_sandbox_access_tests.rs"]
mod access_tests;

#[derive(Clone, Default)]
struct Provider {
    requests: Arc<Mutex<Vec<Value>>>,
    replies: Arc<Mutex<VecDeque<Value>>>,
}
struct Fixture {
    dir: tempfile::TempDir,
    state: AppState,
    app: Router,
    provider: Provider,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
fn text(value: &str) -> Value {
    json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":value}]}]})
}
fn tool(id: &str, name: &str, args: Value) -> Value {
    json!({"status":"completed","output":[{"type":"function_call","call_id":id,"name":name,"arguments":args.to_string()}]})
}
async fn fixture() -> Fixture {
    let provider = Provider::default();
    let mock=Router::new().route("/v1/models",get(||async{Json(json!({"object":"list","has_more":false,"data":[{"id":"fixture-model","display_name":"Fixture"}],"models":[{"slug":"fixture-model","display_name":"ChatGPT Fixture","visibility":"list"},{"slug":"hidden","display_name":"Hidden","visibility":"hide"}]}))}))
 .route("/v1/responses",post(|State(p):State<Provider>,Json(body):Json<Value>|async move{
   p.requests.lock().unwrap().push(body.clone());
   if body.to_string().contains("hold-request") {tokio::time::sleep(std::time::Duration::from_secs(2)).await;}
   let doc=p.replies.lock().unwrap().pop_front().unwrap_or_else(||text("fixture reply sandbox-private-key"));
   if body["stream"]==true {
       let mut stream=String::new();
       for (index,item) in doc["output"].as_array().unwrap().iter().enumerate() {
           let mut item=item.clone();if item["type"]=="function_call" { item["namespace"]=json!("sandbox"); }
           stream.push_str(&format!("data: {}\n\n",json!({"type":"response.output_item.done","output_index":index,"item":item})));
       }
       let mut terminal=doc.clone();terminal["output"]=json!([]);
       stream.push_str(&format!("data: {}\n\n",json!({"type":"response.completed","response":terminal})));
       return ([("content-type","text/event-stream")],stream).into_response();
   }
   Json(doc).into_response()
 }))
 .route("/v1/messages",post(|State(p):State<Provider>,Json(body):Json<Value>|async move{
   p.requests.lock().unwrap().push(body);
   Json(p.replies.lock().unwrap().pop_front().unwrap_or_else(||json!({"stop_reason":"end_turn","content":[{"type":"text","text":"Claude fixture"}]})))
 })).with_state(provider.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let templates =
        crate::templates::TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
    let mut state = AppState::with_db_file(templates, dir.path().join("state.db"))
        .await
        .unwrap();
    state.pipe_ai = crate::pipe_ai::OpenAi::at(format!("{base}/v1/responses"));
    state.pipe_ai_anthropic = crate::pipe_ai::anthropic::Anthropic::at(format!("{base}/v1/models"));
    let app = router(&state);
    Fixture {
        dir,
        state,
        app,
        provider,
        server,
    }
}
fn router(state: &AppState) -> Router {
    build_router(
        state.clone(),
        &HttpConfig::from_values("127.0.0.1:5523", "http://localhost:8080").unwrap(),
    )
    .layer(Extension(crate::ai_sandbox::access::LocalOperator))
}
async fn call(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("host", "127.0.0.1:5523")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let result = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert!(!String::from_utf8_lossy(&bytes).contains("sandbox-private-key"));
    (status, result)
}
async fn connect(f: &Fixture, provider: &str) -> Uuid {
    let id = Uuid::new_v4();
    assert_eq!(
        call(
            &f.app,
            "PUT",
            &format!("/api/pipe/ai/connections/{id}"),
            json!({"name":"Fixture","provider":provider,"apiKey":"sandbox-private-key"})
        )
        .await
        .0,
        StatusCode::OK
    );
    id
}
async fn verify(f: &Fixture, id: Uuid, tools: bool) {
    if tools {
        f.provider
            .replies
            .lock()
            .unwrap()
            .extend([tool("probe", "sandbox_ready", json!({})), text("OK")]);
    }
    let (s, v) = call(
        &f.app,
        "POST",
        &format!("/api/ai-sandbox/connections/{id}/verify"),
        json!({"model":"fixture-model","tools":tools}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["verified"], true);
}
async fn project(f: &Fixture, connection: Uuid, code: bool) -> Uuid {
    let id = Uuid::new_v4();
    let source = f.dir.path().join("project");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("main.txt"), "original").unwrap();
    let vm = Uuid::new_v4();
    if code {
        let mut record = crate::handlers::vms::test_support::record("sandbox", vm);
        record.state = firecrab_api_types::VmState::Running;
        record.last_runtime_id = Some(Uuid::new_v4());
        crate::handlers::vms::test_support::seed_vm(&f.state, &record);
    }
    let (s,v)=call(&f.app,"PUT",&format!("/api/ai-sandbox/projects/{id}"),json!({"name":"Project","sourcePath":if code{json!(source)}else{Value::Null},"vmId":if code{json!(vm)}else{Value::Null},"selection":{"connectionId":connection,"model":"fixture-model"},"instructions":"Be concise"})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    id
}
async fn start(f: &Fixture, project: Uuid, id: Uuid, kind: &str, input: &str) -> Value {
    let (s, v) = call(
        &f.app,
        "PUT",
        &format!("/api/ai-sandbox/projects/{project}/tasks/{id}"),
        json!({"kind":kind,"input":input,"selection":null,"retryOf":null}),
    )
    .await;
    assert!(s == StatusCode::OK || s == StatusCode::ACCEPTED, "{s} {v}");
    v
}
async fn finish(f: &Fixture, id: Uuid) -> Value {
    for _ in 0..300 {
        let (_, t) = call(
            &f.app,
            "GET",
            &format!("/api/ai-sandbox/tasks/{id}"),
            Value::Null,
        )
        .await;
        if !matches!(
            t["state"].as_str(),
            Some("waiting" | "accepted" | "in_progress")
        ) {
            return t;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("task did not finish");
}
#[tokio::test]
async fn sandbox_conversation_uses_verified_defaults_redacts_and_persists_without_duplicate_dispatch()
 {
    let f = fixture().await;
    let c = connect(&f, "openai").await;
    let (_, catalog) = call(
        &f.app,
        "GET",
        &format!("/api/ai-sandbox/connections/{c}/models?discover=true"),
        Value::Null,
    )
    .await;
    assert_eq!(catalog["models"][0]["verified"], false);
    verify(&f, c, false).await;
    let p = project(&f, c, false).await;
    let id = Uuid::new_v4();
    start(&f, p, id, "chat", "hello sandbox-private-key").await;
    let result = finish(&f, id).await;
    assert_eq!(result["state"], "result");
    assert!(result["result"].as_str().unwrap().contains("[REDACTED]"));
    let count = f.provider.requests.lock().unwrap().len();
    let duplicate = start(&f, p, id, "chat", "hello sandbox-private-key").await;
    assert_eq!(duplicate["attemptId"], result["attemptId"]);
    assert_eq!(f.provider.requests.lock().unwrap().len(), count);
    let next = Uuid::new_v4();
    start(&f, p, next, "chat", "continue").await;
    finish(&f, next).await;
    let request = f.provider.requests.lock().unwrap().last().unwrap().clone();
    assert!(request["input"].to_string().contains("hello [REDACTED]"));
    let templates =
        crate::templates::TemplateRegistry::from_specs(f.dir.path(), std::iter::empty()).unwrap();
    let reopened = AppState::with_db_file(templates, f.dir.path().join("state.db"))
        .await
        .unwrap();
    assert_eq!(
        call(
            &router(&reopened),
            "GET",
            &format!("/api/ai-sandbox/projects/{p}/tasks"),
            Value::Null
        )
        .await
        .1
        .as_array()
        .unwrap()
        .len(),
        2
    );
}
#[tokio::test]
async fn sandbox_native_tools_edit_an_isolated_branch_and_apply_checks_conflicts() {
    let f = fixture().await;
    let c = connect(&f, "openai").await;
    verify(&f, c, true).await;
    verify(&f, c, false).await;
    let (_, catalog) = call(
        &f.app,
        "GET",
        &format!("/api/ai-sandbox/connections/{c}/models"),
        Value::Null,
    )
    .await;
    assert_eq!(catalog["models"][0]["toolsVerified"], true);
    let p = project(&f, c, true).await;
    let id = Uuid::new_v4();
    f.provider.replies.lock().unwrap().extend([
        tool(
            "edit",
            "write_file",
            json!({"path":"main.txt","content":"updated"}),
        ),
        text("Edited"),
    ]);
    start(&f, p, id, "code", "edit main").await;
    let result = finish(&f, id).await;
    assert_eq!(result["state"], "result", "{result}");
    assert_eq!(result["changes"][0]["path"], "main.txt");
    let source = f.dir.path().join("project/main.txt");
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "original");
    std::fs::write(&source, "user change").unwrap();
    let apply = format!("/api/ai-sandbox/tasks/{id}/apply");
    let mut pending = f.state.store.sandbox_task(id).unwrap().unwrap();
    pending.task.id = Uuid::new_v4();
    pending.task.attempt_id = Uuid::new_v4();
    pending.task.state = firecrab_api_types::SandboxTaskState::InProgress;
    pending.workspace = None;
    f.state.store.save_sandbox_task(&pending).unwrap();
    assert_eq!(
        call(&f.app, "POST", &apply, json!({"paths":["main.txt"]}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    pending.task.state = firecrab_api_types::SandboxTaskState::Cancelled;
    f.state.store.save_sandbox_task(&pending).unwrap();

    assert_ne!(
        call(&f.app, "POST", &apply, json!({"paths":["main.txt"]}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "user change");
    std::fs::write(&source, "original").unwrap();
    let (s, v) = call(&f.app, "POST", &apply, json!({"paths":["main.txt"]})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["state"], "merged");
    assert_eq!(std::fs::read_to_string(source).unwrap(), "updated");
    let requests = f.provider.requests.lock().unwrap();
    assert!(
        requests.last().unwrap()["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["type"] == "function_call_output" && v["call_id"] == "edit")
    );
}
#[tokio::test]
async fn sandbox_cancel_and_restart_do_not_replay_uncertain_attempts() {
    let f = fixture().await;
    let c = connect(&f, "openai").await;
    verify(&f, c, false).await;
    let p = project(&f, c, false).await;
    let id = Uuid::new_v4();
    start(&f, p, id, "chat", "hold-request").await;
    assert_eq!(
        call(
            &f.app,
            "POST",
            &format!("/api/ai-sandbox/tasks/{id}/cancel"),
            Value::Null
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(finish(&f, id).await["state"], "cancelled");
    let mut saved = f.state.store.sandbox_task(id).unwrap().unwrap();
    saved.task.state = firecrab_api_types::SandboxTaskState::InProgress;
    f.state.store.save_sandbox_task(&saved).unwrap();
    f.state.store.interrupt_sandbox_tasks().unwrap();
    let count = f.provider.requests.lock().unwrap().len();
    let interrupted = call(
        &f.app,
        "GET",
        &format!("/api/ai-sandbox/tasks/{id}"),
        Value::Null,
    )
    .await
    .1;
    assert_eq!(interrupted["state"], "interrupted");
    assert_eq!(
        call(
            &f.app,
            "POST",
            &format!("/api/ai-sandbox/tasks/{id}/reconcile"),
            Value::Null
        )
        .await
        .1["state"],
        "cancelled"
    );
    assert_eq!(f.provider.requests.lock().unwrap().len(), count);
    call(
        &f.app,
        "DELETE",
        &format!("/api/pipe/ai/connections/{c}"),
        Value::Null,
    )
    .await;
    let (s, _) = call(
        &f.app,
        "PUT",
        &format!("/api/ai-sandbox/projects/{p}/tasks/{}", Uuid::new_v4()),
        json!({"kind":"chat","input":"after disconnect"}),
    )
    .await;
    assert_ne!(s, StatusCode::ACCEPTED);
}
#[tokio::test]
async fn sandbox_anthropic_uses_native_messages_and_tool_result_protocol() {
    let f = fixture().await;
    let c = connect(&f, "anthropic").await;
    f.provider.replies.lock().unwrap().extend([json!({"stop_reason":"tool_use","content":[{"type":"tool_use","id":"probe","name":"sandbox_ready","input":{}}]}),json!({"stop_reason":"end_turn","content":[{"type":"text","text":"OK"}]})]);
    let (s, v) = call(
        &f.app,
        "POST",
        &format!("/api/ai-sandbox/connections/{c}/verify"),
        json!({"model":"fixture-model","tools":true}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    {
        let requests = f.provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1]["messages"][2]["content"][0]["type"],
            "tool_result"
        );
        assert!(requests[0]["tools"][0].get("input_schema").is_some());
    }
    let p = project(&f, c, false).await;
    let id = Uuid::new_v4();
    start(&f, p, id, "chat", "hello Claude").await;
    assert_eq!(finish(&f, id).await["result"], "Claude fixture");
}

#[tokio::test]
async fn sandbox_chatgpt_discovers_account_models_and_uses_namespaced_native_tools() {
    let f = fixture().await;
    let c = connect(&f, "openai").await;
    let mut account = f.state.store.ai_connection(c).unwrap().unwrap();
    account.connection.authentication = firecrab_api_types::AiAuthentication::Chatgpt;
    account.oauth = Some(crate::pipe_ai::OAuthSession {
        client_id: "fixture-client".into(),
        subject: "fixture-account".into(),
        refresh_token: "sandbox-refresh-key".into(),
        expires_at: u64::MAX,
        earliest_refresh_at: 0,
        scope: "openid chatgpt.tokens.use.direct".into(),
    });
    f.state.store.save_ai_connection(&account).unwrap();
    let (status, models) = call(
        &f.app,
        "GET",
        &format!("/api/ai-sandbox/connections/{c}/models?discover=true"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{models}");
    assert_eq!(models["models"].as_array().unwrap().len(), 1);
    assert_eq!(models["models"][0]["name"], "ChatGPT Fixture");
    assert_eq!(models["models"][0]["verified"], false);
    verify(&f, c, true).await;
    let requests = f.provider.requests.lock().unwrap();
    let first = &requests[0];
    assert_eq!(first["tools"][0]["type"], "namespace");
    assert_eq!(first["tools"][0]["name"], "sandbox");
    assert_eq!(first["tools"][0]["tools"][0]["name"], "sandbox_ready");
    assert_eq!(first["stream"], true);
    assert_eq!(first["store"], false);
    assert!(first.get("max_output_tokens").is_none());
}

#[tokio::test]
async fn sandbox_task_override_does_not_change_the_project_default_and_replacement_invalidates_proofs()
 {
    let f = fixture().await;
    let c = connect(&f, "openai").await;
    verify(&f, c, false).await;
    let p = project(&f, c, false).await;
    let (status, _) = call(
        &f.app,
        "POST",
        &format!("/api/ai-sandbox/connections/{c}/verify"),
        json!({"model":"override-model","tools":false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = Uuid::new_v4();
    let (status,_)=call(&f.app,"PUT",&format!("/api/ai-sandbox/projects/{p}/tasks/{id}"),json!({"kind":"chat","input":"override","selection":{"connectionId":c,"model":"override-model"}})).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(finish(&f, id).await["selection"]["model"], "override-model");
    assert_eq!(
        call(
            &f.app,
            "GET",
            &format!("/api/ai-sandbox/projects/{p}"),
            Value::Null
        )
        .await
        .1["spec"]["selection"]["model"],
        "fixture-model"
    );
    let mut account = f.state.store.ai_connection(c).unwrap().unwrap();
    account.api_key = "replacement-private-key".into();
    f.state.store.save_ai_connection(&account).unwrap();
    let models = call(
        &f.app,
        "GET",
        &format!("/api/ai-sandbox/connections/{c}/models"),
        Value::Null,
    )
    .await
    .1;
    assert!(models["models"].as_array().unwrap().is_empty());
    let (status, _) = call(
        &f.app,
        "PUT",
        &format!("/api/ai-sandbox/projects/{p}/tasks/{}", Uuid::new_v4()),
        json!({"kind":"chat","input":"must verify again"}),
    )
    .await;
    assert_ne!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn sandbox_rejects_duplicate_tool_calls_unknown_fields_and_untrusted_origins() {
    let f = fixture().await;
    let c = connect(&f, "openai").await;
    verify(&f, c, true).await;
    let p = project(&f, c, true).await;
    let mut duplicate = tool(
        "same",
        "write_file",
        json!({"path":"main.txt","content":"bad"}),
    );
    let item = duplicate["output"][0].clone();
    duplicate["output"].as_array_mut().unwrap().push(item);
    f.provider.replies.lock().unwrap().push_back(duplicate);
    let id = Uuid::new_v4();
    start(&f, p, id, "code", "bad provider call").await;
    let task = finish(&f, id).await;
    assert_eq!(task["state"], "failed");
    assert!(task["events"].as_array().unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(f.dir.path().join("project/main.txt")).unwrap(),
        "original"
    );
    let uri = format!("/api/ai-sandbox/projects/{p}/tasks/{}", Uuid::new_v4());
    assert_ne!(
        call(
            &f.app,
            "PUT",
            &uri,
            json!({"kind":"chat","input":"unknown field","apiKey":"forbidden"})
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(uri)
                .header("host", "127.0.0.1:5523")
                .header("origin", "https://untrusted.invalid")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"kind":"code","input":"must not run"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

// An HTTP connection can disappear while SQLite waits for another writer. Admission must
// retain its owned claim and cancellation receiver until the durable save/dispatch finishes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sandbox_disconnected_admission_still_dispatches_once_and_preserves_early_cancel() {
    for cancel_early in [false, true] {
        let f = fixture().await;
        let c = connect(&f, "openai").await;
        verify(&f, c, false).await;
        let p = project(&f, c, false).await;
        let id = Uuid::new_v4();
        let blocker = rusqlite::Connection::open(f.dir.path().join("state.db")).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let app = f.app.clone();
        let request = tokio::spawn(async move {
            call(
                &app,
                "PUT",
                &format!("/api/ai-sandbox/projects/{p}/tasks/{id}"),
                json!({"kind":"chat","input":"disconnected admission"}),
            )
            .await
        });
        let mut sender = None;
        for _ in 0..200 {
            sender = f
                .state
                .sandbox
                .cancellations
                .lock()
                .unwrap()
                .get(&id)
                .cloned();
            if sender.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let sender = sender.expect("admission must register cancellation before blocking save");
        request.abort();
        if cancel_early {
            sender.send(true).unwrap();
        }
        blocker.execute_batch("COMMIT").unwrap();
        let result = finish(&f, id).await;
        assert_eq!(
            result["state"],
            if cancel_early { "cancelled" } else { "result" },
            "{result}"
        );
        let count = f.provider.requests.lock().unwrap().len();
        let duplicate = start(&f, p, id, "chat", "disconnected admission").await;
        assert_eq!(duplicate["attemptId"], result["attemptId"]);
        assert_eq!(f.provider.requests.lock().unwrap().len(), count);
        assert_eq!(count, if cancel_early { 1 } else { 2 });
    }
}

#[tokio::test]
async fn sandbox_project_settings_reject_unsafe_sources_credentials_and_ineligible_vms() {
    let f = fixture().await;
    connect(&f, "openai").await;
    let id = Uuid::new_v4();
    let uri = format!("/api/ai-sandbox/projects/{id}");
    let source_file = f.dir.path().join("regular-file");
    std::fs::write(&source_file, "keep").unwrap();
    std::fs::create_dir_all(&f.state.sandbox.root).unwrap();
    let builder_id = Uuid::new_v4();
    let mut builder = crate::handlers::vms::test_support::record("builder", builder_id);
    builder.purpose = crate::model::VmPurpose::Builder;
    crate::handlers::vms::test_support::seed_vm(&f.state, &builder);
    let base =
        json!({"name":"Project","sourcePath":null,"vmId":null,"selection":null,"instructions":""});
    for fields in [
        json!({"name":"   "}),
        json!({"name":"bad\nname"}),
        json!({"name":"x".repeat(201)}),
        json!({"instructions":"x".repeat(16385)}),
        json!({"sourcePath":"relative/path"}),
        json!({"sourcePath":f.dir.path().join("missing")}),
        json!({"sourcePath":source_file}),
        json!({"sourcePath":"/"}),
        json!({"sourcePath":"/etc"}),
        json!({"sourcePath":f.state.sandbox.root}),
        json!({"vmId":Uuid::new_v4()}),
        json!({"vmId":builder_id}),
        json!({"instructions":"Do not copy sandbox-private-key into a project"}),
    ] {
        let mut spec = base.clone();
        for (key, value) in fields.as_object().unwrap() {
            spec[key] = value.clone();
        }
        let (status, body) = call(&f.app, "PUT", &uri, spec).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{fields}: {body}");
        assert!(f.state.store.sandbox_project(id).unwrap().is_none());
    }
    assert_eq!(
        call(&f.app, "GET", &uri, Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(call(&f.app, "PUT", &uri, base).await.0, StatusCode::OK);
    assert_eq!(
        call(&f.app, "DELETE", &uri, Value::Null).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&f.app, "GET", "/api/ai-sandbox/projects", Value::Null)
            .await
            .1,
        json!([])
    );
    assert!(f.provider.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn sandbox_project_crud_blocks_active_work_and_preserves_borrowed_resources_on_delete() {
    let f = fixture().await;
    let connection = connect(&f, "openai").await;
    verify(&f, connection, false).await;
    let id = Uuid::new_v4();
    let uri = format!("/api/ai-sandbox/projects/{id}");
    let vm = Uuid::new_v4();
    crate::handlers::vms::test_support::seed_vm(
        &f.state,
        &crate::handlers::vms::test_support::record("borrowed", vm),
    );
    let source = f.dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("keep.txt"), "user-owned").unwrap();
    let spec = json!({"name":"  Project  ","sourcePath":source,"vmId":vm,"selection":{"connectionId":connection,"model":"fixture-model"},"instructions":"Be concise"});
    let (status, first) = call(&f.app, "PUT", &uri, spec.clone()).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["spec"]["name"], "Project");
    assert_eq!(
        first["spec"]["sourcePath"],
        json!(source.canonicalize().unwrap())
    );
    let (_, second) = call(&f.app, "PUT", &uri, spec.clone()).await;
    assert_eq!(second["createdAt"], first["createdAt"]);
    assert_eq!(call(&f.app, "GET", &uri, Value::Null).await.1, second);
    assert_eq!(
        call(&f.app, "GET", "/api/ai-sandbox/projects", Value::Null)
            .await
            .1,
        json!([second])
    );
    let other = format!("/api/ai-sandbox/projects/{}", Uuid::new_v4());
    assert_eq!(
        call(&f.app, "PUT", &other, spec.clone()).await.0,
        StatusCode::BAD_REQUEST
    );

    let owned = f.state.sandbox.root.join(id.to_string());
    let unrelated = f.state.sandbox.root.join("unrelated");
    std::fs::create_dir_all(&owned).unwrap();
    std::fs::create_dir_all(&unrelated).unwrap();
    std::fs::write(owned.join("result.txt"), "retained").unwrap();
    std::fs::write(unrelated.join("keep.txt"), "other task").unwrap();
    let task = Uuid::new_v4();
    start(&f, id, task, "chat", "hold-request").await;
    assert_eq!(
        call(&f.app, "PUT", &uri, spec).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&f.app, "DELETE", &uri, Value::Null).await.0,
        StatusCode::CONFLICT
    );
    assert!(owned.join("result.txt").exists());
    call(
        &f.app,
        "POST",
        &format!("/api/ai-sandbox/tasks/{task}/cancel"),
        Value::Null,
    )
    .await;
    assert_eq!(finish(&f, task).await["state"], "cancelled");
    let requests = f.provider.requests.lock().unwrap().len();
    let (status, deleted) = call(&f.app, "DELETE", &uri, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    assert_eq!(deleted, json!({"deleted":true}));
    assert!(!owned.exists());
    assert_eq!(
        std::fs::read_to_string(unrelated.join("keep.txt")).unwrap(),
        "other task"
    );
    assert_eq!(
        std::fs::read_to_string(source.join("keep.txt")).unwrap(),
        "user-owned"
    );
    assert!(f.state.vms.lock().unwrap().contains_key(&vm));
    assert!(f.state.store.sandbox_project(id).unwrap().is_none());
    assert!(f.state.store.sandbox_task(task).unwrap().is_none());
    assert_eq!(f.provider.requests.lock().unwrap().len(), requests);
}

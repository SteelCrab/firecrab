use super::*;
use axum::extract::ConnectInfo;
use firecrab_api_types::*;

async fn as_host(
    f: &Fixture,
    host: Uuid,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:5523")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(crate::tls::AuthenticatedHost { host_id: host }));
    let response = f.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("sandbox-private-key"));
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn grant(kind: &str, id: Uuid, permissions: &[&str]) -> Value {
    json!({"resource":{"kind":kind,"id":id},"permissions":permissions})
}

async fn policy(f: &Fixture, host: Uuid, grants: Vec<Value>) {
    let (status, value) = call(
        &f.app,
        "PUT",
        &format!("/api/ai-sandbox/access/{host}"),
        json!({"grants":grants}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
}

fn project_grants(f: &Fixture, project: Uuid, connection: Uuid) -> Vec<Value> {
    let spec = f
        .state
        .store
        .sandbox_project(project)
        .unwrap()
        .unwrap()
        .spec;
    let mut grants = vec![
        grant("project", project, &["read", "write", "execute", "apply"]),
        grant("connection", connection, &["read", "execute"]),
    ];
    if let Some(path) = spec.source_path {
        grants
            .push(json!({"resource":{"kind":"source","path":path},"permissions":["read","write"]}));
    }
    if let Some(id) = spec.vm_id {
        grants.push(grant("vm", id, &["read", "execute"]));
    }
    grants
}

#[tokio::test]
async fn sandbox_refuses_controller_owned_vms_even_with_operator_permissions() {
    let f = fixture().await;
    let connection = connect(&f, "openai").await;
    verify(&f, connection, true).await;
    let project = project(&f, connection, true).await;
    let spec = f
        .state
        .store
        .sandbox_project(project)
        .unwrap()
        .unwrap()
        .spec;
    let vm = spec.vm_id.unwrap();
    let host = Uuid::new_v4();
    let target = json!({"network":{"hostId":host,"resourceId":Uuid::new_v4().to_string()},"storage":{"hostId":host,"resourceId":"default"}});
    let receipt:WorkerReceipt=serde_json::from_value(json!({"id":vm,"controllerId":Uuid::new_v4(),"intent":{"generation":1,"action":"create","spec":{"name":"owned","image":{"alias":"fixture","version":"1","kernelSha256":"0".repeat(64),"rootfsSha256":"1".repeat(64),"bootArgsSha256":"2".repeat(64)},"cpu":1,"ram":128,"diskGb":2,"targets":[target]},"target":target},"phase":"confirmed","observed":null,"error":null})).unwrap();
    rusqlite::Connection::open(f.dir.path().join("state.db"))
        .unwrap()
        .execute(
            "INSERT INTO cluster_worker_receipts(id,data) VALUES(?1,?2)",
            rusqlite::params![vm.to_string(), serde_json::to_string(&receipt).unwrap()],
        )
        .unwrap();
    assert!(crate::ai_sandbox::guest::binding(&f.state, vm).is_err());
    assert_eq!(
        call(
            &f.app,
            "PUT",
            &format!("/api/ai-sandbox/projects/{project}"),
            serde_json::to_value(&spec).unwrap()
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let task = Uuid::new_v4();
    assert_eq!(
        call(
            &f.app,
            "PUT",
            &format!("/api/ai-sandbox/projects/{project}/tasks/{task}"),
            json!({"kind":"code","input":"do not bypass owner"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert!(f.state.store.sandbox_task(task).unwrap().is_none());
}

#[tokio::test]
async fn sandbox_source_permissions_filter_history_before_pagination() {
    let f = fixture().await;
    let connection = connect(&f, "openai").await;
    verify(&f, connection, true).await;
    let project = project(&f, connection, true).await;
    let old = Uuid::new_v4();
    f.provider
        .replies
        .lock()
        .unwrap()
        .push_back(text("private-old-answer"));
    start(&f, project, old, "chat", "old source").await;
    finish(&f, old).await;
    let snapshot = f.state.store.sandbox_task(old).unwrap().unwrap();
    let next = f.dir.path().join("next-source");
    std::fs::create_dir(&next).unwrap();
    let next = next.canonicalize().unwrap().to_str().unwrap().to_owned();
    let mut spec = f
        .state
        .store
        .sandbox_project(project)
        .unwrap()
        .unwrap()
        .spec;
    spec.source_path = Some(next.clone());
    assert_eq!(
        call(
            &f.app,
            "PUT",
            &format!("/api/ai-sandbox/projects/{project}"),
            serde_json::to_value(&spec).unwrap()
        )
        .await
        .0,
        StatusCode::OK
    );
    let host = Uuid::new_v4();
    policy(
        &f,
        host,
        vec![
            grant("project", project, &["read", "execute"]),
            grant("connection", connection, &["execute"]),
            json!({"resource":{"kind":"source","path":next},"permissions":["read"]}),
        ],
    )
    .await;
    let id = Uuid::new_v4();
    assert_eq!(
        as_host(
            &f,
            host,
            "PUT",
            &format!("/api/ai-sandbox/projects/{project}/tasks/{id}"),
            json!({"kind":"chat","input":"new source"})
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    finish(&f, id).await;
    assert!(
        !f.provider
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .to_string()
            .contains("private-old-answer")
    );
    assert_eq!(
        as_host(
            &f,
            host,
            "GET",
            &format!("/api/ai-sandbox/tasks/{old}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for _ in 0..60 {
        let mut older = snapshot.clone();
        older.task.id = Uuid::new_v4();
        older.task.attempt_id = Uuid::new_v4();
        f.state.store.save_sandbox_task(&older).unwrap();
    }
    let (status, page) = as_host(
        &f,
        host,
        "GET",
        &format!("/api/ai-sandbox/projects/{project}/tasks"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page.as_array().unwrap().len(), 1);
    assert_eq!(page[0]["id"], id.to_string());
}

#[tokio::test]
async fn sandbox_identity_is_transport_only_and_tls_controller_can_delegate() {
    let f = fixture().await;
    let config = HttpConfig::from_values("127.0.0.1:5523", "").unwrap();
    let untrusted = build_router(f.state.clone(), &config);
    let response = untrusted
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/ai-sandbox/projects")
                .header("x-firecrab-host-id", Uuid::new_v4().to_string())
                .header("x-firecrab-operator", "true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let ca = f.dir.path().join("ca");
    firecrab_pki::init_ca(&ca).unwrap();
    let operator = Uuid::new_v4();
    let host = Uuid::new_v4();
    let server = firecrab_pki::issue_host_cert(&ca, operator, vec!["127.0.0.1".into()], 1).unwrap();
    let peer = firecrab_pki::issue_host_cert(&ca, host, vec![], 1).unwrap();
    let admin = reqwest::Client::builder()
        .no_proxy()
        .use_preconfigured_tls(server.client_config().unwrap())
        .build()
        .unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .use_preconfigured_tls(peer.client_config().unwrap())
        .build()
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let transport = crate::tls::configuration(address, Some(server)).unwrap();
    let task = tokio::spawn(crate::tls::serve(listener, untrusted, transport));
    let url = format!("https://{address}/api/ai-sandbox/access/{host}");
    assert_eq!(
        client
            .get(format!("https://{address}/api/host"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        client
            .get(&url)
            .header("x-firecrab-host-id", operator.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let project = Uuid::new_v4();
    let response = admin
        .put(&url)
        .header("content-type", "application/json")
        .body(json!({"grants":[grant("project",project,&["read","write"])]}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(f.state.store.sandbox_access(host).unwrap().grants.len(), 1);
    assert_ne!(
        client
            .get(format!("https://{address}/"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .get(format!("https://{address}/api/host"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .get(format!("https://{address}/ws/vms/{project}/console"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let response = client
        .put(format!(
            "https://{address}/api/ai-sandbox/projects/{project}"
        ))
        .header("content-type", "application/json")
        .body(json!({"name":"Delegated","instructions":""}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<Value>(&response.text().await.unwrap()).unwrap()["owner"],
        json!({"host":host})
    );
    assert_eq!(
        admin.delete(&url).send().await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        client
            .get(format!("https://{address}/api/pipe/ai/connections"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .get(format!(
                "https://{address}/api/ai-sandbox/projects/{project}"
            ))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    task.abort();
}

#[tokio::test]
async fn sandbox_all_project_task_and_model_routes_reject_another_host() {
    let f = fixture().await;
    let connection = connect(&f, "openai").await;
    verify(&f, connection, false).await;
    let project = project(&f, connection, false).await;
    let id = Uuid::new_v4();
    start(&f, project, id, "chat", "hello").await;
    finish(&f, id).await;
    let host = Uuid::new_v4();
    let requests = [
        (
            "GET",
            format!("/api/ai-sandbox/projects/{project}"),
            Value::Null,
        ),
        (
            "PUT",
            format!("/api/ai-sandbox/projects/{project}"),
            json!({"name":"stolen"}),
        ),
        (
            "DELETE",
            format!("/api/ai-sandbox/projects/{project}"),
            Value::Null,
        ),
        (
            "GET",
            format!("/api/ai-sandbox/projects/{project}/tasks"),
            Value::Null,
        ),
        (
            "PUT",
            format!(
                "/api/ai-sandbox/projects/{project}/tasks/{}",
                Uuid::new_v4()
            ),
            json!({"kind":"chat","input":"stolen"}),
        ),
        ("GET", format!("/api/ai-sandbox/tasks/{id}"), Value::Null),
        (
            "POST",
            format!("/api/ai-sandbox/tasks/{id}/cancel"),
            json!({}),
        ),
        (
            "POST",
            format!("/api/ai-sandbox/tasks/{id}/reconcile"),
            json!({}),
        ),
        (
            "POST",
            format!("/api/ai-sandbox/tasks/{id}/apply"),
            json!({"paths":["main.txt"]}),
        ),
        (
            "GET",
            format!("/api/ai-sandbox/connections/{connection}/models?discover=true"),
            Value::Null,
        ),
        (
            "POST",
            format!("/api/ai-sandbox/connections/{connection}/verify"),
            json!({"model":"fixture-model","tools":false}),
        ),
        ("GET", format!("/api/ai-sandbox/access/{host}"), Value::Null),
        (
            "PUT",
            format!("/api/ai-sandbox/access/{host}"),
            json!({"grants":[]}),
        ),
        (
            "DELETE",
            format!("/api/ai-sandbox/access/{host}"),
            Value::Null,
        ),
    ];
    let calls = f.provider.requests.lock().unwrap().len();
    for (method, path, body) in requests {
        let (status, result) = as_host(&f, host, method, &path, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {result}");
    }
    assert_eq!(f.provider.requests.lock().unwrap().len(), calls);
    assert_eq!(
        as_host(&f, host, "GET", "/api/ai-sandbox/projects", Value::Null)
            .await
            .1,
        json!([])
    );
    policy(&f, host, vec![grant("project", project, &["read"])]).await;
    assert_eq!(
        as_host(&f, host, "GET", "/api/ai-sandbox/projects", Value::Null)
            .await
            .1
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        as_host(
            &f,
            host,
            "GET",
            &format!("/api/ai-sandbox/tasks/{id}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        as_host(
            &f,
            host,
            "POST",
            &format!("/api/ai-sandbox/tasks/{id}/cancel"),
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn sandbox_resource_grants_are_exact_and_do_not_allow_privilege_escalation() {
    let f = fixture().await;
    let connection = connect(&f, "openai").await;
    verify(&f, connection, true).await;
    let project = project(&f, connection, true).await;
    let host = Uuid::new_v4();
    let spec = f
        .state
        .store
        .sandbox_project(project)
        .unwrap()
        .unwrap()
        .spec;
    let grants = project_grants(&f, project, connection);
    for missing in ["source", "vm", "connection"] {
        policy(
            &f,
            host,
            grants
                .iter()
                .filter(|g| g["resource"]["kind"] != missing)
                .cloned()
                .collect(),
        )
        .await;
        let (status, body) = as_host(
            &f,
            host,
            "PUT",
            &format!("/api/ai-sandbox/projects/{project}"),
            serde_json::to_value(&spec).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "missing {missing}: {body}");
        let id = Uuid::new_v4();
        assert_eq!(
            as_host(
                &f,
                host,
                "PUT",
                &format!("/api/ai-sandbox/projects/{project}/tasks/{id}"),
                json!({"kind":"code","input":"denied"})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert!(f.state.store.sandbox_task(id).unwrap().is_none());
    }
    policy(&f, host, grants.clone()).await;
    let result = as_host(
        &f,
        host,
        "PUT",
        &format!("/api/ai-sandbox/projects/{project}"),
        serde_json::to_value(&spec).unwrap(),
    )
    .await;
    assert_eq!(result.0, StatusCode::OK, "{}", result.1);
    assert_eq!(result.1["owner"], "local");
    let mut forged = serde_json::to_value(&spec).unwrap();
    forged["owner"] = json!({"host":host});
    assert_eq!(
        as_host(
            &f,
            host,
            "PUT",
            &format!("/api/ai-sandbox/projects/{project}"),
            forged
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let other = Uuid::new_v4();
    let response=as_host(&f,host,"PUT",&format!("/api/ai-sandbox/projects/{project}/tasks/{other}"),json!({"kind":"chat","input":"no","selection":{"connectionId":other,"model":"fixture-model"}})).await;
    assert_eq!(response.0, StatusCode::FORBIDDEN);
    let mut alias = serde_json::to_value(&spec).unwrap();
    alias["sourcePath"] = json!(format!("{}/../project", spec.source_path.as_ref().unwrap()));
    assert_eq!(
        as_host(
            &f,
            host,
            "PUT",
            &format!("/api/ai-sandbox/projects/{project}"),
            alias
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn sandbox_scoped_apply_and_reconciliation_check_the_original_resources() {
    let f = fixture().await;
    let connection = connect(&f, "openai").await;
    verify(&f, connection, true).await;
    let project = project(&f, connection, true).await;
    let host = Uuid::new_v4();
    let grants = project_grants(&f, project, connection);
    let id = Uuid::new_v4();
    f.provider.replies.lock().unwrap().extend([
        tool(
            "scoped-edit",
            "write_file",
            json!({"path":"main.txt","content":"reviewed"}),
        ),
        text("ready"),
    ]);
    start(&f, project, id, "code", "edit").await;
    let original = finish(&f, id).await;
    policy(&f, host, grants.clone()).await;
    assert_eq!(
        as_host(
            &f,
            host,
            "GET",
            &format!("/api/ai-sandbox/tasks/{id}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    for missing in ["read", "write"] {
        let mut limited = grants.clone();
        limited
            .iter_mut()
            .find(|g| g["resource"]["kind"] == "source")
            .unwrap()["permissions"] = json!([if missing == "read" { "write" } else { "read" }]);
        policy(&f, host, limited).await;
        assert_eq!(
            as_host(
                &f,
                host,
                "POST",
                &format!("/api/ai-sandbox/tasks/{id}/apply"),
                json!({"paths":["main.txt"]})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    let mut interrupted = f.state.store.sandbox_task(id).unwrap().unwrap();
    interrupted.task.state = SandboxTaskState::Interrupted;
    f.state.store.save_sandbox_task(&interrupted).unwrap();
    let vm = interrupted.guest.unwrap().vm_id;
    let next_runtime = Uuid::new_v4();
    f.state
        .vms
        .lock()
        .unwrap()
        .get_mut(&vm)
        .unwrap()
        .last_runtime_id = Some(next_runtime);
    policy(
        &f,
        host,
        grants
            .iter()
            .filter(|g| g["resource"]["kind"] != "connection")
            .cloned()
            .collect(),
    )
    .await;
    let response = as_host(
        &f,
        host,
        "POST",
        &format!("/api/ai-sandbox/tasks/{id}/reconcile"),
        json!({}),
    )
    .await;
    assert_eq!(response.0, StatusCode::OK, "{}", response.1);
    assert_eq!(response.1["runtime"], original["runtime"]);
    assert_eq!(
        f.state
            .store
            .sandbox_task(id)
            .unwrap()
            .unwrap()
            .guest
            .unwrap()
            .runtime_id,
        next_runtime
    );
    let response = as_host(
        &f,
        host,
        "POST",
        &format!("/api/ai-sandbox/tasks/{id}/apply"),
        json!({"paths":["main.txt"]}),
    )
    .await;
    assert_eq!(response.0, StatusCode::OK, "{}", response.1);
    assert_eq!(response.1["state"], "merged");
    assert_eq!(response.1["attemptId"], original["attemptId"]);
    assert_eq!(
        std::fs::read_to_string(f.dir.path().join("project/main.txt")).unwrap(),
        "reviewed"
    );
    policy(&f, host, vec![grant("connection", connection, &["read"])]).await;
    assert_eq!(
        as_host(
            &f,
            host,
            "GET",
            &format!("/api/ai-sandbox/connections/{connection}/models"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        as_host(
            &f,
            host,
            "GET",
            &format!("/api/ai-sandbox/connections/{connection}/models?discover=true"),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        as_host(
            &f,
            host,
            "POST",
            &format!("/api/ai-sandbox/connections/{connection}/verify"),
            json!({"model":"fixture-model","tools":false})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn sandbox_revocation_while_waiting_for_provider_lock_prevents_the_call() {
    let f = fixture().await;
    let connection = connect(&f, "openai").await;
    verify(&f, connection, false).await;
    let project = project(&f, connection, false).await;
    let host = Uuid::new_v4();
    policy(&f, host, project_grants(&f, project, connection)).await;
    let guard = f.state.pipe_ai_mutations.lock().await;
    let before = f.provider.requests.lock().unwrap().len();
    let id = Uuid::new_v4();
    assert_eq!(
        as_host(
            &f,
            host,
            "PUT",
            &format!("/api/ai-sandbox/projects/{project}/tasks/{id}"),
            json!({"kind":"chat","input":"wait for the provider"})
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    for _ in 0..100 {
        if f.state.store.sandbox_task(id).unwrap().unwrap().task.state
            == SandboxTaskState::InProgress
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        f.state.store.sandbox_task(id).unwrap().unwrap().task.state,
        SandboxTaskState::InProgress
    );
    // Let the worker reach the held provider lock before changing its grants.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        call(
            &f.app,
            "DELETE",
            &format!("/api/ai-sandbox/access/{host}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    drop(guard);
    let task = finish(&f, id).await;
    assert_eq!(task["state"], "failed", "{task}");
    assert_eq!(f.provider.requests.lock().unwrap().len(), before);
}

#[tokio::test]
async fn sandbox_revocation_during_model_verification_prevents_the_followup_call() {
    let f = fixture().await;
    let connection = connect(&f, "openai").await;
    let host = Uuid::new_v4();
    policy(
        &f,
        host,
        vec![grant("connection", connection, &["execute"])],
    )
    .await;
    f.provider
        .replies
        .lock()
        .unwrap()
        .extend([tool("probe", "sandbox_ready", json!({})), text("OK")]);
    let before = f.provider.requests.lock().unwrap().len();
    let path = format!("/api/ai-sandbox/connections/{connection}/verify");
    let (response, ()) = tokio::join!(
        as_host(
            &f,
            host,
            "POST",
            &path,
            json!({"model":"hold-request","tools":true})
        ),
        async {
            for _ in 0..100 {
                if f.provider.requests.lock().unwrap().len() > before {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert_eq!(f.provider.requests.lock().unwrap().len(), before + 1);
            assert_eq!(
                call(
                    &f.app,
                    "DELETE",
                    &format!("/api/ai-sandbox/access/{host}"),
                    Value::Null
                )
                .await
                .0,
                StatusCode::OK
            );
        }
    );
    assert_eq!(response.0, StatusCode::FORBIDDEN, "{}", response.1);
    assert_eq!(f.provider.requests.lock().unwrap().len(), before + 1);
    assert!(f.state.store.sandbox_models(connection).unwrap().is_empty());
}

#[tokio::test]
async fn sandbox_revocation_during_a_provider_turn_prevents_file_tool_execution() {
    let f = fixture().await;
    let connection = connect(&f, "openai").await;
    verify(&f, connection, true).await;
    let project = project(&f, connection, true).await;
    let host = Uuid::new_v4();
    policy(&f, host, project_grants(&f, project, connection)).await;
    f.provider.replies.lock().unwrap().push_back(tool(
        "revoked-call",
        "write_file",
        json!({"path":"main.txt","content":"unauthorized"}),
    ));
    let before = f.provider.requests.lock().unwrap().len();
    let id = Uuid::new_v4();
    let response = as_host(
        &f,
        host,
        "PUT",
        &format!("/api/ai-sandbox/projects/{project}/tasks/{id}"),
        json!({"kind":"code","input":"hold-request"}),
    )
    .await;
    assert_eq!(response.0, StatusCode::ACCEPTED, "{}", response.1);
    for _ in 0..100 {
        if f.provider.requests.lock().unwrap().len() > before {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(f.provider.requests.lock().unwrap().len() > before);
    call(
        &f.app,
        "DELETE",
        &format!("/api/ai-sandbox/access/{host}"),
        Value::Null,
    )
    .await;
    let task = finish(&f, id).await;
    assert_eq!(task["state"], "failed", "{task}");
    assert!(task["events"].as_array().unwrap().is_empty());
    let saved = f.state.store.sandbox_task(id).unwrap().unwrap();
    assert_eq!(
        std::fs::read_to_string(saved.workspace.unwrap().join("work/main.txt")).unwrap(),
        "original"
    );
    assert_eq!(
        as_host(
            &f,
            host,
            "GET",
            &format!("/api/ai-sandbox/tasks/{id}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn sandbox_admission_captures_runtime_and_never_retargets_a_queued_attempt() {
    let f = fixture().await;
    let connection = connect(&f, "openai").await;
    verify(&f, connection, true).await;
    let project = project(&f, connection, true).await;
    let vm = f
        .state
        .store
        .sandbox_project(project)
        .unwrap()
        .unwrap()
        .spec
        .vm_id
        .unwrap();
    let runtime = f.state.vms.lock().unwrap()[&vm].last_runtime_id.unwrap();
    let permits = f.state.sandbox.permits.acquire_many(2).await.unwrap();
    let calls = f.provider.requests.lock().unwrap().len();
    let id = Uuid::new_v4();
    let admitted = start(&f, project, id, "code", "queued").await;
    assert_eq!(admitted["runtime"], json!({"vmId":vm,"runtimeId":runtime}));
    f.state
        .vms
        .lock()
        .unwrap()
        .get_mut(&vm)
        .unwrap()
        .last_runtime_id = Some(Uuid::new_v4());
    drop(permits);
    let task = finish(&f, id).await;
    assert_eq!(task["state"], "failed");
    assert_eq!(task["runtime"], admitted["runtime"]);
    assert_eq!(task["attemptId"], admitted["attemptId"]);
    assert_eq!(f.provider.requests.lock().unwrap().len(), calls);
    let reopened = crate::persistence::Store::open(&f.dir.path().join("state.db")).unwrap();
    let persisted = reopened.sandbox_task(id).unwrap().unwrap();
    assert_eq!(
        persisted.task.runtime,
        Some(SandboxRuntimeRef {
            vm_id: vm,
            runtime_id: runtime
        })
    );
    assert_eq!(persisted.task.project_id, project);
    assert_eq!(
        reopened.sandbox_summaries(project, None, None).unwrap()[0].runtime,
        persisted.task.runtime
    );
}

#[tokio::test]
async fn sandbox_policy_validation_is_atomic_and_restart_retains_revocation() {
    let f = fixture().await;
    let host = Uuid::new_v4();
    let id = Uuid::new_v4();
    let valid = grant("project", id, &["read"]);
    policy(&f, host, vec![valid.clone()]).await;
    for grants in [
        vec![valid.clone(), valid.clone()],
        vec![grant("project", Uuid::nil(), &["read"])],
        vec![grant("vm", id, &["apply"])],
        vec![grant("connection", id, &[])],
        vec![
            json!({"resource":{"kind":"source","path":"/path-does-not-exist"},"permissions":["read"]}),
        ],
        vec![
            json!({"resource":{"kind":"source","path":f.dir.path().canonicalize().unwrap()},"permissions":["execute"]}),
        ],
    ] {
        assert_eq!(
            call(
                &f.app,
                "PUT",
                &format!("/api/ai-sandbox/access/{host}"),
                json!({"grants":grants})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(f.state.store.sandbox_access(host).unwrap().grants.len(), 1);
    }
    let path = f.dir.path().join("state.db");
    let reopened = crate::persistence::Store::open(&path).unwrap();
    assert_eq!(reopened.sandbox_access(host).unwrap().grants.len(), 1);
    assert_eq!(
        call(
            &f.app,
            "GET",
            &format!("/api/ai-sandbox/access/{host}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &f.app,
            "DELETE",
            &format!("/api/ai-sandbox/access/{host}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(
        crate::persistence::Store::open(&path)
            .unwrap()
            .sandbox_access(host)
            .unwrap()
            .grants
            .is_empty()
    );
}

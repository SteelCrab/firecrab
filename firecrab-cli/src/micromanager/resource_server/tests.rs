use super::*;
#[test]
fn only_loopback_hosts_and_trusted_browser_origins_are_accepted() {
    let home = tempfile::tempdir().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:5524"));
    assert!(allowed(&headers, home.path()));
    headers.insert("origin", HeaderValue::from_static("http://localhost:5523"));
    assert!(allowed(&headers, home.path()));
    headers.insert("origin", HeaderValue::from_static("https://evil.example"));
    assert!(!allowed(&headers, home.path()));
    headers.remove("origin");
    headers.insert("host", HeaderValue::from_static("evil.example:5524"));
    assert!(!allowed(&headers, home.path()));
}

#[test]
fn private_resource_control_accepts_only_its_own_host_and_browser_ports() {
    let directory = tempfile::tempdir().unwrap();
    let instance = super::super::dev::instance::test_instance(
        directory.path(),
        "0123456789abcdef",
        45121,
        45122,
    );
    let mut headers = HeaderMap::new();
    headers.insert("host", "127.0.0.1:45122".parse().unwrap());
    for origin in ["http://localhost:45121", "http://127.0.0.1:45121"] {
        headers.insert("origin", origin.parse().unwrap());
        assert!(allowed(&headers, &instance.managed_home));
    }
    for origin in [
        "http://localhost:8080",
        "http://localhost:5523",
        "http://localhost:8098",
        "http://localhost:45123",
    ] {
        headers.insert("origin", origin.parse().unwrap());
        assert!(!allowed(&headers, &instance.managed_home));
    }
    fs::create_dir_all(instance.managed_home.join("runtime")).unwrap();
    fs::write(
        instance.managed_home.join("runtime/dev-dashboard.json"),
        json!({
            "source": instance.source, "session": uuid::Uuid::new_v4(), "port": 8098,
            "api_port": instance.api_port, "resource_port": instance.resource_port,
            "isolated": true
        })
        .to_string(),
    )
    .unwrap();
    headers.insert("origin", "http://localhost:8098".parse().unwrap());
    assert!(allowed(&headers, &instance.managed_home));
    headers.insert("host", "127.0.0.1:5524".parse().unwrap());
    assert!(!allowed(&headers, &instance.managed_home));
}
use axum::body::{Body, to_bytes};
use axum::http::Request;
use serde_json::{Value, json};
use tower::ServiceExt;

#[tokio::test]
async fn vm_agent_operations_reject_damaged_private_identity_without_falling_back() {
    use super::super::agents;
    use futures_util::FutureExt;
    let root = tempfile::tempdir().unwrap();
    // Guest::connect must fail while reading private state, before any request to the primary API.
    fs::write(
        root.path().join("dev-instance.json"),
        b"invalid-private-identity",
    )
    .unwrap();
    let controller = Arc::new(Controller {
        home: root.path().into(),
        platform: resource::tests::platform(),
        agents: Mutex::new(()),
        editors: Mutex::new(()),
        skills: Mutex::new(()),
        skill_source: closed_source(),
    });
    let state = || State(controller.clone());
    let vm = uuid::Uuid::new_v4();
    let id = uuid::Uuid::new_v4();
    let cli = || {
        Json(VmCli {
            cli: "codex".into(),
            id: Some(id),
        })
    };
    let operations = [
        ("status", vm_agent_status(state(), RoutePath(vm)).boxed()),
        (
            "automation status",
            vm_automation_status(state(), RoutePath((vm, id))).boxed(),
        ),
        (
            "editor status",
            vm_editor_status(state(), RoutePath((vm, id))).boxed(),
        ),
        (
            "open editor",
            open_vm_editor(state(), RoutePath((vm, id))).boxed(),
        ),
        (
            "stop editor",
            stop_vm_editor(state(), RoutePath((vm, id))).boxed(),
        ),
        (
            "automation",
            run_vm_automation(
                state(),
                RoutePath((vm, id)),
                Json(agents::automation::Request {
                    action: "run".into(),
                    id,
                    config: Default::default(),
                }),
            )
            .boxed(),
        ),
        (
            "start task",
            start_vm_agent_task(
                state(),
                RoutePath(vm),
                Json(agents::TaskStart {
                    id,
                    input: "test task".into(),
                    cwd: String::new(),
                    repository: String::new(),
                    branch: String::new(),
                    cli: "codex".into(),
                    model: String::new(),
                }),
            )
            .boxed(),
        ),
        (
            "connect GitHub",
            connect_vm_github(state(), RoutePath(vm)).boxed(),
        ),
        (
            "repositories",
            vm_github_repositories(state(), RoutePath(vm)).boxed(),
        ),
        (
            "project",
            setup_vm_project(
                state(),
                RoutePath(vm),
                Json(agents::project::Setup {
                    repository: "owner/repo".into(),
                    branch: String::new(),
                }),
            )
            .boxed(),
        ),
        (
            "prepare",
            prepare_vm_agents(state(), RoutePath(vm), cli()).boxed(),
        ),
        (
            "login",
            login_vm_agent(state(), RoutePath(vm), cli()).boxed(),
        ),
        (
            "resume",
            resume_vm_agent_task(state(), RoutePath((vm, id))).boxed(),
        ),
        (
            "inspect",
            inspect_vm_agent(state(), RoutePath((vm, id))).boxed(),
        ),
        (
            "input",
            input_vm_agent(
                state(),
                RoutePath((vm, id)),
                Json(agents::Input {
                    text: Some("hello".into()),
                    key: None,
                }),
            )
            .boxed(),
        ),
        (
            "close",
            close_vm_agent(state(), RoutePath((vm, id))).boxed(),
        ),
    ];
    for (name, operation) in operations {
        let error = operation.await.expect_err(name);
        assert!(
            error.to_string().contains("expected value"),
            "{name}: {error}"
        );
        assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn concurrent_vm_mutations_return_conflict_without_entering_the_guest() {
    use futures_util::FutureExt;
    let root = tempfile::tempdir().unwrap();
    let controller = Arc::new(Controller {
        home: root.path().into(),
        platform: resource::tests::platform(),
        agents: Mutex::new(()),
        editors: Mutex::new(()),
        skills: Mutex::new(()),
        skill_source: closed_source(),
    });
    let (ready, locked) = tokio::sync::oneshot::channel();
    let (release, finish) = std::sync::mpsc::channel();
    let shared = controller.clone();
    let worker = tokio::task::spawn_blocking(move || {
        let _held = shared.agents.lock().unwrap();
        ready.send(()).unwrap();
        let _ = finish.recv();
    });
    locked.await.unwrap();
    let state = || State(controller.clone());
    let vm = uuid::Uuid::new_v4();
    let id = uuid::Uuid::new_v4();
    let operations = [
        prepare_vm_agents(
            state(),
            RoutePath(vm),
            Json(VmCli {
                cli: "codex".into(),
                id: None,
            }),
        )
        .boxed(),
        login_vm_agent(
            state(),
            RoutePath(vm),
            Json(VmCli {
                cli: "codex".into(),
                id: Some(id),
            }),
        )
        .boxed(),
        resume_vm_agent_task(state(), RoutePath((vm, id))).boxed(),
        input_vm_agent(
            state(),
            RoutePath((vm, id)),
            Json(super::super::agents::Input {
                text: Some("test".into()),
                key: None,
            }),
        )
        .boxed(),
        close_vm_agent(state(), RoutePath((vm, id))).boxed(),
    ];
    for operation in operations {
        assert_eq!(
            operation.await.unwrap_err().into_response().status(),
            StatusCode::CONFLICT
        );
    }
    release.send(()).unwrap();
    worker.await.unwrap();
    assert!(!root.path().join("runtime/agents").exists());
}

#[tokio::test]
async fn agent_terminal_upgrade_requires_the_origin_controller_and_bounded_size() {
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{Error as WsError, client::IntoClientRequest},
    };
    let dir = tempfile::tempdir().unwrap();
    let id = resource::controller_id(dir.path()).unwrap();
    let app = router_for(dir.path().to_owned(), resource::tests::platform());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let vm = uuid::Uuid::new_v4();
    let agent = uuid::Uuid::new_v4();
    for (controller, origin, cols, expected) in [
        (
            "wrong-controller",
            Some(ORIGINS[0]),
            100,
            StatusCode::CONFLICT,
        ),
        (id.as_str(), None, 100, StatusCode::FORBIDDEN),
        (
            id.as_str(),
            Some("https://evil.example"),
            100,
            StatusCode::FORBIDDEN,
        ),
        (id.as_str(), Some(ORIGINS[0]), 0, StatusCode::BAD_REQUEST),
        (id.as_str(), Some(ORIGINS[0]), 401, StatusCode::BAD_REQUEST),
    ] {
        let mut request = format!("ws://127.0.0.1:{port}/api/service/agents/microvms/{vm}/{agent}/terminal?controller={controller}&cols={cols}&rows=24").into_client_request().unwrap();
        request
            .headers_mut()
            .insert("host", "127.0.0.1:5524".parse().unwrap());
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert("origin", origin.parse().unwrap());
        }
        let Err(WsError::Http(response)) = connect_async(request).await else {
            panic!("Terminal must reject invalid authority before touching a MicroVM");
        };
        assert_eq!(response.status(), expected);
    }
    server.abort();
}

fn request(method: &str, path: &str, id: Option<&str>, body: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:5524")
        .header("origin", ORIGINS[0])
        .header("content-type", "application/json");
    if let Some(id) = id {
        builder = builder.header("x-firecrab-controller-id", id);
    }
    builder.body(Body::from(body.to_owned())).unwrap()
}

async fn response(app: Router, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let result = app.oneshot(request).await.unwrap();
    let status = result.status();
    let headers = result.headers().clone();
    let bytes = to_bytes(result.into_body(), 16384).await.unwrap();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, headers, body)
}

#[tokio::test]
async fn local_requests_read_save_and_validate_the_same_managed_home() {
    let dir = tempfile::tempdir().unwrap();
    let platform = resource::tests::platform();
    let app = router_for(dir.path().to_owned(), platform);
    let path = "/api/service/resources";
    let (status, headers, current) = response(app.clone(), request("GET", path, None, "")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["access-control-allow-origin"], ORIGINS[0]);
    assert_eq!(headers["cache-control"], "no-store");
    let id = current["controllerId"].as_str().unwrap();
    let mut settings = current["settings"].clone();
    settings["cpu"] = json!(6);
    let (status, _, saved) = response(
        app.clone(),
        request("PUT", path, Some(id), &settings.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["settings"], settings);
    assert_eq!(saved["pending"], true);
    let bytes = fs::read(dir.path().join("resources.json")).unwrap();
    settings["cpu"] = json!(0);
    let (status, _, error) = response(
        app.clone(),
        request("PUT", path, Some(id), &settings.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(error["error"].as_str().unwrap().contains("CPU"));
    assert_eq!(fs::read(dir.path().join("resources.json")).unwrap(), bytes);
    let held = resource::lock(dir.path()).unwrap();
    settings["cpu"] = json!(4);
    let (status, _, _) = response(
        app.clone(),
        request("PUT", path, Some(id), &settings.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _, _) = response(
        app,
        request("POST", "/api/service/resources/apply", Some(id), "{}"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    drop(held);
}

#[tokio::test]
async fn untrusted_hosts_origins_and_controller_ids_cannot_mutate_settings() {
    let dir = tempfile::tempdir().unwrap();
    let app = router_for(dir.path().to_owned(), resource::tests::platform());
    let path = "/api/service/resources";
    for (header, value) in [
        ("host", "evil.example:5524"),
        ("origin", "https://evil.example"),
    ] {
        let mut req = request("GET", path, None, "");
        req.headers_mut()
            .insert(header, HeaderValue::from_str(value).unwrap());
        assert_eq!(response(app.clone(), req).await.0, StatusCode::FORBIDDEN);
    }
    for id in [None, Some("another-host")] {
        let (status, headers, _) = response(app.clone(), request("PUT", path, id, "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(headers["cache-control"], "no-store");
    }
    let (status, headers, _) = response(app.clone(), request("OPTIONS", path, None, "")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        headers["access-control-allow-methods"],
        "GET, PUT, POST, OPTIONS"
    );
    assert!(
        headers["access-control-allow-headers"]
            .to_str()
            .unwrap()
            .contains("X-Firecrab-Controller-Id")
    );
    assert!(!dir.path().join("resources.json").exists());
    fs::write(
        dir.path().join("runtime/resource-controller-id.json"),
        b"corrupt",
    )
    .unwrap();
    assert_eq!(
        response(app, request("PUT", path, Some("stale"), "{}"))
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn saved_dashboard_port_allows_only_its_loopback_browser_origins() {
    let dir = tempfile::tempdir().unwrap();
    let app = router_for(dir.path().to_owned(), resource::tests::platform());
    let path = "/api/service/resources";
    let runtime = dir.path().join("runtime");
    fs::create_dir_all(&runtime).unwrap();
    let state = runtime.join("dev-dashboard.json");
    fs::write(
        &state,
        json!({
            "source": dir.path(), "session": uuid::Uuid::new_v4(),
            "dependencies": "fixture", "port": 8098
        })
        .to_string(),
    )
    .unwrap();
    for (origin, expected) in [
        ("http://localhost:8098", StatusCode::NO_CONTENT),
        ("http://127.0.0.1:8098", StatusCode::NO_CONTENT),
        ("http://localhost:8099", StatusCode::FORBIDDEN),
        ("http://evil.example:8098", StatusCode::FORBIDDEN),
        ("https://localhost:8098", StatusCode::FORBIDDEN),
    ] {
        let mut req = request("OPTIONS", path, None, "");
        req.headers_mut().insert("origin", origin.parse().unwrap());
        let (status, headers, _) = response(app.clone(), req).await;
        assert_eq!(status, expected, "{origin}");
        if expected == StatusCode::NO_CONTENT {
            assert_eq!(headers["access-control-allow-origin"], origin);
        }
    }
    for invalid in [Some("invalid session"), None] {
        match invalid {
            Some(value) => fs::write(&state, value).unwrap(),
            None => fs::remove_file(&state).unwrap(),
        }
        let mut req = request("OPTIONS", path, None, "");
        req.headers_mut()
            .insert("origin", "http://localhost:8098".parse().unwrap());
        assert_eq!(response(app.clone(), req).await.0, StatusCode::FORBIDDEN);
    }
}

#[tokio::test]
async fn invalid_bodies_and_broken_state_return_errors_without_starting_a_worker() {
    let dir = tempfile::tempdir().unwrap();
    let platform = resource::tests::platform();
    let app = router_for(dir.path().to_owned(), platform);
    let id = resource::controller_id(dir.path()).unwrap();
    let path = "/api/service/resources";
    assert_eq!(
        response(
            app.clone(),
            request(
                "POST",
                "/api/service/resources/apply",
                Some(&id),
                "{\"unexpected\":true}"
            )
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        response(
            app.clone(),
            request("PUT", path, Some(&id), &" ".repeat(4097))
        )
        .await
        .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        response(app.clone(), request("PUT", path, Some(&id), "{broken}"))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert!(!dir.path().join("runtime/resource-operation.json").exists());
    fs::write(dir.path().join("resources.json"), b"broken").unwrap();
    assert_eq!(
        response(app, request("GET", path, None, "")).await.0,
        StatusCode::BAD_REQUEST
    );
    let offline = resource::Platform {
        capacity: |_| Err(Error::Invalid("capacity probe failed".into())),
        ..platform
    };
    let (status, _, body) = response(
        router_for(dir.path().to_owned(), offline),
        request("GET", path, None, ""),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "capacity probe failed");
}

#[tokio::test]
async fn accepted_apply_is_persistent_and_blocks_duplicate_requests() {
    let dir = tempfile::tempdir().unwrap();
    let app = router_for(dir.path().to_owned(), resource::tests::platform());
    let id = resource::controller_id(dir.path()).unwrap();
    let path = "/api/service/resources/apply";
    #[cfg(unix)]
    let mut command = Command::new("/bin/sh");
    #[cfg(unix)]
    command.args(["-c", "exit 0"]);
    #[cfg(windows)]
    let mut command = Command::new("cmd.exe");
    #[cfg(windows)]
    command.args(["/C", "exit /B 0"]);
    let operation = spawn_worker(dir.path(), resource::tests::platform(), command).unwrap();
    assert_eq!(operation.state, "applying");
    assert!(uuid::Uuid::parse_str(&operation.id).is_ok());
    assert!(dir.path().join("runtime/resource-apply.log").exists());
    let stored: ManagerResourceOperation =
        resource::read(&dir.path().join("runtime/resource-operation.json"))
            .unwrap()
            .unwrap();
    assert_eq!(stored.id, operation.id);
    assert_eq!(
        response(app, request("POST", path, Some(&id), "{}"))
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn status_exposes_unsupported_wsl_networking_without_replacing_saved_resources() {
    let dir = tempfile::tempdir().unwrap();
    let platform = resource::Platform {
        os: firecrab_api_types::HostOs::Windows,
        defaults: |_, _| {
            Err(Error::Invalid(
                "WSL network mode virtioproxy is not supported by resource management".into(),
            ))
        },
        ..resource::tests::platform()
    };
    let app = router_for(dir.path().to_owned(), platform);
    let (status, _, body) = response(
        app.clone(),
        request("GET", "/api/service/resources", None, ""),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("virtioproxy"));
    let saved = resource::tests::platform()
        .status(dir.path())
        .unwrap()
        .settings;
    resource::write(&dir.path().join("resources.json"), &saved).unwrap();
    let (status, _, body) = response(app, request("GET", "/api/service/resources", None, "")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["settings"]["cpu"], saved.cpu);
    assert!(body["sharedWsl"].as_bool().unwrap());
}

#[test]
fn worker_spawn_failure_is_recorded_and_allows_retry() {
    let dir = tempfile::tempdir().unwrap();
    let platform = resource::tests::platform();
    let error = spawn_worker(
        dir.path(),
        platform,
        Command::new(dir.path().join("missing-executable")),
    )
    .unwrap_err();
    assert!(matches!(error, Error::Io(_)));
    let status = platform.status(dir.path()).unwrap();
    assert_eq!(status.operation.unwrap().state, "failed");
    assert!(status.pending);
    assert!(platform.save(dir.path(), &status.settings).is_ok());
}

#[test]
fn resource_server_rejects_relative_and_missing_homes() {
    assert!(run(Some(PathBuf::from("relative-home"))).is_err());
    let dir = tempfile::tempdir().unwrap();
    assert!(run(Some(dir.path().join("missing"))).is_err());
}

#[tokio::test]
async fn native_agent_controls_are_scoped_to_the_selected_controller() {
    let dir = tempfile::tempdir().unwrap();
    let app = router_for(dir.path().into(), resource::tests::platform());
    let (status, _, body) =
        response(app.clone(), request("GET", "/api/service/agents", None, "")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["agents"].as_array().unwrap().is_empty());
    let id = body["controllerId"].as_str().unwrap();
    let path = format!("/api/service/agents/{}/input", uuid::Uuid::new_v4());
    let (status, _, _) = response(
        app.clone(),
        request(
            "POST",
            &path,
            Some("other-controller"),
            r#"{"text":"do not run"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _, _) = response(
        app.clone(),
        request("POST", &path, Some(id), r#"{"text":"do not run"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let mut foreign = request("GET", "/api/service/agents", None, "");
    foreign.headers_mut().insert(
        "origin",
        HeaderValue::from_static("https://foreign.example"),
    );
    let (status, _, _) = response(app.clone(), foreign).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = response(
        app.clone(),
        request(
            "POST",
            "/api/service/agents",
            Some(id),
            r#"{"unknown":"value"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    for suffix in ["tasks", "prepare", "login", "github", "project"] {
        let path = format!(
            "/api/service/agents/microvms/{}/{suffix}",
            uuid::Uuid::new_v4()
        );
        let (status, _, _) = response(
            app.clone(),
            request("POST", &path, Some("another-controller"), r#"{}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }
    for action in ["automation", "editor", "editor/stop"] {
        let path = format!(
            "/api/service/agents/microvms/{}/{}/{action}",
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4()
        );
        let (status, _, _) = response(
            app.clone(),
            request("POST", &path, Some("another-controller"), r#"{}"#),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }
    let task_path = "/api/service/agents/tasks";
    let empty_task =
        json!({"id":uuid::Uuid::new_v4(),"input":"  ","cwd":dir.path(),"cli":"","model":""});
    let (status, _, body) = response(
        app,
        request("POST", task_path, Some(id), &empty_task.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("Enter a task"));
    assert!(!dir.path().join("runtime/agents/sessions.json").exists());
}

fn closed_source() -> crate::micromanager::skills::source::Source {
    crate::micromanager::skills::source::Source {
        api: "http://127.0.0.1:9".into(),
        raw: "http://127.0.0.1:9".into(),
    }
}

#[tokio::test]
async fn skill_routes_require_the_controller_id_for_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let app = router_with_source(
        dir.path().into(),
        resource::tests::platform(),
        closed_source(),
    );
    let (status, _, _) = response(
        app,
        request(
            "POST",
            "/api/service/agents/skills/webapp-testing/import",
            None,
            "{}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn vm_skill_toggle_is_saved_while_the_vm_is_unreachable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("runtime/skills")).unwrap();
    std::fs::write(
        dir.path().join("runtime/skills/imported.json"),
        serde_json::json!([{"name":"x","description":"","commit":"c","license":"apache2","digest":"d","files":[],"importedAt":0}]).to_string(),
    )
    .unwrap();
    let id = resource::controller_id(dir.path()).unwrap();
    let vm = uuid::Uuid::new_v4();
    let app = router_with_source(
        dir.path().into(),
        resource::tests::platform(),
        closed_source(),
    );
    let (status, _, body) = response(
        app,
        request(
            "POST",
            &format!("/api/service/agents/microvms/{vm}/skills"),
            Some(&id),
            r#"{"name":"x","enabled":true}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "pending");
    assert_eq!(body["enabled"], serde_json::json!(["x"]));
}

#[tokio::test]
async fn rule_routes_reject_unknown_fields() {
    let dir = tempfile::tempdir().unwrap();
    let id = resource::controller_id(dir.path()).unwrap();
    let vm = uuid::Uuid::new_v4();
    let app = router_with_source(
        dir.path().into(),
        resource::tests::platform(),
        closed_source(),
    );
    let (status, _, _) = response(
        app,
        request(
            "POST",
            &format!("/api/service/agents/microvms/{vm}/rules/apply"),
            Some(&id),
            r#"{"mode":"add","text":"- x","digest":"d","extra":1}"#,
        ),
    )
    .await;
    assert!(status.is_client_error(), "{status}");
    assert_ne!(status, StatusCode::NOT_FOUND);
    assert_ne!(status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn static_skill_and_rule_paths_do_not_hit_agent_id_routes() {
    let dir = tempfile::tempdir().unwrap();
    let app = router_with_source(
        dir.path().into(),
        resource::tests::platform(),
        closed_source(),
    );
    let (status, _, body) =
        response(app, request("GET", "/api/service/agents/skills", None, "")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["error"].is_string());
    assert_eq!(body["repository"], "anthropics/skills");
}

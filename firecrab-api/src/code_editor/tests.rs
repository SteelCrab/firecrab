use super::*;
use crate::{
    ai_sandbox::access::LocalOperator,
    persistence::{code_runtime::Admission, code_workspaces::WriteResult},
    server::{HttpConfig, build_router},
};
use axum::{
    body::{Body, to_bytes},
    http::{Request, header},
};
use firecrab_api_types::*;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

struct Fixture {
    _dir: tempfile::TempDir,
    state: AppState,
    app: Router,
    id: Uuid,
    record: Record,
}
async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let templates =
        crate::templates::TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
    let mut state = AppState::with_db_file(templates, dir.path().join("state.db"))
        .await
        .unwrap();
    state.code_jobs = Arc::new(tokio::sync::Semaphore::new(0));
    let id = Uuid::new_v4();
    state
        .store
        .put_pipe_auth(&crate::pipe_auth::SavedAuth::Github {
            username: "octocat".into(),
            token: "not-an-editor-credential".into(),
        })
        .unwrap();
    let spec=serde_json::from_value(json!({"name":"web-app","github":{"account":"octocat","owner":"team","repository":"web-app","branch":"main"},"runtime":"python","resources":{"cpu":2,"ram":4096,"diskGb":16},"agent":null})).unwrap();
    assert!(matches!(
        state
            .store
            .write_code_workspace(
                id,
                CodeWorkspaceWrite {
                    expected_revision: 0,
                    spec
                },
                1
            )
            .unwrap(),
        WriteResult::Written(_)
    ));
    let network =
        crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);
    let placement = CodeRuntimePlacement {
        template: SandboxTemplatePin {
            name: "dev".into(),
            version: "v1".into(),
            kernel_sha256: "a".repeat(64),
            rootfs_sha256: "b".repeat(64),
            initrd_sha256: None,
            boot_args_sha256: "c".repeat(64),
        },
        micro_network_id: network,
        storage_root: state.storage.default_id().into(),
    };
    let Admission::Accepted(mut r) = state
        .store
        .admit_code_runtime(id, 1, Some(placement), CodeRuntimeAction::Start)
        .unwrap()
    else {
        panic!()
    };
    r.vm_created = true;
    r.runtime.phase = CodeRuntimePhase::Ready;
    r.runtime.runtime_id = Some(Uuid::new_v4());
    r.runtime.initial_commit = Some("a".repeat(40));
    assert!(state.store.save_code_runtime(&r).unwrap());
    let mut vm = crate::handlers::vms::test_support::record(
        &format!("code-{}", id.simple()),
        r.runtime.vm_id,
    );
    vm.template = "dev".into();
    vm.template_version = "v1".into();
    vm.template_kernel_sha256 = "a".repeat(64);
    vm.template_rootfs_sha256 = "b".repeat(64);
    vm.template_boot_args_sha256 = "c".repeat(64);
    vm.cpu = 2;
    vm.ram = 4096;
    vm.disk_gb = 16;
    vm.micro_network_id = network;
    vm.storage_root = r.runtime.placement.storage_root.clone();
    vm.state = VmState::Running;
    vm.last_runtime_id = r.runtime.runtime_id;
    vm.egress_policy = EgressPolicy::Internet;
    state
        .store
        .allocate_lease(
            vm.id,
            crate::ipam::SubnetSpec {
                micro_network_id: network,
                network: "172.31.0.0".parse().unwrap(),
                prefix: 24,
                ipv6: None,
            },
        )
        .unwrap();
    state.vms.lock().unwrap().insert(vm.id, vm);
    let app = build_router(
        state.clone(),
        &HttpConfig {
            bind_addr: "127.0.0.1:5523".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 32,
            request_timeout: Duration::from_secs(3),
            static_root: None,
        },
    )
    .layer(Extension(LocalOperator));
    Fixture {
        _dir: dir,
        state,
        app,
        id,
        record: *r,
    }
}
fn body(f: &Fixture) -> Value {
    json!({"expectedRevision":1,"vmId":f.record.runtime.vm_id,"runtimeId":f.record.runtime.runtime_id,"operationId":f.record.runtime.operation_id})
}
async fn request(
    f: &Fixture,
    method: &str,
    path: &str,
    host: &str,
    cookie: Option<&str>,
    origin: Option<&str>,
    body: Value,
) -> axum::response::Response {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", host)
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    if let Some(origin) = origin {
        request = request.header("origin", origin);
    }
    f.app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}
async fn prepared(f: &Fixture) -> Arc<Session> {
    let response = request(
        f,
        "POST",
        &format!("/api/code/workspaces/{}/editor", f.id),
        "localhost:5523",
        None,
        None,
        body(f),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::ACCEPTED,
        "{}",
        String::from_utf8_lossy(&to_bytes(response.into_body(), 8192).await.unwrap())
    );
    find(&f.state, f.id).unwrap()
}
fn host(f: &Fixture, s: &Session) -> String {
    format!("{}-{}.code.localhost:5523", f.id.simple(), s.site)
}

#[tokio::test]
async fn code_editor_admission_requires_current_revision_runtime_and_fixed_request_fields() {
    let f = fixture().await;
    let path = format!("/api/code/workspaces/{}/editor", f.id);
    for (field, value) in [
        ("expectedRevision", json!(2)),
        ("vmId", json!(Uuid::new_v4())),
        ("runtimeId", json!(Uuid::new_v4())),
        ("operationId", json!(Uuid::new_v4())),
    ] {
        let mut b = body(&f);
        b[field] = value;
        assert_eq!(
            request(&f, "POST", &path, "localhost:5523", None, None, b)
                .await
                .status(),
            StatusCode::CONFLICT
        );
    }
    let mut extra = body(&f);
    extra["command"] = json!("bad");
    assert_eq!(
        request(&f, "POST", &path, "localhost:5523", None, None, extra)
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let first = prepared(&f).await;
    let second = prepared(&f).await;
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(*first.phase.lock().unwrap(), "preparing");
    assert_eq!(f.state.code_editor.entries.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn code_editor_one_use_handoff_is_host_bound_and_cannot_reach_controller_routes() {
    let f = fixture().await;
    let s = prepared(&f).await;
    *s.phase.lock().unwrap() = "ready";
    let response = request(
        &f,
        "GET",
        &format!("/api/code/workspaces/{}/editor/open", f.id),
        "localhost:5523",
        None,
        None,
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let url = reqwest::Url::parse(response.headers()[header::LOCATION].to_str().unwrap()).unwrap();
    assert_eq!(
        url.host_str(),
        Some(host(&f, &s).split(':').next().unwrap())
    );
    let response = request(
        &f,
        "GET",
        url.path(),
        &host(&f, &s),
        None,
        None,
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(
        cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Lax")
            && !cookie.contains("Domain=")
    );
    let cookie = cookie.split(';').next().unwrap();
    assert_eq!(
        request(
            &f,
            "GET",
            url.path(),
            &host(&f, &s),
            None,
            None,
            Value::Null
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    for path in [
        "/api/code/workspaces",
        "/api/pipe/ai/connections",
        "/ws/vms/none/console",
    ] {
        assert_eq!(
            request(
                &f,
                "GET",
                path,
                &host(&f, &s),
                Some(cookie),
                None,
                Value::Null
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        request(
            &f,
            "GET",
            "/api/code/workspaces",
            "localhost:5523",
            None,
            Some(&format!("http://{}", host(&f, &s))),
            Value::Null
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &f,
            "GET",
            "/_fc/terminal",
            &host(&f, &s),
            Some(cookie),
            Some("null"),
            Value::Null
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &f,
            "GET",
            "/_fc/terminal",
            &host(&f, &s),
            None,
            None,
            Value::Null
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    let response = request(
        &f,
        "GET",
        "/_fc/terminal",
        &host(&f, &s),
        Some(cookie),
        None,
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
}
#[tokio::test]
async fn code_editor_proxy_preserves_files_but_never_forwards_browser_authentication() {
    let f = fixture().await;
    let s = prepared(&f).await;
    *s.phase.lock().unwrap() = "ready";
    let listener = tokio::net::UnixListener::bind(&s.socket).unwrap();
    let worker = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = vec![0; 8192];
        let n = socket.read(&mut bytes).await.unwrap();
        let headers = String::from_utf8_lossy(&bytes[..n]);
        assert!(headers.starts_with("GET /file?name=hello%20world HTTP/1.1"));
        assert!(!headers.to_ascii_lowercase().contains("fc_code_session"));
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nSet-Cookie: guest=unsafe\r\nConnection: close\r\n\r\neditor bytes").await.unwrap();
    });
    let response = request(
        &f,
        "GET",
        "/file?name=hello%20world",
        &host(&f, &s),
        Some(&format!("fc_code_session={}", s.cookie)),
        None,
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key(header::SET_COOKIE));
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap(),
        "editor bytes"
    );
    worker.await.unwrap();
}
#[tokio::test]
async fn code_editor_runtime_changes_revoke_the_browser_session() {
    let f = fixture().await;
    let s = prepared(&f).await;
    let stop = s.cancel.subscribe();
    f.state
        .store
        .admit_code_runtime(f.id, 1, None, CodeRuntimeAction::Stop)
        .unwrap();
    assert!(find(&f.state, f.id).is_none());
    assert!(*stop.borrow());
    assert_eq!(
        request(
            &f,
            "GET",
            "/_fc/terminal",
            &host(&f, &s),
            Some(&format!("fc_code_session={}", s.cookie)),
            None,
            Value::Null
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
}
#[test]
fn code_editor_requests_reject_credentials_and_unknown_phases() {
    let value = json!({"expectedRevision":1,"vmId":Uuid::new_v4(),"runtimeId":Uuid::new_v4(),"operationId":Uuid::new_v4(),"apiKey":"secret"});
    assert!(serde_json::from_value::<CodeEditorPrepare>(value).is_err());
    assert!(serde_json::from_value::<CodeEditorPhase>(json!("authenticated")).is_err());
}

#[tokio::test]
async fn code_editor_websocket_proxy_streams_both_directions_and_closes_on_revocation() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
    let f = fixture().await;
    let s = prepared(&f).await;
    *s.phase.lock().unwrap() = "ready";
    let unix = tokio::net::UnixListener::bind(&s.socket).unwrap();
    let upstream = tokio::spawn(async move {
        let (stream, _) = unix.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(message)) = ws.next().await {
            if message.is_close() {
                break;
            }
            if ws.send(message).await.is_err() {
                break;
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = f.app.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut req = format!("ws://127.0.0.1:{port}/remote?token=guest")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("host", host(&f, &s).parse().unwrap());
    req.headers_mut().insert(
        "origin",
        format!("http://{}", host(&f, &s)).parse().unwrap(),
    );
    req.headers_mut().insert(
        "cookie",
        format!("fc_code_session={}", s.cookie).parse().unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    ws.send(Message::Binary(vec![0, 255, 3, 4].into()))
        .await
        .unwrap();
    assert_eq!(
        ws.next().await.unwrap().unwrap(),
        Message::Binary(vec![0, 255, 3, 4].into())
    );
    ws.send(Message::Text("terminal handshake".into()))
        .await
        .unwrap();
    assert_eq!(
        ws.next().await.unwrap().unwrap(),
        Message::Text("terminal handshake".into())
    );
    s.cancel.send(true).unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(3), ws.next())
        .await
        .unwrap();
    assert!(ended.is_none() || ended.is_some_and(|v| v.is_err() || v.unwrap().is_close()));
    server.abort();
    upstream.abort();
}

/// Manual protocol QA against a disposable guest service. No live VM or stored credential is used.
#[tokio::test]
#[ignore = "requires an isolated code-server guest exposed at 127.0.0.1:62927"]
async fn code_editor_live_browser_gateway_qa() {
    let f = fixture().await;
    let s = prepared(&f).await;
    *s.phase.lock().unwrap() = "ready";
    let listener = tokio::net::UnixListener::bind(&s.socket).unwrap();
    let bridge = tokio::spawn(async move {
        loop {
            let Ok((mut input, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let Ok(mut output) = tokio::net::TcpStream::connect("127.0.0.1:62927").await else {
                    return;
                };
                let _ = tokio::io::copy_bidirectional(&mut input, &mut output).await;
            });
        }
    });
    let config = HttpConfig {
        bind_addr: "127.0.0.1:62926".parse().unwrap(),
        allowed_origins: vec![],
        max_concurrent_requests: 32,
        request_timeout: Duration::from_secs(10),
        static_root: Some(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../firecrab-frontend/dist"),
        ),
    };
    let app = build_router(f.state.clone(), &config).layer(Extension(LocalOperator));
    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .unwrap();
    println!(
        "CODE_EDITOR_QA_URL=http://127.0.0.1:62926/#/code/{}/editor",
        f.id
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(Duration::from_secs(300)).await;
    server.abort();
    bridge.abort();
}

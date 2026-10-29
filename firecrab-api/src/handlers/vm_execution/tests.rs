use super::*;
use crate::{
    console::ConsoleBroker,
    firecracker::VmProcess,
    handlers::vms::test_support::{record, seed_vm},
    server::{HttpConfig, build_router},
    templates::TemplateRegistry,
};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
};
use tower::ServiceExt;

#[derive(Default)]
struct Native {
    paused: bool,
    reject: bool,
    lose_reply: bool,
    unknown: bool,
    delay: bool,
    patches: usize,
}
struct Fixture {
    _root: tempfile::TempDir,
    state: AppState,
    id: Uuid,
    native: Arc<Mutex<Native>>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let templates = TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap();
        let mut state = AppState::with_db_file(templates, root.path().join("state.db"))
            .await
            .unwrap();
        let mut runtime = (*state.runtime).clone();
        runtime.vms_dir = root.path().join("vms");
        state = state.with_test_runtime(runtime);
        let helper = root.path().join("helper.sock");
        crate::network::test_support::spawn_always_ok_helper(&helper);
        state = state.with_test_network(crate::network::NetworkClient::with_socket_path(helper));
        let id = Uuid::new_v4();
        let runtime_id = Uuid::new_v4();
        let runtime = VmArtifactPaths::for_vm(&state.runtime.vms_dir, id)
            .create_runtime(runtime_id)
            .unwrap();
        let listener = UnixListener::bind(runtime.api_socket).unwrap();
        let mut vm = record("pause-test", id);
        vm.state = VmState::Running;
        vm.last_runtime_id = Some(runtime_id);
        seed_vm(&state, &vm);
        let (_exited, exited) = tokio::sync::watch::channel(false);
        state.processes.lock().unwrap().insert(
            id,
            VmProcess {
                pid: 12345,
                exited,
                console: Arc::new(ConsoleBroker::new()),
                control: crate::vm_shim::client::ShimControl::for_test().0,
            },
        );
        let native = Arc::new(Mutex::new(Native::default()));
        let native_server = native.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut chunk = [0; 1024];
                loop {
                    let n = stream.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&request);
                    if let Some((headers, body)) = text.split_once("\r\n\r\n") {
                        let len = headers
                            .lines()
                            .find_map(|line| {
                                line.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|n| n.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if body.len() >= len {
                            break;
                        }
                    }
                }
                let patch = request.starts_with(b"PATCH /vm ");
                let (status, body, lose, delay) = {
                    let mut native = native_server.lock().unwrap();
                    if patch {
                        native.patches += 1;
                        if !native.reject {
                            native.paused =
                                String::from_utf8_lossy(&request).contains("\"Paused\"");
                        }
                    }
                    let status = if patch && native.reject {
                        "400 Bad Request"
                    } else {
                        "200 OK"
                    };
                    let body = if native.unknown {
                        "{}".to_owned()
                    } else {
                        json!({"state": if native.paused { "Paused" } else { "Running" }})
                            .to_string()
                    };
                    (
                        status,
                        body,
                        patch && std::mem::take(&mut native.lose_reply),
                        patch && native.delay,
                    )
                };
                if delay {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                if !lose {
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                }
            }
        });
        Self {
            _root: root,
            state,
            id,
            native,
            server,
        }
    }
    fn app(&self) -> Router {
        build_router(
            self.state.clone(),
            &HttpConfig {
                bind_addr: "127.0.0.1:3000".parse().unwrap(),
                allowed_origins: vec![],
                max_concurrent_requests: 128,
                request_timeout: std::time::Duration::from_secs(10),
                static_root: None,
            },
        )
    }
    async fn action(&self, action: &str) -> (StatusCode, Value) {
        let response = self
            .app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/vms/{}/{action}", self.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }
    fn saved(&self) -> VmState {
        self.state.store.load_all().unwrap()[&self.id].state
    }
}

#[tokio::test]
async fn pause_resume_is_idempotent_and_keeps_runtime_and_lease() {
    let fixture = Fixture::new().await;
    let before = fixture.state.vms.lock().unwrap()[&fixture.id].clone();
    let lease = fixture.state.store.active_lease(fixture.id).unwrap();
    for (action, expected) in [
        ("pause", "paused"),
        ("pause", "paused"),
        ("resume", "running"),
        ("resume", "running"),
    ] {
        let (status, vm) = fixture.action(action).await;
        assert_eq!(status, StatusCode::OK, "{vm}");
        assert_eq!(vm["state"], expected);
        assert_eq!(
            fixture.state.processes.lock().unwrap()[&fixture.id].pid,
            12345
        );
        let after = fixture.state.vms.lock().unwrap()[&fixture.id].clone();
        assert_eq!(after.last_runtime_id, before.last_runtime_id);
        assert_eq!(after.disk_generation, before.disk_generation);
        assert_eq!(fixture.state.store.active_lease(fixture.id).unwrap(), lease);
    }
    assert_eq!(fixture.saved(), VmState::Running);
}

#[tokio::test]
async fn a_lost_patch_reply_is_resolved_from_the_runtime() {
    let fixture = Fixture::new().await;
    fixture.native.lock().unwrap().lose_reply = true;
    assert_eq!(fixture.action("pause").await.0, StatusCode::OK);
    assert_eq!(fixture.saved(), VmState::Paused);
}

#[tokio::test]
async fn a_rejected_patch_does_not_report_a_pause() {
    let fixture = Fixture::new().await;
    fixture.native.lock().unwrap().reject = true;
    assert_eq!(
        fixture.action("pause").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(fixture.saved(), VmState::Running);
}

#[tokio::test]
async fn unconfirmed_state_stays_active_and_can_be_retried() {
    let fixture = Fixture::new().await;
    fixture.native.lock().unwrap().unknown = true;
    assert_eq!(
        fixture.action("pause").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(fixture.saved(), VmState::Pausing);
    assert!(!fixture.saved().can_delete());
    fixture.native.lock().unwrap().unknown = false;
    assert_eq!(fixture.action("resume").await.0, StatusCode::OK);
    assert_eq!(fixture.saved(), VmState::Running);
}

#[tokio::test]
async fn concurrent_pause_and_resume_are_serialized() {
    let fixture = Arc::new(Fixture::new().await);
    fixture.native.lock().unwrap().delay = true;
    let pausing = fixture.clone();
    let pause = tokio::spawn(async move { pausing.action("pause").await });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while fixture.native.lock().unwrap().patches == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("pause must reach the runtime before resume is submitted");
    let (pause, resume) = tokio::join!(pause, fixture.action("resume"));
    assert_eq!(pause.unwrap().0, StatusCode::OK);
    assert_eq!(resume.0, StatusCode::OK);
    assert_eq!(fixture.saved(), VmState::Running);
}

#[tokio::test]
async fn dropping_the_http_caller_does_not_cancel_the_transition() {
    let fixture = Fixture::new().await;
    fixture.native.lock().unwrap().delay = true;
    let state = fixture.state.clone();
    let id = fixture.id.to_string();
    let request = tokio::spawn(pause_vm(
        State(state),
        Extension(RequestId(Uuid::new_v4())),
        Path(id),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while fixture.native.lock().unwrap().patches == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    request.abort();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while fixture.saved() != VmState::Paused {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn paused_vms_reject_start_delete_and_resource_edits_and_can_stop() {
    let fixture = Fixture::new().await;
    assert_eq!(fixture.action("pause").await.0, StatusCode::OK);
    assert_eq!(fixture.action("start").await.0, StatusCode::CONFLICT);
    let response = fixture
        .app()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/vms/{}", fixture.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(!fixture.saved().can_edit_resources());
    assert!(!fixture.saved().can_edit_env());
    // Synthetic VMM has no OS process to stop; runtime shutdown is covered by shim/KVM tests.
    fixture.state.processes.lock().unwrap().remove(&fixture.id);
    assert_eq!(fixture.action("stop").await.0, StatusCode::OK);
    assert_eq!(fixture.saved(), VmState::Stopped);
}

#[tokio::test]
async fn inactive_and_internal_vms_are_rejected_without_touching_runtime() {
    let fixture = Fixture::new().await;
    fixture
        .state
        .vms
        .lock()
        .unwrap()
        .get_mut(&fixture.id)
        .unwrap()
        .state = VmState::Stopped;
    assert_eq!(fixture.action("pause").await.0, StatusCode::CONFLICT);
    {
        let mut vms = fixture.state.vms.lock().unwrap();
        let vm = vms.get_mut(&fixture.id).unwrap();
        vm.state = VmState::Running;
        vm.purpose = crate::model::VmPurpose::Builder;
    }
    assert_eq!(fixture.action("pause").await.0, StatusCode::CONFLICT);
    assert_eq!(fixture.native.lock().unwrap().patches, 0);
}

#[tokio::test]
async fn service_owned_vm_is_rejected_before_changing_runtime() {
    let fixture = Fixture::new().await;
    let deployment: firecrab_api_types::MicroServiceDeployment = serde_json::from_value(json!({
        "id": Uuid::new_v4(), "serviceId": Uuid::new_v4(), "phase": "running",
        "spec": {"name":"owned", "image":"ghcr.io/team/owned:1", "port":8080, "ram":128, "cpu":1},
        "request": {"hostPort":18080}, "digest":null, "log":[], "error":null,
        "instances": [{"vmId":fixture.id, "ipv4":"172.30.0.5", "hostPort":18080, "state":"running", "healthy":true}]
    })).unwrap();
    fixture
        .state
        .store
        .save_service_deployment(&deployment)
        .unwrap();
    let (status, body) = fixture.action("pause").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "vm_service_owned");
    assert_eq!(fixture.native.lock().unwrap().patches, 0);
}

#[tokio::test]
async fn a_paused_record_blocks_sleep_even_before_its_process_is_readopted() {
    let fixture = Fixture::new().await;
    assert_eq!(fixture.action("pause").await.0, StatusCode::OK);
    fixture.state.processes.lock().unwrap().clear();
    let activity = crate::handlers::micromanager::activity(State(fixture.state.clone())).await;
    assert_eq!(serde_json::to_value(activity.0).unwrap()["busy"], true);
}

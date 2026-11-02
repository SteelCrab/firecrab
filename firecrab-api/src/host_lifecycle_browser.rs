//! Ignored evidence harness: actual API/SQLite/React with the real Linux broker over a test proxy.

use super::*;
use firecrab_helper_protocol::{
    PROTOCOL_VERSION,
    framing::{read_frame, write_frame},
    network::{HelperFailure, NetworkRequest, NetworkRequestEnvelope, NetworkResponseEnvelope},
};
use firecrab_stack::{Manager, ResourceOwner};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::io::AsyncWriteExt;

#[tokio::test]
#[ignore = "isolated API/React and actual Linux broker evidence; requires a dedicated prepared Docker fixture"]
async fn iac_ingress_browser_fixture() {
    let container = std::env::var("FIRECRAB_IAC_QA_CONTAINER").unwrap();
    assert!(
        container.starts_with("firecrab-548-ingress-evidence-")
            && container
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    );
    let info = std::path::PathBuf::from(std::env::var_os("FIRECRAB_IAC_QA_INFO").unwrap());
    let assets = std::path::PathBuf::from(std::env::var_os("FIRECRAB_IAC_QA_ASSETS").unwrap());
    assert!(assets.join("index.html").is_file());
    let directory = tempfile::Builder::new()
        .prefix("fc-ingress")
        .tempdir_in("/private/tmp")
        .unwrap();
    let listener = tokio::net::UnixListener::bind(directory.path().join("helper.sock")).unwrap();
    let lose = Arc::new(AtomicBool::new(false));
    let loss = lose.clone();
    let target = container.clone();
    let helper = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let envelope: NetworkRequestEnvelope = read_frame(&mut stream).await.unwrap();
            let mut response = NetworkResponseEnvelope {
                version: PROTOCOL_VERSION,
                request_id: envelope.request_id,
                result: Err(HelperFailure::UnsupportedOperation),
                pipe: None,
                host_iac: None,
            };
            if let NetworkRequest::HostIac { request } = envelope.request {
                let is_change = matches!(request, Request::ChangeOwner { .. });
                let mut child = tokio::process::Command::new("docker")
                    .args([
                        "exec",
                        "-i",
                        &target,
                        "env",
                        "CONFDIR=/fixture/conf",
                    "PATH=/fixture/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                        "/build/target/release/firecrab",
                        "pipe",
                        "host-iac",
                    ])
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .unwrap();
                let mut input = child.stdin.take().unwrap();
                input
                    .write_all(&serde_json::to_vec(&request).unwrap())
                    .await
                    .unwrap();
                drop(input);
                let output = child.wait_with_output().await.unwrap();
                assert!(
                    output.status.success(),
                    "broker fixture failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                response.result = Ok(());
                response.host_iac = Some(serde_json::from_slice(&output.stdout).unwrap());
                if is_change && loss.swap(false, Ordering::SeqCst) {
                    continue;
                }
            }
            write_frame(&mut stream, &response).await.unwrap();
        }
    });
    let mut state = AppState::with_db_file(
        crate::templates::TemplateRegistry::from_specs(directory.path(), std::iter::empty())
            .unwrap(),
        directory.path().join("state.db"),
    )
    .await
    .unwrap();
    state.network =
        crate::network::NetworkClient::with_socket_path(directory.path().join("helper.sock"));
    let endpoint = state.store.iac_endpoint_id().unwrap();
    let first_owner = Uuid::new_v4();
    let second_owner = Uuid::new_v4();
    let mut ownerships = Vec::new();
    for (host, owner, key) in [
        ("app.example.com", first_owner, "firecrab_ingress.app"),
        ("peer.example.com", second_owner, "firecrab_ingress.peer"),
    ] {
        let request = ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: endpoint,
            resource: ManagedResource {
                kind: "ingress".into(),
                id: format!("route:{host}/"),
            },
            expected_generation: 0,
            expected_owner: None,
            owner: Some(ResourceOwner {
                manager: Manager::Terraform,
                id: owner,
                key: key.into(),
            }),
        };
        ownerships.push(
            change(state.clone(), Uuid::new_v4(), request)
                .await
                .unwrap(),
        );
    }
    let transfer = ChangeResourceOwner {
        operation_id: Uuid::new_v4(),
        expected_endpoint_id: endpoint,
        resource: ownerships[0].resource.clone(),
        expected_generation: 1,
        expected_owner: ownerships[0].owner.clone(),
        owner: Some(ResourceOwner {
            manager: Manager::Terraform,
            id: second_owner,
            key: "firecrab_ingress.app".into(),
        }),
    };
    let router = crate::server::build_router(
        state.clone(),
        &crate::server::HttpConfig {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 128,
            request_timeout: std::time::Duration::from_secs(30),
            static_root: Some(assets),
        },
    )
    .route(
        "/qa/lose-next-owner-ack",
        axum::routing::post(move || async move {
            lose.store(true, Ordering::SeqCst);
            axum::http::StatusCode::NO_CONTENT
        }),
    )
    .layer(axum::Extension(crate::ai_sandbox::access::LocalOperator));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let finished = Arc::new(tokio::sync::Notify::new());
    let shutdown = finished.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { shutdown.notified().await })
            .await
            .unwrap();
    });
    std::fs::write(&info,serde_json::to_vec_pretty(&serde_json::json!({
        "fixture":true,"url":url,"container":container,"transfer":transfer,"ownerships":ownerships,
        "scope":"Actual local Axum/SQLite/production React and the real root CLI broker inside an isolated Linux container. A test Unix-socket proxy forwards typed helper requests to that CLI and intentionally drops one committed acknowledgement. No live systemd/Nginx, KVM guest, Stack apply, Terraform provider or remote host runs. Terraform cards represent ownership metadata."
    })).unwrap()).unwrap();
    let stop = info.with_extension("stop");
    let started = std::time::Instant::now();
    while !stop.exists() {
        assert!(started.elapsed() < std::time::Duration::from_secs(600));
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    finished.notify_one();
    server.await.unwrap();
    helper.abort();
    assert_eq!(
        state
            .store
            .host_owner_status(transfer.operation_id)
            .unwrap(),
        (true, false)
    );
    assert_eq!(
        state
            .store
            .iac_resource_owner(&transfer.resource)
            .unwrap()
            .generation,
        2
    );
    std::fs::remove_file(info).unwrap();
    std::fs::remove_file(stop).unwrap();
}

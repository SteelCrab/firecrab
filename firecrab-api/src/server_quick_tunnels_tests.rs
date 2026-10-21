use super::*;
use axum::body::Body;
use axum::http::StatusCode;
use firecrab_helper_protocol::framing::{read_frame, write_frame};
use firecrab_helper_protocol::network::{
    NetworkRequest, NetworkRequestEnvelope, NetworkResponseEnvelope,
};
use firecrab_helper_protocol::pipe::{
    Operation, QuickTunnelRequest, QuickTunnelSnapshot, QuickTunnelTarget, ServiceAction, Snapshot,
    UnitStatus, quick_tunnel_unit,
};
use serde_json::{Value, json};
use tower::ServiceExt;

pub(super) async fn router(root: &std::path::Path, socket: PathBuf) -> Router {
    let templates =
        crate::templates::TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
    let mut state = AppState::with_db_file(templates, root.join("state.db"))
        .await
        .unwrap();
    state.network = crate::network::NetworkClient::with_socket_path(socket);
    let mut config = HttpConfig::from_values("127.0.0.1:3000", "", false, false).unwrap();
    config.request_timeout = Duration::from_millis(1);
    build_router(state, &config)
}

pub(super) fn request(method: Method, path: &str, body: Value, origin: &str) -> Request {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "localhost:3000")
        .header(header::ORIGIN, origin)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

#[tokio::test]
async fn multiple_quick_tunnels_forward_only_the_requested_intent_and_preserve_origin_checks() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("quick-list-helper.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let id = Uuid::from_u128(2);
    let apps = QuickTunnelTarget::Apps {
        upstreams: vec!["172.30.1.2:80".parse().unwrap()],
    };
    let routes = QuickTunnelTarget::Routes {
        host: "app.example.com".into(),
    };
    let path = format!("/api/pipe/quick-tunnels/{id}");
    let service_path = format!("{path}/service");
    let cases = [
        (
            Method::POST,
            "/api/pipe/quick-tunnels".to_owned(),
            json!({"name":"New app","target":apps}),
            QuickTunnelRequest::Save {
                id: None,
                name: "New app".into(),
                target: apps.clone(),
            },
        ),
        (
            Method::POST,
            "/api/pipe/quick-tunnels".to_owned(),
            json!({"id":id,"name":"All paths","target":routes}),
            QuickTunnelRequest::Save {
                id: Some(id),
                name: "All paths".into(),
                target: routes,
            },
        ),
        (
            Method::POST,
            service_path.clone(),
            json!({"action":"start"}),
            QuickTunnelRequest::Control {
                id,
                service_action: ServiceAction::Start,
            },
        ),
        (
            Method::POST,
            service_path.clone(),
            json!({"action":"stop"}),
            QuickTunnelRequest::Control {
                id,
                service_action: ServiceAction::Stop,
            },
        ),
        (
            Method::POST,
            service_path,
            json!({"action":"restart"}),
            QuickTunnelRequest::Control {
                id,
                service_action: ServiceAction::Restart,
            },
        ),
        (
            Method::DELETE,
            path,
            Value::Null,
            QuickTunnelRequest::Delete { id },
        ),
    ];
    let snapshot = Snapshot {
        firecrab_tunnels: Vec::new(),
        config: None,
        units: Vec::new(),
        nginx_available: true,
        cloudflared_available: true,
        quick_tunnel: None,
        quick_tunnels: vec![QuickTunnelSnapshot {
            id,
            name: "New app".into(),
            target: apps,
            unit: UnitStatus {
                unit: quick_tunnel_unit(id),
                load_state: "loaded".into(),
                active_state: "active".into(),
                sub_state: "running".into(),
                main_pid: 42,
            },
            public_url: Some("https://test-app.trycloudflare.com".into()),
            error: None,
        }],
    };
    let expected = cases
        .iter()
        .map(|(_, _, _, intent)| intent.clone())
        .collect::<Vec<_>>();
    let response_snapshot = snapshot.clone();
    let helper = tokio::spawn(async move {
        for intent in expected {
            let (mut stream, _) = listener.accept().await.unwrap();
            let envelope: NetworkRequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert_eq!(
                envelope.request,
                NetworkRequest::Pipe {
                    request: Operation::ManageQuickTunnel { request: intent }
                }
            );
            // Installation and service cleanup can outlast the ordinary REST deadline.
            tokio::time::sleep(Duration::from_millis(10)).await;
            write_frame(
                &mut stream,
                &NetworkResponseEnvelope {
                    version: firecrab_helper_protocol::PROTOCOL_VERSION,
                    request_id: envelope.request_id,
                    result: Ok(()),
                    pipe: Some(response_snapshot.clone()),
                },
            )
            .await
            .unwrap();
        }
    });
    let app = router(directory.path(), socket).await;
    for origin in ["http://untrusted.invalid", "http://localhost:3000"] {
        for (method, path, body, _) in &cases {
            let response = app
                .clone()
                .oneshot(request(method.clone(), path, body.clone(), origin))
                .await
                .unwrap();
            if origin == "http://untrusted.invalid" {
                assert_eq!(response.status(), axum::http::StatusCode::FORBIDDEN);
            } else {
                assert_eq!(response.status(), axum::http::StatusCode::OK);
                let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                    .await
                    .unwrap();
                assert_eq!(serde_json::from_slice::<Snapshot>(&body).unwrap(), snapshot);
            }
        }
    }
    helper.await.unwrap();
}

#[tokio::test]
async fn multiple_quick_tunnels_reject_malformed_or_excessive_intents_before_helper_calls() {
    use axum::http::StatusCode;
    let directory = tempfile::tempdir().unwrap();
    let app = router(
        directory.path(),
        directory.path().join("unused-helper.sock"),
    )
    .await;
    let nil = Uuid::nil();
    let id = Uuid::from_u128(2);
    let target = json!({"type":"apps","upstreams":["172.30.1.2:80"]});
    let cases = [
        (
            Method::POST,
            "/api/pipe/quick-tunnels".to_owned(),
            json!({"name":"","target":target}),
            StatusCode::BAD_REQUEST,
        ),
        (
            Method::POST,
            "/api/pipe/quick-tunnels".to_owned(),
            json!({"name":"x".repeat(65),"target":target}),
            StatusCode::BAD_REQUEST,
        ),
        (
            Method::POST,
            "/api/pipe/quick-tunnels".to_owned(),
            json!({"id":nil,"name":"App","target":target}),
            StatusCode::BAD_REQUEST,
        ),
        (
            Method::POST,
            "/api/pipe/quick-tunnels".to_owned(),
            json!({"name":"App","target":{"type":"apps","upstreams":[]}}),
            StatusCode::BAD_REQUEST,
        ),
        (
            Method::POST,
            "/api/pipe/quick-tunnels".to_owned(),
            json!({"name":"Routes","target":{"type":"routes","host":"*.example.com"}}),
            StatusCode::BAD_REQUEST,
        ),
        (
            Method::POST,
            "/api/pipe/quick-tunnels".to_owned(),
            json!({"name":"App","target":target,"config":{}}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            Method::POST,
            "/api/pipe/quick-tunnels".to_owned(),
            json!({"name":"App","target":{"type":"apps","upstreams":["172.30.1.2:80"],"host":"example.com"}}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            Method::POST,
            "/api/pipe/quick-tunnels".to_owned(),
            json!({"name":"x".repeat(48 * 1024),"target":target}),
            StatusCode::BAD_REQUEST,
        ),
        (
            Method::POST,
            "/api/pipe/quick-tunnels".to_owned(),
            json!({"name":"x".repeat(65 * 1024),"target":target}),
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
        (
            Method::DELETE,
            "/api/pipe/quick-tunnels/not-a-uuid".to_owned(),
            Value::Null,
            StatusCode::BAD_REQUEST,
        ),
        (
            Method::DELETE,
            format!("/api/pipe/quick-tunnels/{nil}"),
            Value::Null,
            StatusCode::BAD_REQUEST,
        ),
        (
            Method::POST,
            format!("/api/pipe/quick-tunnels/{id}/service"),
            json!({"action":"reload"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            Method::POST,
            format!("/api/pipe/quick-tunnels/{id}/service"),
            json!({"action":"stop","unit":"sshd.service"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ];
    for (method, path, body, status) in cases {
        let response = app
            .clone()
            .oneshot(request(method, &path, body, "http://localhost:3000"))
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{path}");
    }
}

#[tokio::test]
async fn cloudflare_disconnect_keeps_origin_protection_and_its_service_deadline() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("disconnect.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let helper = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let envelope: NetworkRequestEnvelope = read_frame(&mut stream).await.unwrap();
        assert_eq!(
            envelope.request,
            NetworkRequest::Pipe {
                request: Operation::DisconnectCloudflare
            }
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
        write_frame(&mut stream, &NetworkResponseEnvelope {
            version: firecrab_helper_protocol::PROTOCOL_VERSION,
            request_id: envelope.request_id,
            result: Ok(()),
            pipe: Some(serde_json::from_value(json!({"config": null,"units":[],"nginxAvailable":true,"cloudflaredAvailable":true})).unwrap()),
        }).await.unwrap();
    });
    let app = router(directory.path(), socket).await;
    for (origin, status) in [
        (
            "http://untrusted.invalid",
            axum::http::StatusCode::FORBIDDEN,
        ),
        ("http://localhost:3000", axum::http::StatusCode::OK),
    ] {
        let response = app
            .clone()
            .oneshot(request(
                Method::POST,
                "/api/pipe/cloudflare/disconnect",
                Value::Null,
                origin,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
    helper.await.unwrap();
}

#[tokio::test]
async fn firecrab_address_requests_validate_tokens_targets_and_ownership_choice() {
    let directory = tempfile::tempdir().unwrap();
    let app = router(directory.path(), directory.path().join("missing.sock")).await;
    for (body, status) in [
        (
            json!({"token":"invalid","target":{"type":"nginx","host":"app.test"}}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"target":{"type":"nginx","host":"app.test"}}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"subdomain":"www","target":{"type":"nginx","host":"app.test"}}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"subdomain":"my-app","target":{"type":"nginx","host":"bad;host"}}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"subdomain":"my-app","target":{"type":"quick_tunnel","id":"00000000-0000-0000-0000-000000000000"}}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"subdomain":"my-app","target":{"type":"nginx","host":"app.test"},"token":"secret"}),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(request(
                Method::POST,
                "/api/pipe/firecrab-tunnels",
                body,
                "http://localhost:3000",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
}

#[tokio::test]
async fn firecrab_address_forwards_only_local_intent_and_keeps_origin_protection() {
    use firecrab_helper_protocol::firecrab_tunnel::{Request as AddressRequest, Target};
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("addresses.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let helper = tokio::spawn(async move {
        for expected in [
            AddressRequest::Connect {
                token: firecrab_helper_protocol::firecrab_tunnel::ConnectionToken("a".repeat(64)),
                target: Target::Apps {
                    upstreams: vec!["172.30.36.2:80".parse().unwrap()],
                },
            },
            AddressRequest::Save {
                subdomain: "my-app".into(),
                target: Target::Nginx {
                    host: "app.test".into(),
                },
            },
            AddressRequest::Delete {
                subdomain: "my-app".into(),
            },
        ] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let envelope: NetworkRequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert_eq!(
                envelope.request,
                NetworkRequest::Pipe {
                    request: Operation::ManageFirecrabTunnel { request: expected }
                }
            );
            write_frame(&mut stream,&NetworkResponseEnvelope{version:firecrab_helper_protocol::PROTOCOL_VERSION,request_id:envelope.request_id,result:Ok(()),pipe:Some(serde_json::from_value(json!({"config":null,"units":[],"nginxAvailable":true,"cloudflaredAvailable":true})).unwrap())}).await.unwrap();
        }
    });
    let app = router(directory.path(), socket).await;
    let body = json!({"subdomain":"my-app","target":{"type":"nginx","host":"app.test"}});
    assert_eq!(
        app.clone()
            .oneshot(request(
                Method::POST,
                "/api/pipe/firecrab-tunnels",
                json!({"token":"a".repeat(64),"target":{"type":"apps","upstreams":["172.30.36.2:80"]}}),
                "http://localhost:3000"
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.clone()
            .oneshot(request(
                Method::POST,
                "/api/pipe/firecrab-tunnels",
                body.clone(),
                "http://evil.test"
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.clone()
            .oneshot(request(
                Method::POST,
                "/api/pipe/firecrab-tunnels",
                body,
                "http://localhost:3000"
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.oneshot(request(
            Method::DELETE,
            "/api/pipe/firecrab-tunnels/my-app",
            Value::Null,
            "http://localhost:3000"
        ))
        .await
        .unwrap()
        .status(),
        StatusCode::OK
    );
    helper.await.unwrap();
}

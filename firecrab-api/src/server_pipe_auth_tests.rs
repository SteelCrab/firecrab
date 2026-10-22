use super::*;
use crate::pipe_auth::{AwsCredentials, SavedAuth};
use axum::{
    body::{Body, to_bytes},
    http::StatusCode,
};
use tower::ServiceExt;

#[tokio::test]
async fn firecrab_tunnel_reuses_pipe_auth_only_on_explicit_requests_without_exposing_secrets() {
    use firecrab_helper_protocol::firecrab_tunnel::{
        ConnectionToken, Request as AddressRequest, Target,
    };
    use firecrab_helper_protocol::framing::{read_frame, write_frame};
    use firecrab_helper_protocol::network::{
        NetworkRequest, NetworkRequestEnvelope, NetworkResponseEnvelope,
    };
    use firecrab_helper_protocol::pipe::Operation;
    use serde_json::json;

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("helper.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let templates =
        crate::templates::TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
    let mut state = AppState::with_db_file(templates, dir.path().join("state.db"))
        .await
        .unwrap();
    state.network = crate::network::NetworkClient::with_socket_path(socket);
    let token = "a".repeat(64);
    state
        .store
        .put_pipe_auth(&SavedAuth::Firecrab {
            subdomain: "console-app".into(),
            token: token.clone(),
        })
        .unwrap();
    let expected_token = token.clone();
    let helper =
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let envelope: NetworkRequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert_eq!(
                envelope.request,
                NetworkRequest::Pipe {
                    request: Operation::ManageFirecrabTunnel {
                        request: AddressRequest::Connect {
                            token: ConnectionToken(expected_token),
                            target: Target::Nginx {
                                host: "app.test".into()
                            },
                        },
                    },
                }
            );
            write_frame(&mut stream, &NetworkResponseEnvelope {
            version: firecrab_helper_protocol::PROTOCOL_VERSION,
            request_id: envelope.request_id,
            result: Ok(()),
            pipe: Some(serde_json::from_value(json!({
                "config":null,"units":[],"nginxAvailable":true,"cloudflaredAvailable":true
            })).unwrap()),
        }).await.unwrap();
        });
    let config =
        HttpConfig::from_values("127.0.0.1:3000", "http://localhost:8080", false, false).unwrap();
    let app = build_router(state.clone(), &config);
    for (ownership, expected) in [
        (
            json!({"usePipeAuth":true,"token":token}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"usePipeAuth":true,"subdomain":"console-app"}),
            StatusCode::BAD_REQUEST,
        ),
        (json!({"usePipeAuth":true}), StatusCode::OK),
    ] {
        let mut body = ownership;
        body["target"] = json!({"type":"nginx","host":"app.test"});
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/pipe/firecrab-tunnels")
                    .header("origin", "http://localhost:8080")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        let body = to_bytes(response.into_body(), 65536).await.unwrap();
        assert!(!String::from_utf8_lossy(&body).contains(&token));
    }
    helper.await.unwrap();
    state
        .store
        .delete_pipe_auth(crate::pipe_auth::Provider::Firecrab)
        .unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/pipe/firecrab-tunnels")
                .header("origin", "http://localhost:8080")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"usePipeAuth":true,"target":{"type":"nginx","host":"app.test"}})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), 65536).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains("Connect Firecrab in Pipe Auth first"));
}

#[tokio::test]
async fn pipe_auth_routes_protect_mutations_redact_secrets_and_disconnect_one_provider() {
    let dir = tempfile::tempdir().unwrap();
    let templates =
        crate::templates::TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
    let state = AppState::with_db_file(templates, dir.path().join("state.db"))
        .await
        .unwrap();
    state
        .store
        .put_pipe_auth(&SavedAuth::Firecrab {
            subdomain: "console-app".into(),
            token: "a".repeat(64),
        })
        .unwrap();
    state
        .store
        .put_pipe_auth(&SavedAuth::Github {
            username: "octocat".into(),
            token: "github-private-secret".into(),
        })
        .unwrap();
    state
        .store
        .put_pipe_auth(&SavedAuth::Aws {
            account_id: "123456789012".into(),
            credentials: AwsCredentials {
                access_key_id: "AKIAIOSFODNN7EXAMPLE".into(),
                secret_access_key: "aws-private-secret".into(),
                session_token: "private-session-token".into(),
                region: "ap-northeast-2".into(),
            },
        })
        .unwrap();
    let config =
        HttpConfig::from_values("127.0.0.1:3000", "http://localhost:8080", false, false).unwrap();
    let app = build_router(state.clone(), &config);
    for uri in ["/api/pipe/auth", "/api/pipe/registries"] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 65536).await.unwrap();
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("octocat"));
        assert!(body.contains("123456789012"));
        assert_eq!(body.contains("console-app"), uri == "/api/pipe/auth");
        for secret in [
            "github-private-secret",
            "aws-private-secret",
            "private-session-token",
            "AKIAIOSFODNN7EXAMPLE",
            &"a".repeat(64),
        ] {
            assert!(!body.contains(secret));
        }
    }
    for (method, uri, origin, body, status) in [
        (
            "PUT",
            "/api/pipe/auth/firecrab",
            "http://localhost:8080",
            r#"{"provider":"firecrab","token":"invalid-token"}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            "PUT",
            "/api/pipe/auth/firecrab",
            "https://untrusted.invalid",
            r#"{"provider":"firecrab","token":"invalid-token"}"#,
            StatusCode::FORBIDDEN,
        ),
        (
            "PUT",
            "/api/pipe/auth/github",
            "https://untrusted.invalid",
            r#"{"provider":"github","token":"bad token"}"#,
            StatusCode::FORBIDDEN,
        ),
        (
            "PUT",
            "/api/pipe/auth/github",
            "http://localhost:8080",
            r#"{"provider":"github","token":"bad token"}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            "PUT",
            "/api/pipe/auth/aws",
            "http://localhost:8080",
            r#"{"provider":"github","token":"private-token"}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            "PUT",
            "/api/pipe/registries/ghcr.io",
            "http://localhost:8080",
            r#"{"username":"override","secret":"private-token"}"#,
            StatusCode::CONFLICT,
        ),
        (
            "DELETE",
            "/api/pipe/registries/ghcr.io",
            "http://localhost:8080",
            "",
            StatusCode::CONFLICT,
        ),
        (
            "DELETE",
            "/api/pipe/auth/github",
            "https://untrusted.invalid",
            "",
            StatusCode::FORBIDDEN,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("origin", origin)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{method} {uri}");
        assert_eq!(state.store.pipe_auths().unwrap().len(), 3);
        let body = to_bytes(response.into_body(), 65536).await.unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("private-token"));
    }
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/pipe/auth/github")
                    .header("origin", "http://localhost:8080")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    let connections = state.store.pipe_auths().unwrap();
    assert_eq!(connections.len(), 2);
    assert_eq!(
        connections[0].connection().provider,
        crate::pipe_auth::Provider::Aws
    );
}

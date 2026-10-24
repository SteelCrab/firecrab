use super::*;
use crate::pipe_auth::{AwsCredentials, SavedAuth};
use axum::{
    body::{Body, to_bytes},
    http::StatusCode,
};
use tower::ServiceExt;

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
    let config = HttpConfig::from_values("127.0.0.1:3000", "http://localhost:8080").unwrap();
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
        for secret in [
            "github-private-secret",
            "aws-private-secret",
            "private-session-token",
            "AKIAIOSFODNN7EXAMPLE",
        ] {
            assert!(!body.contains(secret));
        }
    }
    for (method, uri, origin, body, status) in [
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
        assert_eq!(state.store.pipe_auths().unwrap().len(), 2);
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
    assert_eq!(connections.len(), 1);
    assert_eq!(
        connections[0].connection().provider,
        crate::pipe_auth::Provider::Aws
    );
}

use super::*;
use axum::body::{Body, to_bytes};
use axum::response::IntoResponse;

#[tokio::test]
async fn driver_errors_cannot_leak_credentials() {
    let error = bounded::<()>(Uuid::nil(), async {
        Err(sqlx::Error::Protocol(
            "password=private-password postgres://private-user".into(),
        ))
    })
    .await
    .unwrap_err()
    .into_response();
    assert_eq!(error.status(), StatusCode::BAD_GATEWAY);
    let body = to_bytes(error.into_body(), 64 * 1024).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(!text.contains("private-password"));
    assert!(!text.contains("private-user"));
}

#[tokio::test(start_paused = true)]
async fn connection_wait_is_bounded() {
    let error = bounded::<()>(Uuid::nil(), std::future::pending())
        .await
        .unwrap_err()
        .into_response();
    assert_eq!(error.status(), StatusCode::GATEWAY_TIMEOUT);
}

#[tokio::test]
async fn invalid_uri_is_rejected_before_connecting() {
    let input: DatabaseInput = serde_json::from_value(serde_json::json!({
        "name": "Invalid", "provider": "supabase", "uri": "mysql://user:secret@localhost/app"
    }))
    .unwrap();
    let response = test(Extension(RequestId(Uuid::nil())), ValidatedJson(input))
        .await
        .unwrap_err()
        .into_response();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn database_routes_keep_json_validation_and_origin_checks() {
    use tower::ServiceExt;
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::with_db_file(
        crate::templates::TemplateRegistry::load_default().unwrap(),
        dir.path().join("firecrab.db"),
    )
    .await
    .unwrap();
    let config = crate::server::HttpConfig {
        bind_addr: "127.0.0.1:5523".parse().unwrap(),
        allowed_origins: vec!["http://localhost:8080".parse().unwrap()],
        max_concurrent_requests: 8,
        request_timeout: std::time::Duration::from_secs(10),
        static_root: None,
    };
    let store = state.store.clone();
    let app = crate::server::build_router(state, &config);
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/pipe/databases")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        &to_bytes(response.into_body(), 1024).await.unwrap()[..],
        b"[]"
    );
    let id = Uuid::new_v4();
    let spec: DatabaseInput = serde_json::from_value(serde_json::json!({
        "name": "Saved", "provider": "local", "uri": "mysql://private-user:private-password@localhost/app"
    })).unwrap();
    store.save_pipe_database(id, &spec).unwrap();
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/pipe/databases")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body[0]["name"], "Saved");
    assert!(body[0].get("uri").is_none());
    assert!(!body.to_string().contains("private-"));
    for expected in [StatusCode::OK, StatusCode::NOT_FOUND] {
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/pipe/databases/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    assert!(store.list_pipe_databases().unwrap().is_empty());
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/pipe/databases/test")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"name":"bad","provider":"local","uri":"https://example.com"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/pipe/databases/test")
                .header("origin", "https://evil.example")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

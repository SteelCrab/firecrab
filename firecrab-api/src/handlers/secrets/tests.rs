use super::*;
use crate::{
    server::{HttpConfig, build_router},
    templates::TemplateRegistry,
};
use axum::{Router, body::Body, http::Request};
use tower::ServiceExt;

async fn app(root: &std::path::Path) -> Router {
    let templates = TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
    let state = AppState::with_db_file(templates, root.join("state.db"))
        .await
        .unwrap();
    build_router(
        state,
        &HttpConfig {
            bind_addr: "127.0.0.1:5523".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 128,
            request_timeout: std::time::Duration::from_secs(10),
            static_root: None,
        },
    )
}

async fn request(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    if path.starts_with("/api/secrets") {
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}

async fn create_secret(app: &Router, name: &str, value: &str) -> Value {
    let (status, record) = request(
        app,
        "POST",
        "/api/secrets",
        json!({"name":name,"value":value}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(record.get("value").is_none());
    record
}

async fn remote_connection(local: &Router, remote: Router) -> (Value, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, remote).await.unwrap();
    });
    let (status, connection) = request(local, "POST", "/api/secrets/connections", json!({"name":"remote","provider":"firecrab","settings":{"baseUrl":format!("http://{address}")},"credentials":{"token":"private-fixture-token"}})).await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(connection.get("credentials").is_none());
    (connection, task)
}

#[tokio::test]
async fn secrets_crud_is_masked_persistent_and_checks_versions() {
    let root = tempfile::tempdir().unwrap();
    let router = app(root.path()).await;
    let record = create_secret(&router, "PASSWORD", "original-password").await;
    let path = format!("/api/secrets/{}", record["id"].as_str().unwrap());
    let listed = request(&router, "GET", "/api/secrets", Value::Null).await.1;
    assert!(!listed.to_string().contains("original-password"));
    assert_eq!(
        request(&router, "POST", &format!("{path}/reveal"), Value::Null)
            .await
            .1["value"],
        "original-password"
    );
    assert_eq!(
        request(
            &router,
            "PUT",
            &path,
            json!({"name":"PASSWORD","value":"stale","version":0})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status, updated) = request(
        &router,
        "PUT",
        &path,
        json!({"name":"PASSWORD","description":"edited","version":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["version"], 2);
    drop(router);
    let router = app(root.path()).await;
    assert_eq!(
        request(&router, "POST", &format!("{path}/reveal"), Value::Null)
            .await
            .1["value"],
        "original-password"
    );
    assert_eq!(
        request(
            &router,
            "PUT",
            &path,
            json!({"name":"PASSWORD","value":"","version":2})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&router, "POST", &format!("{path}/reveal"), Value::Null)
            .await
            .1["value"],
        ""
    );
    assert_eq!(
        request(&router, "DELETE", &path, Value::Null).await.0,
        StatusCode::OK
    );
    assert_eq!(
        request(&router, "POST", &format!("{path}/reveal"), Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn connections_keep_credentials_out_of_responses_and_preserve_omitted_credentials() {
    let root = tempfile::tempdir().unwrap();
    let router = app(root.path()).await;
    let input = json!({"name":"infisical","provider":"infisical","settings":{"baseUrl":"https://us.infisical.com","projectId":"project","environment":"prod","secretPath":"/"},"credentials":{"token":"private-fixture-token"}});
    let (status, connection) =
        request(&router, "POST", "/api/secrets/connections", input.clone()).await;
    assert_eq!(status, StatusCode::CREATED);
    let path = format!(
        "/api/secrets/connections/{}",
        connection["id"].as_str().unwrap()
    );
    let mut update = input;
    update["credentials"] = json!({});
    update["version"] = json!(1);
    assert_eq!(
        request(&router, "PUT", &path, update.clone()).await.0,
        StatusCode::OK
    );
    assert_eq!(
        request(&router, "PUT", &path, update).await.0,
        StatusCode::CONFLICT
    );
    let listed = request(&router, "GET", "/api/secrets/connections", Value::Null)
        .await
        .1;
    assert!(!listed.to_string().contains("private-fixture-token"));
    assert_eq!(listed[0]["version"], 2);
}

#[tokio::test]
async fn imports_only_selected_changes_without_exposing_values_and_keeps_local_copies() {
    let root = tempfile::tempdir().unwrap();
    let remote_root = tempfile::tempdir().unwrap();
    let local = app(root.path()).await;
    let remote = app(remote_root.path()).await;
    let remote_copy = remote.clone();
    create_secret(&local, "EXISTING", "local-value").await;
    create_secret(&remote, "EXISTING", "remote-value").await;
    create_secret(&remote, "NEW", "private-import-value").await;
    let (connection, task) = remote_connection(&local, remote).await;
    let preview_path = format!(
        "/api/secrets/connections/{}/preview",
        connection["id"].as_str().unwrap()
    );
    let (status, preview) =
        request(&local, "POST", &preview_path, json!({"direction":"import"})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!preview.to_string().contains("private-import-value"));
    assert!(!preview.to_string().contains("remote-value"));
    assert_eq!(
        preview["changes"],
        json!([{"name":"EXISTING","change":"update"},{"name":"NEW","change":"add"}])
    );
    let apply = format!(
        "/api/secrets/plans/{}/apply",
        preview["id"].as_str().unwrap()
    );
    assert_eq!(
        request(&local, "POST", &apply, json!({"names":["UNLISTED"]}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(&local, "POST", &apply, json!({"names":["NEW"]}))
            .await
            .1["applied"],
        json!(["NEW"])
    );
    assert_eq!(
        request(&local, "POST", &apply, json!({"names":["NEW"]}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let listed = request(&local, "GET", "/api/secrets", Value::Null).await.1;
    let existing = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == "EXISTING")
        .unwrap();
    assert_eq!(
        request(
            &local,
            "POST",
            &format!("/api/secrets/{}/reveal", existing["id"].as_str().unwrap()),
            Value::Null
        )
        .await
        .1["value"],
        "local-value"
    );
    let imported = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == "NEW")
        .unwrap();
    assert_eq!(imported["source"]["provider"], "firecrab");
    assert_eq!(imported["source"]["connectionId"], connection["id"]);
    let remote_new = request(&remote_copy, "GET", "/api/secrets", Value::Null)
        .await
        .1
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == "NEW")
        .unwrap()
        .clone();
    request(
        &remote_copy,
        "PUT",
        &format!("/api/secrets/{}", remote_new["id"].as_str().unwrap()),
        json!({"name":"NEW","value":"changed-externally","version":1}),
    )
    .await;
    assert_eq!(
        request(
            &local,
            "POST",
            &format!("/api/secrets/{}/reveal", imported["id"].as_str().unwrap()),
            Value::Null
        )
        .await
        .1["value"],
        "private-import-value"
    );
    task.abort();
}

#[tokio::test]
async fn previews_reject_deleted_and_recreated_values_instead_of_overwriting_them() {
    let root = tempfile::tempdir().unwrap();
    let remote_root = tempfile::tempdir().unwrap();
    let local = app(root.path()).await;
    let remote = app(remote_root.path()).await;
    let old = create_secret(&local, "KEY", "old").await;
    create_secret(&remote, "KEY", "remote").await;
    let (connection, task) = remote_connection(&local, remote).await;
    let preview_path = format!(
        "/api/secrets/connections/{}/preview",
        connection["id"].as_str().unwrap()
    );
    let preview = request(&local, "POST", &preview_path, json!({"direction":"import"}))
        .await
        .1;
    request(
        &local,
        "DELETE",
        &format!("/api/secrets/{}", old["id"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    create_secret(&local, "KEY", "new-local-value").await;
    let apply = format!(
        "/api/secrets/plans/{}/apply",
        preview["id"].as_str().unwrap()
    );
    assert_eq!(
        request(&local, "POST", &apply, json!({"names":["KEY"]}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    task.abort();
}

#[tokio::test]
async fn uploads_recheck_remote_values_and_report_only_completed_writes() {
    let root = tempfile::tempdir().unwrap();
    let remote_root = tempfile::tempdir().unwrap();
    let local = app(root.path()).await;
    let remote = app(remote_root.path()).await;
    let remote_copy = remote.clone();
    let source = create_secret(&local, "KEY", "upload-value").await;
    let remote_key = create_secret(&remote, "KEY", "previous").await;
    let (connection, task) = remote_connection(&local, remote).await;
    let preview_path = format!(
        "/api/secrets/connections/{}/preview",
        connection["id"].as_str().unwrap()
    );
    let preview = request(
        &local,
        "POST",
        &preview_path,
        json!({"direction":"export","secretIds":[source["id"]]}),
    )
    .await
    .1;
    let remote_path = format!("/api/secrets/{}", remote_key["id"].as_str().unwrap());
    request(
        &remote_copy,
        "PUT",
        &remote_path,
        json!({"name":"KEY","value":"rotated","version":1}),
    )
    .await;
    let apply = format!(
        "/api/secrets/plans/{}/apply",
        preview["id"].as_str().unwrap()
    );
    assert_eq!(
        request(&local, "POST", &apply, json!({"names":["KEY"]}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let preview = request(
        &local,
        "POST",
        &preview_path,
        json!({"direction":"export","secretIds":[source["id"]]}),
    )
    .await
    .1;
    let apply = format!(
        "/api/secrets/plans/{}/apply",
        preview["id"].as_str().unwrap()
    );
    let result = request(&local, "POST", &apply, json!({"names":["KEY"]})).await;
    assert_eq!(result.0, StatusCode::OK);
    assert_eq!(result.1, json!({"applied":["KEY"],"failed":[]}));
    assert_eq!(
        request(
            &remote_copy,
            "POST",
            &format!("{remote_path}/reveal"),
            Value::Null
        )
        .await
        .1["value"],
        "upload-value"
    );
    task.abort();
}

#[tokio::test]
async fn validation_does_not_echo_values_and_foreign_origins_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let router = app(root.path()).await;
    let (status, body) = request(
        &router,
        "POST",
        "/api/secrets",
        json!({"name":"bad name","value":"private-validation-value"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!body.to_string().contains("private-validation-value"));
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/secrets")
                .header("origin", "https://untrusted.example")
                .header("content-type", "application/json")
                .body(Body::from("{\"name\":\"KEY\",\"value\":\"private\"}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_partial_upload_reports_the_successful_and_failed_names_separately() {
    use axum::{
        middleware::{self, Next},
        response::IntoResponse,
    };
    let root = tempfile::tempdir().unwrap();
    let remote_root = tempfile::tempdir().unwrap();
    let local = app(root.path()).await;
    let remote = app(remote_root.path()).await;
    let remote_copy = remote.clone();
    let source_a = create_secret(&local, "A", "new-a").await;
    let source_b = create_secret(&local, "B", "new-b").await;
    let target_a = create_secret(&remote, "A", "old-a").await;
    let target_b = create_secret(&remote, "B", "old-b").await;
    let failed_path = format!("/api/secrets/{}", target_b["id"].as_str().unwrap());
    let remote = remote.layer(middleware::from_fn(
        move |request: axum::extract::Request, next: Next| {
            let failed_path = failed_path.clone();
            async move {
                if request.method() == "PUT" && request.uri().path() == failed_path {
                    return (StatusCode::FORBIDDEN, "private-provider-response").into_response();
                }
                next.run(request).await
            }
        },
    ));
    let (connection, task) = remote_connection(&local, remote).await;
    let preview_path = format!(
        "/api/secrets/connections/{}/preview",
        connection["id"].as_str().unwrap()
    );
    let preview = request(
        &local,
        "POST",
        &preview_path,
        json!({"direction":"export","secretIds":[source_a["id"],source_b["id"]]}),
    )
    .await
    .1;
    let apply = format!(
        "/api/secrets/plans/{}/apply",
        preview["id"].as_str().unwrap()
    );
    let result = request(&local, "POST", &apply, json!({"names":["A","B"]})).await;
    assert_eq!(result.0, StatusCode::OK);
    assert_eq!(result.1["applied"], json!(["A"]));
    assert_eq!(result.1["failed"][0]["name"], "B");
    assert!(!result.1.to_string().contains("private-provider-response"));
    for (target, expected) in [(target_a, "new-a"), (target_b, "old-b")] {
        let reveal = format!("/api/secrets/{}/reveal", target["id"].as_str().unwrap());
        assert_eq!(
            request(&remote_copy, "POST", &reveal, Value::Null).await.1["value"],
            expected
        );
    }
    task.abort();
}

async fn linked_state(root: &std::path::Path) -> AppState {
    let templates = TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
    let state = AppState::with_db_file(templates, root.join("state.db"))
        .await
        .unwrap();
    state
        .store
        .put_pipe_auth(&crate::pipe_auth::SavedAuth::Firecrab {
            account_id: "11111111-1111-4111-8111-111111111111".into(),
            email: "owner@example.com".into(),
            expires_at: 4102444800000,
            token: format!("fca_{}", "a".repeat(64)),
        })
        .unwrap();
    state
}

async fn console_fixture(
    account_id: &str,
) -> (
    String,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    tokio::task::JoinHandle<()>,
) {
    use axum::routing::get;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let allowed = Arc::new(AtomicBool::new(true));
    let flag = allowed.clone();
    let account_id = account_id.to_owned();
    let router = Router::new().route("/api/account/secrets", get(move |headers: axum::http::HeaderMap| {
        let flag = flag.clone(); let account_id = account_id.clone(); async move {
            assert_eq!(headers["authorization"], format!("Bearer fca_{}", "a".repeat(64)));
            if !flag.load(Ordering::SeqCst) { return (StatusCode::FORBIDDEN, Json(json!({"error":"private upstream body"}))); }
            (StatusCode::OK, Json(json!({"accountId":account_id,"secrets":[{"name":"DB_PASSWORD","value":"console-private-value"},{"name":"EMPTY","value":""}]})))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "http://{}/api/account/secrets",
        listener.local_addr().unwrap()
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (endpoint, allowed, task)
}

#[tokio::test]
async fn linked_account_import_reuses_auth_encrypts_snapshot_and_preserves_local_ids() {
    let root = tempfile::tempdir().unwrap();
    let state = linked_state(root.path()).await;
    let router = build_router(
        state.clone(),
        &HttpConfig {
            bind_addr: "127.0.0.1:5523".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 128,
            request_timeout: std::time::Duration::from_secs(10),
            static_root: None,
        },
    );
    let existing = create_secret(&router, "DB_PASSWORD", "old-value").await;
    let (endpoint, _, task) = console_fixture("11111111-1111-4111-8111-111111111111").await;
    let request = RequestId(Uuid::new_v4());
    let preview = account_preview_from(state.clone(), request, &endpoint)
        .await
        .unwrap()
        .0;
    let serialized = serde_json::to_string(&preview).unwrap();
    assert!(!serialized.contains("console-private-value"));
    assert_eq!(
        preview
            .changes
            .iter()
            .find(|change| change.name == "DB_PASSWORD")
            .unwrap()
            .change,
        Change::Update
    );
    let encrypted = std::fs::read(root.path().join("state.secrets/vault.bin")).unwrap();
    assert!(
        !encrypted
            .windows(b"console-private-value".len())
            .any(|chunk| chunk == b"console-private-value")
    );
    let applied = apply_with_account_endpoint(
        state.clone(),
        request,
        &preview.id.to_string(),
        ApplyInput {
            names: vec!["DB_PASSWORD".into(), "EMPTY".into()],
        },
        &endpoint,
    )
    .await
    .unwrap()
    .0;
    assert_eq!(applied["applied"].as_array().unwrap().len(), 2);
    let saved = state
        .secrets
        .read(|contents| {
            Ok(contents
                .secrets
                .iter()
                .find(|secret| secret.summary.name == "DB_PASSWORD")
                .unwrap()
                .clone())
        })
        .unwrap();
    assert_eq!(saved.summary.id.to_string(), existing["id"]);
    assert_eq!(saved.value, "console-private-value");
    assert_eq!(
        saved.summary.source.unwrap().connection_name,
        "Firecrab Console"
    );
    assert!(
        apply_with_account_endpoint(
            state,
            request,
            &preview.id.to_string(),
            ApplyInput {
                names: vec!["EMPTY".into()]
            },
            &endpoint
        )
        .await
        .is_err()
    );
    task.abort();
}

#[tokio::test]
async fn linked_account_import_rejects_revocation_changed_credentials_and_other_owner() {
    use std::sync::atomic::Ordering;
    let root = tempfile::tempdir().unwrap();
    let state = linked_state(root.path()).await;
    let request = RequestId(Uuid::new_v4());
    let (endpoint, allowed, task) = console_fixture("11111111-1111-4111-8111-111111111111").await;
    let preview = account_preview_from(state.clone(), request, &endpoint)
        .await
        .unwrap()
        .0;
    allowed.store(false, Ordering::SeqCst);
    assert!(
        apply_with_account_endpoint(
            state.clone(),
            request,
            &preview.id.to_string(),
            ApplyInput {
                names: vec!["DB_PASSWORD".into()]
            },
            &endpoint
        )
        .await
        .is_err()
    );
    assert!(
        state
            .secrets
            .read(|contents| Ok(contents.secrets.is_empty()))
            .unwrap()
    );
    allowed.store(true, Ordering::SeqCst);
    state
        .store
        .delete_pipe_auth(crate::pipe_auth::Provider::Firecrab)
        .unwrap();
    assert!(
        apply_with_account_endpoint(
            state.clone(),
            request,
            &preview.id.to_string(),
            ApplyInput {
                names: vec!["DB_PASSWORD".into()]
            },
            &endpoint
        )
        .await
        .is_err()
    );
    state
        .store
        .put_pipe_auth(&crate::pipe_auth::SavedAuth::Firecrab {
            account_id: "22222222-2222-4222-8222-222222222222".into(),
            email: "other@example.com".into(),
            expires_at: 4102444800000,
            token: format!("fca_{}", "b".repeat(64)),
        })
        .unwrap();
    assert!(
        apply_with_account_endpoint(
            state,
            request,
            &preview.id.to_string(),
            ApplyInput {
                names: vec!["DB_PASSWORD".into()]
            },
            &endpoint
        )
        .await
        .is_err()
    );
    task.abort();
    let other_root = tempfile::tempdir().unwrap();
    let other_state = linked_state(other_root.path()).await;
    let (endpoint, _, task) = console_fixture("33333333-3333-4333-8333-333333333333").await;
    assert!(
        account_preview_from(other_state, request, &endpoint)
            .await
            .is_err()
    );
    task.abort();
}

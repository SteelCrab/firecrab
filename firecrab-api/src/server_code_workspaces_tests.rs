use super::*;
use crate::{
    ai_sandbox::access::{ControllerIdentity, LocalOperator},
    persistence::code_workspaces::WriteResult,
    pipe_ai::SavedConnection,
    pipe_auth::{Provider, SavedAuth},
};
use axum::{
    Extension,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::StatusCode,
};
use firecrab_api_types::{AiAuthentication, AiConnection, AiProvider, CodeWorkspaceWrite};
use serde_json::{Value, json};
use std::sync::{Arc, Barrier};
use tower::ServiceExt;

const ROOT: &str = "/api/code/workspaces";

struct Fixture {
    dir: tempfile::TempDir,
    state: AppState,
    app: Router,
    connection: Uuid,
}

fn router(state: &AppState) -> Router {
    build_router(
        state.clone(),
        &HttpConfig::from_values("127.0.0.1:5523", "http://localhost:8080").unwrap(),
    )
}

fn account(id: Uuid, provider: AiProvider, authentication: AiAuthentication) -> SavedConnection {
    SavedConnection {
        connection: AiConnection {
            id,
            name: "CODE test account".into(),
            provider,
            authentication,
            endpoint: "https://provider.invalid".into(),
            models: vec![],
            streaming: false,
            tool_calls: false,
        },
        api_key: "code-test-private-ai-key".into(),
        oauth: None,
    }
}

async fn fixture() -> Fixture {
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
            token: "code-test-private-github-token".into(),
        })
        .unwrap();
    let connection = Uuid::new_v4();
    state
        .store
        .save_ai_connection(&account(
            connection,
            AiProvider::Openai,
            AiAuthentication::ApiKey,
        ))
        .unwrap();
    let app = router(&state).layer(Extension(LocalOperator));
    Fixture {
        dir,
        state,
        app,
        connection,
    }
}

fn input(connection: Uuid) -> Value {
    json!({"expectedRevision":0,"spec":{
        "name":"web-app","github":{"account":"OctoCat","owner":"team","repository":"web-app","branch":"feature/login"},
        "runtime":"node_js","resources":{"cpu":2,"ram":4096,"diskGb":16},
        "agent":{"agent":"codex","connectionId":connection}
    }})
}

async fn call(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("host", "127.0.0.1:5523")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    if status.is_success() {
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        !text.contains("code-test-private"),
        "credentials must never appear in a public response"
    );
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

#[tokio::test]
async fn code_workspaces_persist_update_and_delete_metadata_without_creating_resources() {
    let f = fixture().await;
    let id = Uuid::new_v4();
    let path = format!("{ROOT}/{id}");
    let body = input(f.connection);
    let (status, created) = call(&f.app, "PUT", &path, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["revision"], 1);
    assert_eq!(
        created["connections"],
        json!({"github":"present_unverified","ai":"present_unverified"})
    );
    assert!(created.get("vmId").is_none());
    assert_eq!(
        call(&f.app, "PUT", &path, body.clone()).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&f.app, "GET", ROOT, Value::Null)
            .await
            .1
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let templates =
        crate::templates::TemplateRegistry::from_specs(f.dir.path(), std::iter::empty()).unwrap();
    let reopened = AppState::with_db_file(templates, f.dir.path().join("state.db"))
        .await
        .unwrap();
    let app = router(&reopened).layer(Extension(LocalOperator));
    assert_eq!(call(&app, "GET", &path, Value::Null).await.1, created);
    let mut update = body;
    update["expectedRevision"] = json!(1);
    update["spec"]["name"] = json!("updated-workspace");
    let (status, changed) = call(&app, "PUT", &path, update).await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(changed["revision"], 2);
    assert_eq!(changed["createdAtMs"], created["createdAtMs"]);
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("{path}?expectedRevision=1"),
            Value::Null
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("{path}?expectedRevision=2"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&app, "GET", &path, Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(call(&app, "GET", ROOT, Value::Null).await.1, json!([]));
    assert!(reopened.store.load_all().unwrap().is_empty());
    assert!(
        reopened
            .store
            .ai_connection(f.connection)
            .unwrap()
            .is_some()
    );
    assert_eq!(reopened.store.pipe_auths().unwrap().len(), 1);
}

#[tokio::test]
async fn code_workspaces_observe_disconnection_and_account_replacement_without_losing_configuration()
 {
    let f = fixture().await;
    let path = format!("{ROOT}/{}", Uuid::new_v4());
    assert_eq!(
        call(&f.app, "PUT", &path, input(f.connection)).await.0,
        StatusCode::OK
    );
    f.state
        .store
        .save_ai_connection(&account(
            f.connection,
            AiProvider::Anthropic,
            AiAuthentication::ApiKey,
        ))
        .unwrap();
    f.state
        .store
        .put_pipe_auth(&SavedAuth::Github {
            username: "another-account".into(),
            token: "code-test-private-replacement".into(),
        })
        .unwrap();
    let observed = call(&f.app, "GET", &path, Value::Null).await.1;
    assert_eq!(
        observed["connections"],
        json!({"github":"identity_changed","ai":"incompatible"})
    );
    assert_eq!(observed["revision"], 1);
    f.state.store.delete_ai_connection(f.connection).unwrap();
    f.state.store.delete_pipe_auth(Provider::Github).unwrap();
    let observed = call(&f.app, "GET", &path, Value::Null).await.1;
    assert_eq!(
        observed["connections"],
        json!({"github":"missing","ai":"missing"})
    );
    assert_eq!(
        observed["spec"]["agent"]["connectionId"],
        f.connection.to_string()
    );
    assert_eq!(
        call(
            &f.app,
            "DELETE",
            &format!("{path}?expectedRevision=1"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn code_workspaces_reject_missing_incompatible_or_unknown_inputs_and_allow_an_explicitly_disabled_agent()
 {
    let f = fixture().await;
    let path = format!("{ROOT}/{}", Uuid::new_v4());
    for (field, value) in [
        ("connectionId", json!(Uuid::new_v4())),
        ("agent", json!("claude_code")),
    ] {
        let mut body = input(f.connection);
        body["spec"]["agent"][field] = value;
        let (status, error) = call(&f.app, "PUT", &path, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            error["error"]["fields"]
                .get("spec.agent.connectionId")
                .is_some()
        );
    }
    let mut body = input(f.connection);
    body["spec"]["github"]["account"] = json!("wrong-account");
    assert_eq!(
        call(&f.app, "PUT", &path, body).await.0,
        StatusCode::BAD_REQUEST
    );
    for location in ["root", "spec", "github", "agent", "resources"] {
        let mut body = input(f.connection);
        match location {
            "root" => body["apiKey"] = json!("code-test-private-submitted"),
            "spec" => body["spec"]["vmId"] = json!(Uuid::new_v4()),
            other => body["spec"][other]["token"] = json!("code-test-private-submitted"),
        }
        assert_eq!(
            call(&f.app, "PUT", &path, body).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(call(&f.app, "GET", ROOT, Value::Null).await.1, json!([]));
    f.state.store.delete_ai_connection(f.connection).unwrap();
    let mut body = input(f.connection);
    body["spec"]["agent"] = Value::Null;
    let (status, saved) = call(&f.app, "PUT", &path, body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["connections"]["ai"], "not_required");
}

#[tokio::test]
async fn code_workspaces_enforce_revision_name_uniqueness_and_delete_query_contracts() {
    let f = fixture().await;
    let path = format!("{ROOT}/{}", Uuid::new_v4());
    assert_eq!(
        call(&f.app, "PUT", &path, input(f.connection)).await.0,
        StatusCode::OK
    );
    let mut duplicate = input(f.connection);
    duplicate["spec"]["name"] = json!("WEB-APP");
    let other = format!("{ROOT}/{}", Uuid::new_v4());
    let (status, error) = call(&f.app, "PUT", &other, duplicate).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "code_workspace_name_taken");
    for query in [
        "",
        "?expectedRevision=0",
        "?expectedRevision=no",
        "?expectedRevision=18446744073709551615",
        "?expectedRevision=1&token=private",
    ] {
        let (status, error) = call(&f.app, "DELETE", &format!("{path}{query}"), Value::Null).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(error["error"]["code"], "validation_failed");
    }
    let mut stale = input(f.connection);
    stale["expectedRevision"] = json!(2);
    assert_eq!(
        call(&f.app, "PUT", &other, stale).await.0,
        StatusCode::CONFLICT
    );
    let (status, error) = call(&f.app, "GET", &format!("{ROOT}/not-a-uuid"), Value::Null).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(error.get("error").is_some());
    let mut oversized = input(f.connection);
    oversized["expectedRevision"] = json!(u64::MAX);
    let (status, error) = call(&f.app, "PUT", &other, oversized).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(error["error"]["fields"].get("expectedRevision").is_some());
    assert_eq!(
        call(
            &f.app,
            "PUT",
            &format!("{ROOT}/{}", Uuid::nil()),
            input(f.connection)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn code_workspaces_require_verified_operator_identity_and_preserve_origin_checks() {
    let f = fixture().await;
    let bare = router(&f.state);
    assert_eq!(
        call(&bare, "GET", ROOT, Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    let host = Uuid::new_v4();
    let peer = bare
        .clone()
        .layer(Extension(ConnectInfo(crate::tls::AuthenticatedHost {
            host_id: host,
        })));
    assert_eq!(
        call(&peer, "GET", ROOT, Value::Null).await.0,
        StatusCode::FORBIDDEN
    );
    let controller = peer.layer(Extension(ControllerIdentity(host)));
    assert_eq!(
        call(&controller, "GET", ROOT, Value::Null).await.0,
        StatusCode::OK
    );
    let forged = Request::builder()
        .uri(ROOT)
        .header("host", "127.0.0.1:5523")
        .header("x-firecrab-host-id", host.to_string())
        .header("x-forwarded-for", "127.0.0.1")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        bare.oneshot(forged).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("{ROOT}/{}", Uuid::new_v4()))
                .header("host", "127.0.0.1:5523")
                .header("origin", "https://untrusted.invalid")
                .header("content-type", "application/json")
                .body(Body::from(input(f.connection).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn code_workspaces_compare_revisions_across_independent_sqlite_connections() {
    let f = fixture().await;
    let id = Uuid::new_v4();
    assert_eq!(
        call(&f.app, "PUT", &format!("{ROOT}/{id}"), input(f.connection))
            .await
            .0,
        StatusCode::OK
    );
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = ["first", "second"]
        .into_iter()
        .map(|name| {
            let store = crate::persistence::Store::open(&f.dir.path().join("state.db")).unwrap();
            let barrier = barrier.clone();
            let mut body = input(f.connection);
            body["expectedRevision"] = json!(1);
            body["spec"]["name"] = json!(name);
            let input: CodeWorkspaceWrite = serde_json::from_value(body).unwrap();
            std::thread::spawn(move || {
                barrier.wait();
                store.write_code_workspace(id, input, 1).unwrap()
            })
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, WriteResult::Written(_)))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, WriteResult::Conflict))
            .count(),
        1
    );
    let record = f.state.store.code_workspace(id).unwrap().unwrap();
    assert_eq!(record.revision, 2);
    assert!(record.updated_at_ms >= record.created_at_ms);
}

#[tokio::test]
async fn code_workspaces_fail_closed_on_corrupt_connection_metadata_without_leaking_credentials() {
    let f = fixture().await;
    let path = format!("{ROOT}/{}", Uuid::new_v4());
    assert_eq!(
        call(&f.app, "PUT", &path, input(f.connection)).await.0,
        StatusCode::OK
    );
    let conn = rusqlite::Connection::open(f.dir.path().join("state.db")).unwrap();
    conn.execute(
        "UPDATE pipe_ai_connections SET spec=?1 WHERE id=?2",
        rusqlite::params![
            "code-test-private-corrupt-document",
            f.connection.to_string()
        ],
    )
    .unwrap();
    let (status, error) = call(&f.app, "GET", &path, Value::Null).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error["error"]["code"], "internal_error");
}

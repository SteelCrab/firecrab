use super::*;
use crate::{
    ai_sandbox::access::LocalOperator,
    persistence::{Store, code_workspaces::WriteResult},
    pipe_ai::{OAuthSession, SavedConnection},
    pipe_auth::{Provider, SavedAuth},
};
use axum::{
    Extension, Json,
    body::{Body, to_bytes},
    http::{HeaderMap, StatusCode, Uri},
};
use firecrab_api_types::*;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Notify;
use tower::ServiceExt;

const SHA: &str = "1234567890abcdef1234567890abcdef12345678";
const REF: &str = "/github/repos/team/web-app/git/ref/heads/feature%2Flogin";
const SCOPES: &str = "openid resource.invoke chatgpt.tokens.use.direct offline_access";

#[derive(Clone, Default)]
struct Remote {
    requests: Arc<Mutex<Vec<(String, HeaderMap)>>>,
    replies: Arc<Mutex<HashMap<String, (u16, Value)>>>,
    pause: Arc<Mutex<Option<String>>>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

async fn provider(
    State(remote): State<Remote>,
    uri: Uri,
    headers: HeaderMap,
) -> axum::response::Response {
    let path = uri.path().to_owned();
    remote
        .requests
        .lock()
        .unwrap()
        .push((path.clone(), headers.clone()));
    let paused = remote
        .pause
        .lock()
        .unwrap()
        .take_if(|paused| *paused == path)
        .is_some();
    if paused {
        remote.entered.notify_one();
        remote.release.notified().await;
    }
    if let Some((status, body)) = remote.replies.lock().unwrap().get(&path).cloned() {
        let rate_limited = body.get("rateLimited").and_then(Value::as_bool) == Some(true);
        let mut response = (
            StatusCode::from_u16(status).unwrap(),
            [("location", "/credential-leak-target")],
            Json(body),
        )
            .into_response();
        if rate_limited {
            response
                .headers_mut()
                .insert("x-ratelimit-remaining", "0".parse().unwrap());
        }
        return response;
    }
    let body = match path.as_str() {
        "/github/user" => json!({"login":"OctoCat"}),
        "/github/repos/team/web-app" => json!({"full_name":"team/web-app","disabled":false}),
        REF => json!({"ref":"refs/heads/feature/login","object":{"sha":SHA,"type":"commit"}}),
        "/v1/models" => {
            if headers["authorization"].to_str().unwrap().contains("oauth") {
                json!({"models":[{"slug":"catalog-only","display_name":"Test","visibility":"list"}]})
            } else {
                json!({"object":"list","data":[]})
            }
        }
        "/anthropic/v1/models" => {
            assert!(headers.contains_key("x-api-key"));
            assert_eq!(
                headers["anthropic-version"],
                crate::pipe_ai::anthropic::API_VERSION
            );
            json!({"data":[],"has_more":false})
        }
        "/api/accounts/oauth/token" => {
            json!({"access_token":"code-private-oauth-new","refresh_token":"code-private-refresh-new","expires_in":3600,"token_type":"Bearer","scope":SCOPES})
        }
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    Json(body).into_response()
}

struct Fixture {
    dir: tempfile::TempDir,
    state: AppState,
    app: Router,
    remote: Remote,
    server: tokio::task::JoinHandle<()>,
    id: Uuid,
    account: Uuid,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.remote.release.notify_one();
        self.server.abort();
    }
}

fn router(state: &AppState, operator: bool) -> Router {
    let app = build_router(
        state.clone(),
        &HttpConfig::from_values("127.0.0.1:5523", "http://localhost:8080").unwrap(),
    );
    if operator {
        app.layer(Extension(LocalOperator))
    } else {
        app
    }
}

fn account(id: Uuid, provider: AiProvider) -> SavedConnection {
    SavedConnection {
        connection: AiConnection {
            id,
            name: "Test AUTH".into(),
            provider,
            authentication: AiAuthentication::ApiKey,
            endpoint: match provider {
                AiProvider::Openai => crate::pipe_ai::ENDPOINT,
                AiProvider::Anthropic => crate::pipe_ai::anthropic::CREDENTIAL_ENDPOINT,
            }
            .into(),
            models: vec![],
            streaming: false,
            tool_calls: false,
        },
        api_key: "code-private-api-key".into(),
        oauth: None,
    }
}

async fn fixture() -> Fixture {
    let remote = Remote::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let mock = Router::new()
        .route("/{*path}", axum::routing::any(provider))
        .with_state(remote.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let templates =
        crate::templates::TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
    let mut state = AppState::with_db_file(templates, dir.path().join("state.db"))
        .await
        .unwrap();
    state.code_github = crate::code_verification::Github::at(format!("{origin}/github"));
    state.pipe_ai = crate::pipe_ai::OpenAi::at(format!("{origin}/v1/responses"));
    state.pipe_ai_anthropic =
        crate::pipe_ai::anthropic::Anthropic::at(format!("{origin}/anthropic/v1/models"));
    state
        .store
        .put_pipe_auth(&SavedAuth::Github {
            username: "octocat".into(),
            token: "code-private-github".into(),
        })
        .unwrap();
    let id = Uuid::new_v4();
    let account_id = Uuid::new_v4();
    state
        .store
        .save_ai_connection(&account(account_id, AiProvider::Openai))
        .unwrap();
    let spec = serde_json::from_value(json!({"name":"web-app","github":{"account":"octocat","owner":"team","repository":"web-app","branch":"feature/login"},"runtime":"node_js","resources":{"cpu":2,"ram":4096,"diskGb":16},"agent":{"agent":"codex","connectionId":account_id}})).unwrap();
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
    let app = router(&state, true);
    Fixture {
        dir,
        state,
        app,
        remote,
        server,
        id,
        account: account_id,
    }
}

async fn call(app: &Router, id: Uuid, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/code/workspaces/{id}/verify"))
                .header("host", "127.0.0.1:5523")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("code-private"));
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn check(f: &Fixture) -> (StatusCode, Value) {
    call(&f.app, f.id, json!({"expectedRevision":1})).await
}

fn oauth(f: &Fixture, expired: bool) {
    let mut saved = account(f.account, AiProvider::Openai);
    saved.connection.authentication = AiAuthentication::Chatgpt;
    saved.api_key = "code-private-oauth-old".into();
    saved.oauth = Some(OAuthSession {
        client_id: "oaiapp_test".into(),
        subject: "test-subject".into(),
        refresh_token: "code-private-refresh-old".into(),
        expires_at: if expired {
            1
        } else {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 3600
        },
        earliest_refresh_at: 0,
        scope: SCOPES.into(),
    });
    f.state.store.save_ai_connection(&saved).unwrap();
}

#[tokio::test]
async fn code_verification_checks_exact_branch_and_credentials_without_inference_or_configuration_mutation()
 {
    let f = fixture().await;
    let before = f.state.store.code_workspace(f.id).unwrap().unwrap();
    let (status, result) = check(&f).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(
        result["github"],
        json!({"status":"verified","commitSha":SHA})
    );
    assert_eq!(
        result["ai"],
        json!({"status":"credentials_verified","method":"openai_api_key"})
    );
    assert_eq!(result["revision"], 1);
    assert_eq!(before, f.state.store.code_workspace(f.id).unwrap().unwrap());
    assert!(
        f.state
            .store
            .ai_connection(f.account)
            .unwrap()
            .unwrap()
            .connection
            .models
            .is_empty()
    );
    let requests = f.remote.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[2].0, REF);
    assert_eq!(requests[0].1["x-github-api-version"], "2026-03-10");
    assert!(
        requests
            .iter()
            .all(|(path, _)| !path.ends_with("responses"))
    );
}

#[tokio::test]
async fn code_verification_rejects_untrusted_callers_stale_revisions_and_caller_supplied_endpoints_before_io()
 {
    let f = fixture().await;
    assert_eq!(
        call(
            &router(&f.state, false),
            f.id,
            json!({"expectedRevision":1})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    for body in [
        json!({"expectedRevision":0}),
        json!({"expectedRevision":u64::MAX}),
        json!({"expectedRevision":1,"endpoint":"https://evil.invalid"}),
        json!({"expectedRevision":1,"token":"code-private-submitted"}),
    ] {
        assert_eq!(call(&f.app, f.id, body).await.0, StatusCode::BAD_REQUEST);
    }
    assert_eq!(
        call(&f.app, f.id, json!({"expectedRevision":2})).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&f.app, Uuid::new_v4(), json!({"expectedRevision":1}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert!(f.remote.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn code_verification_reports_missing_changed_and_incompatible_accounts_without_sending_their_credentials()
 {
    let f = fixture().await;
    f.state.store.delete_pipe_auth(Provider::Github).unwrap();
    f.state.store.delete_ai_connection(f.account).unwrap();
    let (status, result) = check(&f).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["github"]["reason"], "missing");
    assert_eq!(result["ai"]["reason"], "missing");
    f.state
        .store
        .put_pipe_auth(&SavedAuth::Github {
            username: "another-user".into(),
            token: "code-private-new-github".into(),
        })
        .unwrap();
    f.state
        .store
        .save_ai_connection(&account(f.account, AiProvider::Anthropic))
        .unwrap();
    let (_, result) = check(&f).await;
    assert_eq!(result["github"]["reason"], "identity_changed");
    assert_eq!(result["ai"]["reason"], "incompatible");
    assert!(f.remote.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn code_verification_classifies_provider_failures_and_never_returns_provider_bodies_or_follows_redirects()
 {
    let f = fixture().await;
    for (code, reason) in [
        (401, "authentication_rejected"),
        (403, "access_denied"),
        (404, "not_found"),
        (409, "not_found"),
        (429, "rate_limited"),
        (503, "unavailable"),
        (302, "redirected"),
    ] {
        f.remote.replies.lock().unwrap().insert(
            REF.into(),
            (code, json!({"message":"code-private-provider-echo"})),
        );
        let (status, result) = check(&f).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(result["github"]["reason"], reason);
    }
    for (code, reason) in [
        (401, "authentication_rejected"),
        (429, "rate_limited"),
        (503, "unavailable"),
    ] {
        f.remote.replies.lock().unwrap().insert(
            "/v1/models".into(),
            (code, json!({"message":"code-private-ai-echo"})),
        );
        assert_eq!(check(&f).await.1["ai"]["reason"], reason);
    }
    assert!(
        !f.remote
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(path, _)| path == "/credential-leak-target")
    );
    f.remote
        .replies
        .lock()
        .unwrap()
        .insert(REF.into(), (403, json!({"rateLimited":true})));
    assert_eq!(check(&f).await.1["github"]["reason"], "rate_limited");
}

#[tokio::test]
async fn code_verification_does_not_probe_untrusted_endpoints_or_expired_refresh_blocked_sessions()
{
    let f = fixture().await;
    let mut saved = account(f.account, AiProvider::Openai);
    saved.connection.endpoint = "https://untrusted.invalid/credentials".into();
    f.state.store.save_ai_connection(&saved).unwrap();
    assert_eq!(check(&f).await.1["ai"]["reason"], "incompatible");
    oauth(&f, true);
    let mut saved = f.state.store.ai_connection(f.account).unwrap().unwrap();
    saved.oauth.as_mut().unwrap().earliest_refresh_at = u64::MAX;
    f.state.store.save_ai_connection(&saved).unwrap();
    assert_eq!(check(&f).await.1["ai"]["reason"], "authentication_rejected");
    assert!(
        f.remote
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(path, _)| path.starts_with("/github/"))
    );
}

#[tokio::test]
async fn code_verification_rejects_mismatched_identity_refs_non_commit_objects_and_oversized_responses()
 {
    let f = fixture().await;
    f.remote.replies.lock().unwrap().insert(
        "/github/user".into(),
        (200, json!({"login":"different-user"})),
    );
    assert_eq!(check(&f).await.1["github"]["reason"], "identity_changed");
    f.remote.replies.lock().unwrap().clear();
    for body in [
        json!({"ref":"refs/tags/feature/login","object":{"sha":SHA,"type":"commit"}}),
        json!({"ref":"refs/heads/feature/login","object":{"sha":SHA,"type":"tree"}}),
        json!({"ref":"refs/heads/feature/login","object":{"sha":"invalid","type":"commit"}}),
        json!({"huge":"x".repeat(128*1024)}),
    ] {
        f.remote
            .replies
            .lock()
            .unwrap()
            .insert(REF.into(), (200, body));
        assert_eq!(check(&f).await.1["github"]["reason"], "invalid_response");
    }
}

#[tokio::test]
async fn code_verification_selects_anthropic_and_opencode_key_checks_and_allows_no_agent() {
    for (agent, provider, method) in [
        (
            CodeAgent::ClaudeCode,
            AiProvider::Anthropic,
            "anthropic_api_key",
        ),
        (
            CodeAgent::OpenCode,
            AiProvider::Anthropic,
            "anthropic_api_key",
        ),
        (CodeAgent::OpenCode, AiProvider::Openai, "openai_api_key"),
    ] {
        let f = fixture().await;
        f.state
            .store
            .save_ai_connection(&account(f.account, provider))
            .unwrap();
        let mut spec = f.state.store.code_workspace(f.id).unwrap().unwrap().spec;
        spec.agent.as_mut().unwrap().agent = agent;
        assert!(matches!(
            f.state
                .store
                .write_code_workspace(
                    f.id,
                    CodeWorkspaceWrite {
                        expected_revision: 1,
                        spec
                    },
                    2
                )
                .unwrap(),
            WriteResult::Written(_)
        ));
        let (status, result) = call(&f.app, f.id, json!({"expectedRevision":2})).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(result["ai"]["method"], method);
    }
    let f = fixture().await;
    let mut spec = f.state.store.code_workspace(f.id).unwrap().unwrap().spec;
    spec.agent = None;
    assert!(matches!(
        f.state
            .store
            .write_code_workspace(
                f.id,
                CodeWorkspaceWrite {
                    expected_revision: 1,
                    spec
                },
                2
            )
            .unwrap(),
        WriteResult::Written(_)
    ));
    assert_eq!(
        call(&f.app, f.id, json!({"expectedRevision":2})).await.1["ai"]["status"],
        "not_required"
    );
    assert_eq!(f.remote.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn code_verification_uses_the_chatgpt_catalogue_contract_and_requires_consent() {
    let f = fixture().await;
    oauth(&f, false);
    assert_eq!(
        check(&f).await.1["ai"],
        json!({"status":"credentials_verified","method":"chatgpt_plan"})
    );
    f.remote.replies.lock().unwrap().insert(
        "/v1/models".into(),
        (200, json!({"object":"list","data":[]})),
    );
    assert_eq!(check(&f).await.1["ai"]["status"], "failed");
    let mut saved = f.state.store.ai_connection(f.account).unwrap().unwrap();
    saved.oauth.as_mut().unwrap().scope = "openid".into();
    f.state.store.save_ai_connection(&saved).unwrap();
    let previous = f.remote.requests.lock().unwrap().len();
    assert_eq!(check(&f).await.1["ai"]["reason"], "consent_required");
    assert_eq!(f.remote.requests.lock().unwrap().len(), previous + 3);
}

#[tokio::test]
async fn code_verification_refreshes_chatgpt_in_existing_auth_store_and_rechecks_reduced_consent() {
    let f = fixture().await;
    oauth(&f, true);
    assert_eq!(check(&f).await.1["ai"]["status"], "credentials_verified");
    let saved = f.state.store.ai_connection(f.account).unwrap().unwrap();
    assert_eq!(saved.api_key, "code-private-oauth-new");
    assert_eq!(
        saved.oauth.unwrap().refresh_token,
        "code-private-refresh-new"
    );
    assert_eq!(
        f.state
            .store
            .code_workspace(f.id)
            .unwrap()
            .unwrap()
            .revision,
        1
    );
    assert_eq!(check(&f).await.1["ai"]["status"], "credentials_verified");
    assert_eq!(
        f.remote
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(path, _)| path == "/api/accounts/oauth/token")
            .count(),
        1
    );
    oauth(&f, true);
    f.remote.replies.lock().unwrap().insert("/api/accounts/oauth/token".into(),(200,json!({"access_token":"code-private-oauth-new","refresh_token":"code-private-refresh-new","expires_in":3600,"token_type":"Bearer","scope":"chatgpt.tokens.use.direct"})));
    assert_eq!(check(&f).await.1["ai"]["reason"], "consent_required");
}

#[tokio::test]
async fn code_verification_rejects_configuration_or_credential_changes_during_network_io() {
    for mutation in [
        "workspace",
        "github-delete",
        "github-rotate",
        "ai-delete",
        "ai-rotate",
    ] {
        let f = fixture().await;
        *f.remote.pause.lock().unwrap() = Some(REF.into());
        let app = f.app.clone();
        let id = f.id;
        let task = tokio::spawn(async move { call(&app, id, json!({"expectedRevision":1})).await });
        tokio::time::timeout(Duration::from_secs(5), f.remote.entered.notified())
            .await
            .unwrap();
        let store = Store::open(&f.dir.path().join("state.db")).unwrap();
        match mutation {
            "workspace" => {
                let mut spec = store.code_workspace(f.id).unwrap().unwrap().spec;
                spec.name = "renamed".into();
                assert!(matches!(
                    store
                        .write_code_workspace(
                            f.id,
                            CodeWorkspaceWrite {
                                expected_revision: 1,
                                spec
                            },
                            2
                        )
                        .unwrap(),
                    WriteResult::Written(_)
                ));
            }
            "github-delete" => store.delete_pipe_auth(Provider::Github).unwrap(),
            "github-rotate" => store
                .put_pipe_auth(&SavedAuth::Github {
                    username: "octocat".into(),
                    token: "code-private-rotated".into(),
                })
                .unwrap(),
            "ai-delete" => store.delete_ai_connection(f.account).unwrap(),
            _ => {
                let mut saved = account(f.account, AiProvider::Openai);
                saved.api_key = "code-private-rotated".into();
                store.save_ai_connection(&saved).unwrap();
            }
        }
        f.remote.release.notify_one();
        let (status, result) = task.await.unwrap();
        assert_eq!(status, StatusCode::CONFLICT, "{mutation}: {result}");
        assert_eq!(result["error"]["code"], "code_workspace_bindings_changed");
    }
}

#[tokio::test]
async fn code_verification_refresh_cannot_resurrect_or_overwrite_an_account_changed_by_another_sqlite_connection()
 {
    for deleted in [true, false] {
        let f = fixture().await;
        oauth(&f, true);
        *f.remote.pause.lock().unwrap() = Some("/api/accounts/oauth/token".into());
        let app = f.app.clone();
        let id = f.id;
        let task = tokio::spawn(async move { call(&app, id, json!({"expectedRevision":1})).await });
        tokio::time::timeout(Duration::from_secs(5), f.remote.entered.notified())
            .await
            .unwrap();
        let store = Store::open(&f.dir.path().join("state.db")).unwrap();
        if deleted {
            store.delete_ai_connection(f.account).unwrap();
        } else {
            let mut saved = account(f.account, AiProvider::Openai);
            saved.api_key = "code-private-replacement".into();
            store.save_ai_connection(&saved).unwrap();
        }
        f.remote.release.notify_one();
        assert_eq!(task.await.unwrap().0, StatusCode::CONFLICT);
        let saved = store.ai_connection(f.account).unwrap();
        if deleted {
            assert!(saved.is_none());
        } else {
            assert_eq!(saved.unwrap().api_key, "code-private-replacement");
        }
        assert!(
            !f.remote
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(path, _)| path == "/v1/models")
        );
    }
}

#[tokio::test]
async fn code_verification_bounds_slow_provider_reads() {
    let f = fixture().await;
    *f.remote.pause.lock().unwrap() = Some(REF.into());
    let (status, result) = check(&f).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["github"]["reason"], "timed_out");
    assert_eq!(result["ai"]["status"], "credentials_verified");
}

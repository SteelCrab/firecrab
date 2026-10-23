//! Public REST + real WebSocket/PTY tests with a disposable CLI fixture.
//! This does not authenticate a real Anthropic account.
use super::*;
use crate::pipe_ai::claude_code::ClaudeCode;
use firecrab_api_types::{AiAuthentication, AiCliState};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

struct Fixture {
    dir: tempfile::TempDir,
    state: AppState,
    base: String,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

const CLI: &str = r#"#!/bin/sh
set -eu
test "$1" = auth
test -n "$CLAUDE_CONFIG_DIR"
test "$PWD" = "$CLAUDE_CONFIG_DIR"
test -z "${ANTHROPIC_API_KEY:-}"
test -z "${CLAUDE_CODE_OAUTH_TOKEN:-}"
case "$2" in
status)
  if test -f invalid-status; then printf 'provider-secret-on-stdout'; exit 0; fi
  if test -f oversized-status; then head -c 9000 /dev/zero; exit 0; fi
  printf 'provider-secret-on-stderr' >&2
  if test -f signed-in; then printf '{"loggedIn":true,"sessionToken":"synthetic-never-return"}'; else printf '{"loggedIn":false}'; exit 1; fi;;
login)
  test -t 0 && test -t 1 && test -t 2
  echo $$ > login-pid
  printf 'Official CLI fixture login. Enter approve: '
  read answer
  if test "$answer" = approve; then touch signed-in; echo 'Login successful'; else exit 1; fi;;
logout)
  if test -f fail-logout; then echo 'provider-secret-logout-error'; exit 1; fi
  rm -f signed-in;;
*) exit 2;;
esac
"#;

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("claude-fixture");
    std::fs::write(&binary, CLI).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let templates =
        crate::templates::TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
    let mut state = AppState::with_db_file(templates, dir.path().join("state.db"))
        .await
        .unwrap();
    state.claude_code = Arc::new(ClaudeCode::at(dir.path().join("profiles"), binary));
    let config = HttpConfig::from_values("127.0.0.1:5523", "", false, false).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = build_router(state.clone(), &config);
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Fixture {
        dir,
        state,
        base,
        server,
    }
}

impl Fixture {
    fn profile(&self, id: Uuid) -> PathBuf {
        self.dir.path().join("profiles").join(id.to_string())
    }
    fn path(&self, id: Uuid, action: &str) -> String {
        format!(
            "{}/api/pipe/ai/connections/{id}/claude-code/{action}",
            self.base
        )
    }
    async fn begin(&self, id: Uuid) -> Value {
        let response = reqwest::Client::new()
            .post(self.path(id, "login"))
            .json(&json!({"name":"Claude account"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        response.json().await.unwrap()
    }
    async fn status(&self, id: Uuid) -> Value {
        reqwest::get(self.path(id, "status"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn socket(
        &self,
        id: Uuid,
        ticket: &Value,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let url = format!(
            "{}/ws/pipe/ai/connections/{id}/claude-code/login",
            self.base.replace("http:", "ws:")
        );
        let (mut socket, _) = connect_async(url).await.unwrap();
        socket
            .send(Message::Text(json!({"ticket":ticket}).to_string().into()))
            .await
            .unwrap();
        socket
    }
    async fn sign_in(&self, id: Uuid, ticket: &Value, approve: bool) {
        let mut socket = self.socket(id, ticket).await;
        let mut text = String::new();
        while !text.contains("Enter approve:") {
            let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let Message::Binary(bytes) = frame {
                text.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
        socket
            .send(Message::Text(
                json!({"type":"resize","cols":90,"rows":30})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        socket
            .send(Message::Binary(
                if approve {
                    b"approve\n".to_vec()
                } else {
                    b"deny\n".to_vec()
                }
                .into(),
            ))
            .await
            .unwrap();
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let Message::Text(text) = frame {
                let exit: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(exit, json!({"type":"exit","success":approve}));
                break;
            }
        }
    }
}

#[tokio::test]
async fn pipe_ai_claude_real_pty_login_status_restart_and_logout_keep_tokens_in_cli() {
    let f = fixture().await;
    let id = Uuid::new_v4();
    let started = f.begin(id).await;
    assert_eq!(started["connection"]["provider"], "anthropic");
    assert_eq!(started["connection"]["authentication"], "claude_code");
    assert_eq!(f.status(id).await, json!({"status":"signed_out"}));
    f.sign_in(id, &started["ticket"], true).await;
    assert_eq!(f.status(id).await, json!({"status":"connected"}));
    let rows: Value = reqwest::get(format!("{}/api/pipe/ai/connections", f.base))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!rows.to_string().contains("synthetic-never-return"));
    let saved = f.state.store.ai_connection(id).unwrap().unwrap();
    assert!(saved.api_key.is_empty() && saved.oauth.is_none());
    assert!(saved.connection.models.is_empty() && !saved.connection.tool_calls);
    assert!(
        !serde_json::to_value(&saved)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("api_key")
    );
    assert_eq!(
        std::fs::metadata(f.profile(id))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    let restarted = ClaudeCode::at(
        f.dir.path().join("profiles"),
        f.dir.path().join("claude-fixture"),
    );
    assert_eq!(restarted.status(id).await.unwrap(), AiCliState::Connected);
    let templates =
        crate::templates::TemplateRegistry::from_specs(f.dir.path(), std::iter::empty()).unwrap();
    let recovered = AppState::with_db_file(templates, f.dir.path().join("state.db"))
        .await
        .unwrap();
    assert_eq!(
        recovered
            .store
            .ai_connection(id)
            .unwrap()
            .unwrap()
            .connection
            .authentication,
        AiAuthentication::ClaudeCode
    );

    let result = reqwest::Client::new()
        .delete(format!("{}/api/pipe/ai/connections/{id}", f.base))
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), 200);
    assert!(f.state.store.ai_connection(id).unwrap().is_none());
    assert_eq!(restarted.status(id).await.unwrap(), AiCliState::SignedOut);
}

#[tokio::test]
async fn pipe_ai_claude_rejects_credentials_models_and_http_replacement() {
    let f = fixture().await;
    let id = Uuid::new_v4();
    let client = reqwest::Client::new();
    for extra in [
        json!({"apiKey":"do-not-accept"}),
        json!({"model":"unused"}),
        json!({"command":"sh"}),
        json!({"configDirectory":"/tmp/borrowed"}),
    ] {
        let mut input = json!({"name":"Claude"});
        input
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert_eq!(
            client
                .post(f.path(id, "login"))
                .json(&input)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    let started = f.begin(id).await;
    f.sign_in(id, &started["ticket"], true).await;
    let input = json!({"name":"replace","provider":"openai","apiKey":"unrelated-key"});
    assert_eq!(
        client
            .put(format!("{}/api/pipe/ai/connections/{id}", f.base))
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(
        client
            .post(format!("{}/api/pipe/ai/connections/{id}/login", f.base))
            .json(&json!({"name":"replace"}))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(
        client
            .post(format!("{}/api/pipe/ai/connections/{id}/verify", f.base))
            .json(&json!({"model":"anything"}))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(f.status(id).await, json!({"status":"connected"}));
}

#[tokio::test]
async fn pipe_ai_claude_failed_reauth_and_logout_preserve_state_and_other_profiles() {
    let f = fixture().await;
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let started = f.begin(a).await;
    f.sign_in(a, &started["ticket"], true).await;
    let _other = f.begin(b).await;
    assert_eq!(f.status(b).await, json!({"status":"signed_out"}));
    let retry = f.begin(a).await;
    f.sign_in(a, &retry["ticket"], false).await;
    assert_eq!(f.status(a).await, json!({"status":"connected"}));
    std::fs::write(f.profile(a).join("fail-logout"), "").unwrap();
    let response = reqwest::Client::new()
        .delete(format!("{}/api/pipe/ai/connections/{a}", f.base))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 502);
    assert!(!response.text().await.unwrap().contains("provider-secret"));
    assert!(f.state.store.ai_connection(a).unwrap().is_some());
    assert!(f.state.store.ai_connection(b).unwrap().is_some());
    assert_eq!(f.status(a).await, json!({"status":"connected"}));
    assert_eq!(f.status(b).await, json!({"status":"signed_out"}));
}

#[tokio::test]
async fn pipe_ai_claude_login_tickets_are_single_use_and_origin_protected() {
    let f = fixture().await;
    let id = Uuid::new_v4();
    let started = f.begin(id).await;
    let url = format!(
        "{}/ws/pipe/ai/connections/{id}/claude-code/login",
        f.base.replace("http:", "ws:")
    );
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", "http://evil.example".parse().unwrap());
    assert!(
        matches!(connect_async(request).await, Err(tokio_tungstenite::tungstenite::Error::Http(response)) if response.status() == 403)
    );
    let mut wrong = f.socket(id, &json!(Uuid::new_v4())).await;
    assert!(!matches!(wrong.next().await, Some(Ok(Message::Binary(_)))));
    assert!(!f.profile(id).join("login-pid").exists());
    f.sign_in(id, &started["ticket"], true).await;
    let mut reused = f.socket(id, &started["ticket"]).await;
    assert!(!matches!(reused.next().await, Some(Ok(Message::Binary(_)))));
}

#[tokio::test]
async fn pipe_ai_claude_disconnect_cancels_active_login_and_reaps_process() {
    let f = fixture().await;
    let id = Uuid::new_v4();
    let started = f.begin(id).await;
    let mut socket = f.socket(id, &started["ticket"]).await;
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap(),
        Some(Ok(Message::Binary(_)))
    ));
    assert_eq!(f.status(id).await, json!({"status":"signing_in"}));
    let pid: i32 = std::fs::read_to_string(f.profile(id).join("login-pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let response = reqwest::Client::new()
        .delete(format!("{}/api/pipe/ai/connections/{id}", f.base))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    for _ in 0..50 {
        if unsafe { libc::kill(pid, 0) } == -1 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("login child survived disconnect");
}

#[tokio::test]
async fn pipe_ai_claude_missing_cli_and_invalid_status_never_report_connected() {
    let mut f = fixture().await;
    let id = Uuid::new_v4();
    f.begin(id).await;
    for marker in ["invalid-status", "oversized-status"] {
        std::fs::write(f.profile(id).join(marker), "").unwrap();
        let response = reqwest::get(f.path(id, "status")).await.unwrap();
        assert_eq!(response.status(), 502);
        assert!(!response.text().await.unwrap().contains("provider-secret"));
        std::fs::remove_file(f.profile(id).join(marker)).unwrap();
    }
    f.state.claude_code = Arc::new(ClaudeCode::at(
        f.dir.path().join("missing-profiles"),
        f.dir.path().join("not-installed"),
    ));
    assert_eq!(
        f.state.claude_code.status(id).await.unwrap(),
        AiCliState::Unavailable
    );
    // The running router retains its own state; a fresh router exercises the missing executable.
    let config = HttpConfig::from_values("127.0.0.1:5523", "", false, false).unwrap();
    use tower::ServiceExt;
    let app = build_router(f.state.clone(), &config);
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/pipe/ai/connections/{}/claude-code/login",
                    Uuid::new_v4()
                ))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(r#"{"name":"Claude"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    assert_eq!(f.state.store.ai_connections().unwrap().len(), 1);
}

#[tokio::test]
async fn pipe_ai_claude_closing_terminal_cancels_login_and_allows_retry() {
    let f = fixture().await;
    let id = Uuid::new_v4();
    let first = f.begin(id).await;
    let second = f.begin(id).await;
    assert_ne!(first["ticket"], second["ticket"]);
    let mut socket = f.socket(id, &second["ticket"]).await;
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap(),
        Some(Ok(Message::Binary(_)))
    ));
    let pid: i32 = std::fs::read_to_string(f.profile(id).join("login-pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    socket.close(None).await.unwrap();
    let mut stopped = false;
    for _ in 0..50 {
        if f.status(id).await == json!({"status":"signed_out"})
            && unsafe { libc::kill(pid, 0) } == -1
        {
            stopped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(stopped, "closing the terminal must stop its login process");
    let retry = f.begin(id).await;
    f.sign_in(id, &retry["ticket"], true).await;
    assert_eq!(f.status(id).await, json!({"status":"connected"}));
}

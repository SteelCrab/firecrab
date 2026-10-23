use super::*;
use crate::pipe_ai::OpenAi;
use axum::http::StatusCode;
use axum::{
    Json,
    body::{Body, to_bytes},
};
use serde_json::{Value, json};
use std::sync::{
    Mutex,
    atomic::{AtomicU16, Ordering},
};
use tower::ServiceExt;

#[derive(Clone, Default)]
struct Provider {
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    credential_requests: Arc<Mutex<Vec<String>>>,
    status: Arc<AtomicU16>,
    token_status: Arc<AtomicU16>,
    nonce: Arc<Mutex<String>>,
    scope: Arc<Mutex<String>>,
    grants: Arc<Mutex<Vec<std::collections::HashMap<String, String>>>>,
}

struct Fixture {
    _dir: tempfile::TempDir,
    state: AppState,
    app: Router,
    provider: Provider,
    server: tokio::task::JoinHandle<()>,
    config: HttpConfig,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn fixture() -> Fixture {
    let provider = Provider::default();
    let mock = Router::new().route("/v1/responses", post(|State(provider): State<Provider>, headers: axum::http::HeaderMap, Json(body): Json<Value>| async move {
        let auth = headers.get("authorization").unwrap().to_str().unwrap().to_owned();
        provider.requests.lock().unwrap().push((auth.clone(), body.clone()));
        let status = provider.status.load(Ordering::SeqCst);
        if status > 0 { return (StatusCode::from_u16(status).unwrap(), Json(json!({"error":{"message":auth}}))).into_response(); }
        let doc = json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":format!("response from {} {}",body["model"], auth.trim_start_matches("Bearer "))}]}]});
        if body["stream"] == true {
            return ([("content-type", "text/event-stream")], format!("data: {}\n\n", json!({"type":"response.completed","response":doc}))).into_response();
        }
        Json(doc).into_response()
    }))
    .route("/v1/models", get(|State(provider): State<Provider>, headers: axum::http::HeaderMap| async move {
        let auth = headers.get("authorization").unwrap().to_str().unwrap().to_owned();
        provider.credential_requests.lock().unwrap().push(auth.clone());
        let status = provider.status.load(Ordering::SeqCst);
        if status > 0 { return (StatusCode::from_u16(status).unwrap(), Json(json!({"error":{"message":auth}}))).into_response(); }
        Json(json!({"object":"list","data":[{"id":"listed-but-unverified","object":"model"}]})).into_response()
    }))
    .route("/.well-known/jwks.json", get(|| async { Json(serde_json::from_str::<Value>(include_str!("pipe_ai/fixtures/oidc-test-jwks.json")).unwrap()) }))
    .route("/api/accounts/oauth/token", post(|State(provider): State<Provider>, axum::Form(form): axum::Form<std::collections::HashMap<String,String>>| async move {
        let status = provider.token_status.load(Ordering::SeqCst);
        if status > 0 { return (StatusCode::from_u16(status).unwrap(), Json(json!({"error":"temporarily_unavailable"}))).into_response(); }
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        let client = form["client_id"].clone();
        let refresh = form["grant_type"] == "refresh_token";
        provider.grants.lock().unwrap().push(form);
        let timestamp=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let claims=json!({"iss":"https://auth.openai.com","sub":"fixture-account","aud":client,"iat":timestamp,"exp":timestamp+3600,"nonce":provider.nonce.lock().unwrap().clone()});
        let data=format!("{}.{}", URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","kid":"test-only"}"#),URL_SAFE_NO_PAD.encode(claims.to_string()));
        let key=ring::signature::RsaKeyPair::from_pkcs8(include_bytes!("pipe_ai/fixtures/oidc-test-key.pk8")).unwrap();
        let mut sig=vec![0;key.public().modulus_len()];
        key.sign(&ring::signature::RSA_PKCS1_SHA256,&ring::rand::SystemRandom::new(),data.as_bytes(),&mut sig).unwrap();
        Json(json!({"access_token":if refresh {"refreshed-access-token"} else {"oauth-access-token"},"refresh_token":if refresh {"rotated-refresh-token"} else {"oauth-refresh-token"},"id_token":format!("{data}.{}",URL_SAFE_NO_PAD.encode(sig)),"expires_in":3600,"token_type":"Bearer","scope":provider.scope.lock().unwrap().clone()})).into_response()
    })).with_state(provider.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/responses", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let templates =
        crate::templates::TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
    let mut state = AppState::with_db_file(templates, dir.path().join("state.db"))
        .await
        .unwrap();
    state.pipe_ai = OpenAi::at(endpoint);
    let config =
        HttpConfig::from_values("127.0.0.1:5523", "http://localhost:8080", false, false).unwrap();
    let app = build_router(state.clone(), &config);
    Fixture {
        _dir: dir,
        state,
        app,
        provider,
        server,
        config,
    }
}

async fn call(app: &Router, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("host", "127.0.0.1:5523")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 2_000_000).await.unwrap();
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes).to_string()));
    assert!(!value.to_string().contains("test-private-key"));
    (status, value)
}

fn input(model: &str) -> Value {
    json!({"name":"My AI","provider":"openai","apiKey":"test-private-key","model":model})
}
fn connection_path(id: Uuid) -> String {
    format!("/api/pipe/ai/connections/{id}")
}
fn selection(id: Uuid, model: &str) -> Value {
    json!({"connectionId":id,"model":model})
}

#[tokio::test]
async fn pipe_ai_account_connection_needs_no_model_and_does_not_prove_inference() {
    let f = fixture().await;
    let id = Uuid::new_v4();
    let mut account = input("unused");
    account.as_object_mut().unwrap().remove("model");
    let (status, connected) = call(&f.app, "PUT", &connection_path(id), account.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(connected["models"], json!([]));
    assert!(f.provider.requests.lock().unwrap().is_empty());
    assert_eq!(
        *f.provider.credential_requests.lock().unwrap(),
        ["Bearer test-private-key"]
    );
    let default = format!("/api/pipe/ai/projects/{}/model", Uuid::new_v4());
    assert_eq!(
        call(
            &f.app,
            "PUT",
            &default,
            selection(id, "listed-but-unverified")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    for (provider_status, expected) in [
        (401, StatusCode::UNAUTHORIZED),
        (403, StatusCode::UNAUTHORIZED),
        (429, StatusCode::TOO_MANY_REQUESTS),
        (503, StatusCode::BAD_GATEWAY),
        (307, StatusCode::BAD_GATEWAY),
    ] {
        f.provider.status.store(provider_status, Ordering::SeqCst);
        let mut replacement = account.clone();
        replacement["name"] = json!("Replacement");
        assert_eq!(
            call(&f.app, "PUT", &connection_path(id), replacement)
                .await
                .0,
            expected
        );
        assert_eq!(
            f.state
                .store
                .ai_connection(id)
                .unwrap()
                .unwrap()
                .connection
                .name,
            "My AI"
        );
    }
    assert!(f.provider.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn pipe_ai_chatgpt_account_login_saves_identity_without_a_model_or_inference() {
    let f = fixture().await;
    let id = Uuid::new_v4();
    *f.provider.scope.lock().unwrap() =
        "openid offline_access resource.invoke chatgpt.tokens.use.direct".into();
    let (status, start) = call(
        &f.app,
        "POST",
        &format!("{}/login", connection_path(id)),
        json!({"name":"My account"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let url = reqwest::Url::parse(start["authorizationUrl"].as_str().unwrap()).unwrap();
    let args: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    *f.provider.nonce.lock().unwrap() = args["nonce"].clone();
    let callback = format!(
        "/auth/callback?state={}&code=test-code&client_id=oaiapp_fixture",
        args["state"]
    );
    assert_eq!(
        call(&f.app, "GET", &callback, Value::Null).await.0,
        StatusCode::OK
    );
    let (_, status) = call(
        &f.app,
        "GET",
        &format!("/api/pipe/ai/logins/{}", start["loginId"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(status["status"], "connected");
    let saved = f.state.store.ai_connection(id).unwrap().unwrap();
    assert!(saved.connection.models.is_empty());
    assert_eq!(saved.oauth.unwrap().subject, "fixture-account");
    assert!(f.provider.requests.lock().unwrap().is_empty());
    assert!(f.provider.credential_requests.lock().unwrap().is_empty());
    let (_, public) = call(&f.app, "GET", "/api/pipe/ai/connections", Value::Null).await;
    assert!(!public.to_string().contains("oauth-access-token"));
    assert!(!public.to_string().contains("oauth-refresh-token"));
}

#[tokio::test]
async fn pipe_ai_connection_proves_inference_and_redacts_public_views() {
    let f = fixture().await;
    let id = Uuid::new_v4();
    let (status, connection) = call(
        &f.app,
        "PUT",
        &connection_path(id),
        input("configured-model"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(connection["models"], json!(["configured-model"]));
    assert_eq!(connection["authentication"], "api_key");
    assert_eq!(connection["endpoint"], crate::pipe_ai::ENDPOINT);
    assert_eq!(connection["toolCalls"], false);
    assert_eq!(connection["streaming"], false);
    let (_, list) = call(&f.app, "GET", "/api/pipe/ai/connections", Value::Null).await;
    assert_eq!(list[0], connection);
    let requests = f.provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "Bearer test-private-key");
    assert_eq!(requests[0].1["store"], false);
    assert_eq!(requests[0].1["stream"], false);
    assert!(requests[0].1.get("tools").is_none());
    assert!(!requests[0].1.to_string().contains("test-private-key"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(f._dir.path().join("state.db"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn pipe_ai_failed_credential_rotation_preserves_connection_and_project_default() {
    let f = fixture().await;
    let id = Uuid::new_v4();
    let project = Uuid::new_v4();
    let path = connection_path(id);
    assert_eq!(
        call(&f.app, "PUT", &path, input("first")).await.0,
        StatusCode::OK
    );
    let default = format!("/api/pipe/ai/projects/{project}/model");
    assert_eq!(
        call(&f.app, "PUT", &default, selection(id, "first"))
            .await
            .0,
        StatusCode::OK
    );
    f.provider.status.store(401, Ordering::SeqCst);
    let (status, error) = call(&f.app, "PUT", &path, input("second")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error["error"]["code"], "ai_authentication_expired");
    assert_eq!(
        f.state
            .store
            .ai_connection(id)
            .unwrap()
            .unwrap()
            .connection
            .models,
        ["first"]
    );
    assert_eq!(
        call(&f.app, "GET", &default, Value::Null).await.1,
        selection(id, "first")
    );
    f.provider.status.store(0, Ordering::SeqCst);
    assert_eq!(
        call(&f.app, "PUT", &path, input("second")).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&f.app, "GET", &default, Value::Null).await.1,
        Value::Null
    );
}

#[tokio::test]
async fn pipe_ai_defaults_overrides_disconnect_and_restart_keep_one_model_identity() {
    let f = fixture().await;
    let id = Uuid::new_v4();
    let project = Uuid::new_v4();
    let second = Uuid::new_v4();
    assert_eq!(
        call(&f.app, "PUT", &connection_path(id), input("default-model"))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&f.app, "PUT", &connection_path(second), input("task-model"))
            .await
            .0,
        StatusCode::OK
    );
    let default = format!("/api/pipe/ai/projects/{project}/model");
    call(&f.app, "PUT", &default, selection(id, "default-model")).await;
    let response_path = "/api/pipe/ai/responses";
    let (status, answer) = call(
        &f.app,
        "POST",
        response_path,
        json!({"projectId":project,"input":"Hello test-private-key"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["selection"], selection(id, "default-model"));
    assert!(
        !f.provider
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .1
            .to_string()
            .contains("test-private-key")
    );
    assert!(answer["text"].as_str().unwrap().contains("[REDACTED]"));
    let (status, answer) = call(
        &f.app,
        "POST",
        response_path,
        json!({"projectId":project,"selection":selection(second,"task-model"),"input":"Override"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["selection"], selection(second, "task-model"));
    assert_eq!(
        call(&f.app, "GET", &default, Value::Null).await.1,
        selection(id, "default-model")
    );
    let templates =
        crate::templates::TemplateRegistry::from_specs(f._dir.path(), std::iter::empty()).unwrap();
    let mut reopened = AppState::with_db_file(templates, f._dir.path().join("state.db"))
        .await
        .unwrap();
    reopened.pipe_ai = f.state.pipe_ai.clone();
    let app = build_router(reopened, &f.config);
    assert_eq!(
        call(&app, "GET", &default, Value::Null).await.1,
        selection(id, "default-model")
    );
    call(&app, "DELETE", &connection_path(id), Value::Null).await;
    assert_eq!(
        call(&app, "GET", &default, Value::Null).await.1,
        Value::Null
    );
    assert_eq!(
        call(
            &app,
            "POST",
            response_path,
            json!({"projectId":project,"input":"No fallback"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(call(&app, "POST", response_path, json!({"projectId":project,"selection":selection(id,"default-model"),"input":"Disconnected"})).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&app, "POST", response_path, json!({"projectId":project,"selection":selection(second,"task-model"),"input":"Still connected"})).await.0, StatusCode::OK);
}

#[tokio::test]
async fn pipe_ai_model_verification_reports_provider_failures_without_changing_proofs() {
    let f = fixture().await;
    let id = Uuid::new_v4();
    let path = connection_path(id);
    call(&f.app, "PUT", &path, input("first")).await;
    for (provider_status, expected, code) in [
        (403, StatusCode::FORBIDDEN, "ai_model_denied"),
        (404, StatusCode::FORBIDDEN, "ai_model_denied"),
        (429, StatusCode::TOO_MANY_REQUESTS, "ai_rate_limited"),
        (503, StatusCode::BAD_GATEWAY, "ai_provider_failed"),
        (307, StatusCode::BAD_GATEWAY, "ai_provider_failed"),
    ] {
        f.provider.status.store(provider_status, Ordering::SeqCst);
        let (status, error) = call(
            &f.app,
            "POST",
            &format!("{path}/verify"),
            json!({"model":"new-model"}),
        )
        .await;
        assert_eq!(status, expected);
        assert_eq!(error["error"]["code"], code);
        assert_eq!(
            f.state
                .store
                .ai_connection(id)
                .unwrap()
                .unwrap()
                .connection
                .models,
            ["first"]
        );
    }
    f.provider.status.store(0, Ordering::SeqCst);
    for _ in 0..2 {
        assert_eq!(
            call(
                &f.app,
                "POST",
                &format!("{path}/verify"),
                json!({"model":"new-model"})
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    assert_eq!(
        f.state
            .store
            .ai_connection(id)
            .unwrap()
            .unwrap()
            .connection
            .models,
        ["first", "new-model"]
    );
    let count = f.provider.requests.lock().unwrap().len();
    assert_eq!(
        call(
            &f.app,
            "PUT",
            &format!("/api/pipe/ai/projects/{}/model", Uuid::new_v4()),
            selection(id, "unverified")
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(f.provider.requests.lock().unwrap().len(), count);
}

#[tokio::test]
async fn pipe_ai_rejects_untrusted_origins_unknown_fields_and_concurrent_rotation() {
    let f = fixture().await;
    let id = Uuid::new_v4();
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(connection_path(id))
                .header("content-type", "application/json")
                .header("origin", "https://untrusted.invalid")
                .body(Body::from(input("model").to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let mut doc = input("model");
    doc["endpoint"] = json!("https://attacker.invalid");
    assert_eq!(
        call(&f.app, "PUT", &connection_path(id), doc).await.0,
        StatusCode::BAD_REQUEST
    );
    let guard = f.state.pipe_ai_mutations.lock().await;
    assert_eq!(
        call(&f.app, "PUT", &connection_path(id), input("model"))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&f.app, "DELETE", &connection_path(id), Value::Null)
            .await
            .0,
        StatusCode::CONFLICT
    );
    drop(guard);
    assert!(f.provider.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn pipe_ai_login_is_one_time_loopback_only_and_preserves_prior_connection_on_denial() {
    let f = fixture().await;
    let id = Uuid::new_v4();
    call(&f.app, "PUT", &connection_path(id), input("existing")).await;
    let path = format!("{}/login", connection_path(id));
    let (status, start) = call(
        &f.app,
        "POST",
        &path,
        json!({"name":"ChatGPT","model":"model"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let url = reqwest::Url::parse(start["authorizationUrl"].as_str().unwrap()).unwrap();
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["client_id"], "dynamic_agent_client");
    assert_eq!(query["redirect_uri"], "http://127.0.0.1:5523/auth/callback");
    assert_eq!(query["code_challenge_method"], "S256");
    assert!(query["scope"].contains("chatgpt.tokens.use.direct"));
    let callback = format!(
        "/auth/callback?state={}&error=access_denied",
        query["state"]
    );
    assert_eq!(
        call(&f.app, "GET", &callback, Value::Null).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&f.app, "GET", &callback, Value::Null).await.0,
        StatusCode::BAD_REQUEST
    );
    let (_, status) = call(
        &f.app,
        "GET",
        &format!("/api/pipe/ai/logins/{}", start["loginId"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(status["status"], "failed");
    assert_eq!(
        f.state
            .store
            .ai_connection(id)
            .unwrap()
            .unwrap()
            .connection
            .models,
        ["existing"]
    );
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("host", "remote.example")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"name":"Remote","model":"model"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let first = f.state.store.ai_host_id().unwrap();
    assert_eq!(first, f.state.store.ai_host_id().unwrap());
}

#[tokio::test]
async fn pipe_ai_chatgpt_login_verifies_signed_identity_streams_inference_and_rotates_refresh_tokens()
 {
    let f = fixture().await;
    let id = Uuid::new_v4();
    *f.provider.scope.lock().unwrap() =
        "openid offline_access resource.invoke chatgpt.tokens.use.direct".into();
    let (status, start) = call(
        &f.app,
        "POST",
        &format!("{}/login", connection_path(id)),
        json!({"name":"Personal ChatGPT","model":"plan-model"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let url = reqwest::Url::parse(start["authorizationUrl"].as_str().unwrap()).unwrap();
    let args: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    *f.provider.nonce.lock().unwrap() = args["nonce"].clone();
    let callback = format!(
        "/auth/callback?state={}&code=test-code&client_id=oaiapp_fixture",
        args["state"]
    );
    assert_eq!(
        call(&f.app, "GET", &callback, Value::Null).await.0,
        StatusCode::OK
    );
    let (_, status) = call(
        &f.app,
        "GET",
        &format!("/api/pipe/ai/logins/{}", start["loginId"].as_str().unwrap()),
        Value::Null,
    )
    .await;
    assert_eq!(status["status"], "connected");
    let saved = f.state.store.ai_connection(id).unwrap().unwrap();
    assert_eq!(
        saved.connection.authentication,
        firecrab_api_types::AiAuthentication::Chatgpt
    );
    assert_eq!(saved.oauth.as_ref().unwrap().client_id, "oaiapp_fixture");
    {
        let requests = f.provider.requests.lock().unwrap();
        assert_eq!(requests[0].1["stream"], true);
        assert!(requests[0].1["input"].is_array());
        assert!(requests[0].1.get("max_output_tokens").is_none());
    }
    let mut expired = saved;
    expired.oauth.as_mut().unwrap().expires_at = 1;
    f.state.store.save_ai_connection(&expired).unwrap();
    f.provider.token_status.store(503, Ordering::SeqCst);
    let (status, error) = call(
        &f.app,
        "POST",
        "/api/pipe/ai/responses",
        json!({"projectId":Uuid::new_v4(),"selection":selection(id,"plan-model"),"input":"hello"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(error["error"]["code"], "ai_provider_failed");
    assert_eq!(
        f.state.store.ai_connection(id).unwrap().unwrap().api_key,
        "oauth-access-token"
    );
    f.provider.token_status.store(0, Ordering::SeqCst);
    let (_, answer) = call(
        &f.app,
        "POST",
        "/api/pipe/ai/responses",
        json!({"projectId":Uuid::new_v4(),"selection":selection(id,"plan-model"),"input":"hello"}),
    )
    .await;
    assert_eq!(answer["selection"], selection(id, "plan-model"));
    assert!(!answer.to_string().contains("refreshed-access-token"));
    let saved = f.state.store.ai_connection(id).unwrap().unwrap();
    assert_eq!(saved.api_key, "refreshed-access-token");
    assert_eq!(saved.oauth.unwrap().refresh_token, "rotated-refresh-token");
    {
        let grants = f.provider.grants.lock().unwrap();
        assert_eq!(grants[0]["code_verifier"].len(), 43);
        assert_eq!(
            grants[0]["redirect_uri"],
            "http://127.0.0.1:5523/auth/callback"
        );
        assert_eq!(grants[1]["grant_type"], "refresh_token");
        assert_eq!(grants[1]["client_id"], "oaiapp_fixture");
        assert!(!grants[1].contains_key("scope"));
    }
    let (_, removed) = call(&f.app, "DELETE", &connection_path(id), Value::Null).await;
    assert_eq!(removed["remoteRevocationConfirmed"], false);
    assert!(f.state.store.ai_connection(id).unwrap().is_none());
    assert_eq!(
        f.state
            .store
            .ai_registration(id)
            .unwrap()
            .unwrap()
            .client_id,
        "oaiapp_fixture"
    );
}

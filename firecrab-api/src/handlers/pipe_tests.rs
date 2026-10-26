use super::*;
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use serde_json::{Value, json};

#[tokio::test]
async fn old_cli_stderr_has_a_static_rest_error_and_unknown_details_stay_private() {
    use tracing::instrument::WithSubscriber;
    for (detail, status, code) in [
        (
            "error: unrecognized subcommand 'pipe'\n\nUsage: firecrab [OPTIONS] <COMMAND>\nprivate-stderr-canary",
            StatusCode::NOT_IMPLEMENTED,
            "pipe_cli_unsupported",
        ),
        (
            "[ERROR] error: unrecognized subcommand 'pipe'\nprivate-stderr-canary",
            StatusCode::NOT_IMPLEMENTED,
            "pipe_cli_unsupported",
        ),
        (
            "error: unrecognized subcommand 'private-stderr-canary'",
            StatusCode::SERVICE_UNAVAILABLE,
            "pipe_unavailable",
        ),
        (
            "private-stderr-canary\nerror: unrecognized subcommand 'pipe'",
            StatusCode::SERVICE_UNAVAILABLE,
            "pipe_unavailable",
        ),
    ] {
        let result = Err(firecrab_helper_protocol::network::HelperFailure::Internal {
            detail: detail.into(),
        });
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = log.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_max_level(tracing::Level::WARN)
            .with_writer(move || CapturedLog(writer.clone()))
            .finish();
        let response = pipe_inspect_response(result)
            .with_subscriber(subscriber)
            .await;
        assert_eq!(response.status(), status);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], code);
        if code == "pipe_cli_unsupported" {
            assert_eq!(
                body["error"]["message"],
                "The installed Firecrab CLI does not support Pipe. Update the CLI and helper together."
            );
        }
        assert!(!String::from_utf8_lossy(&bytes).contains("private-stderr-canary"));
        assert!(!String::from_utf8_lossy(&bytes).contains("Usage:"));
        let log = String::from_utf8(log.lock().unwrap().clone()).unwrap();
        assert!(log.contains("pipe helper command failed"));
        assert!(log.contains("request_id="));
        assert!(
            log.contains(&format!("failure_code=\"{code}\""))
                || log.contains(&format!("failure_code={code}"))
        );
        assert!(!log.contains("private-stderr-canary"));
        assert!(!log.contains("Usage:"));
        assert!(!log.contains("unrecognized subcommand"));
    }
}

/// Capture only this fixture's tracing events without changing the global subscriber.
struct CapturedLog(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
impl std::io::Write for CapturedLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn matching_cli_reports_missing_engines_as_successful_disabled_metadata() {
    let snapshot: Snapshot = serde_json::from_value(json!({
        "config":null,"units":[{"unit":"firecrab-ingress.service","loadState":"not-found","activeState":"inactive","subState":"dead","mainPid":0}],
        "nginxAvailable":false,"cloudflaredAvailable":false
    })).unwrap();
    let response = pipe_inspect_response(Ok(snapshot)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["config"], Value::Null);
    assert_eq!(body["nginxAvailable"], false);
    assert_eq!(body["cloudflaredAvailable"], false);
    assert_eq!(body["units"][0]["loadState"], "not-found");
    assert_eq!(body["units"][0]["mainPid"], 0);
    assert!(body.get("error").is_none());
}

async fn pipe_inspect_response(
    result: Result<Snapshot, firecrab_helper_protocol::network::HelperFailure>,
) -> axum::response::Response {
    use axum::{body::Body, http::Request};
    use firecrab_helper_protocol::framing::{read_frame, write_frame};
    use firecrab_helper_protocol::network::{
        NetworkRequest, NetworkRequestEnvelope, NetworkResponseEnvelope,
    };
    use tower::ServiceExt;
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("pipe-metadata.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let helper = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request: NetworkRequestEnvelope = read_frame(&mut stream).await.unwrap();
        assert_eq!(
            request.request,
            NetworkRequest::Pipe {
                request: Operation::Inspect
            }
        );
        let (result, pipe) = match result {
            Ok(snapshot) => (Ok(()), Some(snapshot)),
            Err(error) => (Err(error), None),
        };
        write_frame(
            &mut stream,
            &NetworkResponseEnvelope {
                version: firecrab_helper_protocol::PROTOCOL_VERSION,
                request_id: request.request_id,
                result,
                pipe,
            },
        )
        .await
        .unwrap();
    });
    let mut state = AppState::with_db_file(
        crate::templates::TemplateRegistry::from_specs(directory.path(), std::iter::empty())
            .unwrap(),
        directory.path().join("state.db"),
    )
    .await
    .unwrap();
    state.network = NetworkClient::with_socket_path(socket);
    let router = crate::server::build_router(
        state,
        &crate::server::HttpConfig {
            bind_addr: "127.0.0.1:5523".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 8,
            request_timeout: std::time::Duration::from_secs(3),
            static_root: None,
        },
    )
    .layer(Extension(crate::ai_sandbox::access::LocalOperator));
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/pipe")
                .header("host", "127.0.0.1:5523")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    helper.await.unwrap();
    response
}

#[tokio::test]
async fn connection_failures_expose_only_fixed_safe_messages() {
    use firecrab_helper_protocol::network::HelperFailure;
    for (message, status) in [
        (
            "The connection token is invalid or revoked",
            StatusCode::BAD_REQUEST,
        ),
        (
            "Firecrab connection API is unavailable; check the service deployment",
            StatusCode::SERVICE_UNAVAILABLE,
        ),
    ] {
        let error = NetworkError::Helper(HelperFailure::Internal {
            detail: format!("[ERROR] {message}\n"),
        });
        let response = firecrab_error(&error, uuid::Uuid::new_v4())
            .unwrap()
            .into_response();
        assert_eq!(response.status(), status);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["error"]["message"], message);
    }
    let error = NetworkError::Helper(HelperFailure::Internal {
        detail: "[ERROR] private-command-output-and-secret".into(),
    });
    assert!(firecrab_error(&error, uuid::Uuid::new_v4()).is_none());
}

#[tokio::test]
async fn invalid_config_credentials_and_oversized_requests_fail_before_helper_calls() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::with_db_file(
        crate::templates::TemplateRegistry::from_specs(directory.path(), std::iter::empty())
            .unwrap(),
        directory.path().join("state.db"),
    )
    .await
    .unwrap();
    let config =
        json!({"routes":[{"host":"app.example.com","upstreams":["127.0.0.1:9000"]}],"tunnel":null});
    let mut invalid_config = config.clone();
    invalid_config["listen"] = json!("0.0.0.0:8080");
    let mut tunnel_config = config.clone();
    tunnel_config["tunnel"] =
        json!({"id":"11111111-1111-4111-8111-111111111111","credentialsFile":"credentials.json"});
    let oversized = json!({"AccountTag":"account","TunnelID":"11111111-1111-4111-8111-111111111111","TunnelSecret":"private-fixture-secret","extra":"x".repeat(48*1024)});
    for (config, credentials, field) in [
        (invalid_config, Value::Null, "config"),
        (config, json!({}), "credentials"),
        (
            tunnel_config.clone(),
            json!({"TunnelID":"22222222-2222-4222-8222-222222222222"}),
            "credentials",
        ),
        (tunnel_config, oversized, "config"),
    ] {
        let request =
            serde_json::from_value(json!({"config":config,"credentials":credentials})).unwrap();
        let request_id = RequestId(uuid::Uuid::new_v4());
        let response = apply(State(state.clone()), Extension(request_id), Json(request))
            .await
            .unwrap_err()
            .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert!(body["error"]["fields"][field].is_string(), "{body}");
        assert!(!body.to_string().contains("private-fixture-secret"));
    }
}

use super::*;
use axum::{
    Router,
    body::Bytes,
    extract::{OriginalUri, State},
    http::{HeaderMap, HeaderValue},
    routing::post,
};
use std::collections::VecDeque;

#[derive(Clone, Default)]
struct Collector {
    requests: Arc<Mutex<Vec<(String, HeaderMap, Value)>>>,
    responses: Arc<Mutex<VecDeque<(StatusCode, String)>>>,
}

async fn collect(
    State(state): State<Collector>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, HeaderMap, String) {
    let payload = serde_json::from_slice(&body).unwrap();
    state
        .requests
        .lock()
        .unwrap()
        .push((uri.path().into(), headers, payload));
    let (status, mut body) = state
        .responses
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or((StatusCode::OK, "{}".into()));
    let mut headers = HeaderMap::new();
    headers.insert(reqwest::header::RETRY_AFTER, HeaderValue::from_static("0"));
    if status.is_redirection() {
        headers.insert(
            reqwest::header::LOCATION,
            HeaderValue::from_str(&body).unwrap(),
        );
        body = "{}".into();
    }
    (status, headers, body)
}

async fn start_collector(
    responses: Vec<(StatusCode, &str)>,
) -> (Collector, String, tokio::task::JoinHandle<()>) {
    let state = Collector {
        requests: Arc::new(Mutex::new(Vec::new())),
        responses: Arc::new(Mutex::new(
            responses.into_iter().map(|(s, b)| (s, b.into())).collect(),
        )),
    };
    let app = Router::new()
        .route("/collector/v1/logs", post(collect))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/collector", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (state, url, server)
}

fn record(kind: RecordKind, body: &str) -> ExportRecord {
    ExportRecord {
        timestamp_unix_nano: 1_700_000_000_123_000_000,
        kind,
        severity: Severity::Info,
        instance_id: None,
        stream: None,
        body: body.into(),
    }
}

async fn wait_for(exporter: &Exporter, predicate: impl Fn(&ExportStatus) -> bool) -> ExportStatus {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = exporter.status();
            if predicate(&status) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("exporter did not reach the expected state")
}

#[test]
fn endpoint_rejects_credentials_query_fragments_and_invalid_schemes() {
    for endpoint in [
        "",
        "file:///tmp/out",
        "ftp://example.test",
        "http://user:password@example.test",
        "http://@example.test",
        "http://example.test/?token=secret",
        "http://example.test/#token",
        "http://example.test/\nsecret",
    ] {
        let error = validate_endpoint(endpoint).unwrap_err();
        assert_eq!(error.to_string(), "invalid OTLP endpoint");
        assert!(!format!("{error:?}").contains("secret"));
    }
    assert_eq!(
        logs_url("https://example.test/collector").unwrap().as_str(),
        "https://example.test/collector/v1/logs"
    );
    assert_eq!(
        logs_url("https://example.test/collector/")
            .unwrap()
            .as_str(),
        "https://example.test/collector/v1/logs"
    );
}

#[test]
fn sdk_environment_is_explicit_and_conflicts_do_not_overwrite_user_values() {
    let mut env = BTreeMap::from([("APP_MODE".into(), "production".into())]);
    let secrets = BTreeSet::new();
    let configured =
        application_env("http://collector.test:4318", "orders", &env, &secrets).unwrap();
    assert_eq!(
        configured["OTEL_EXPORTER_OTLP_ENDPOINT"],
        "http://collector.test:4318/"
    );
    assert_eq!(configured["OTEL_EXPORTER_OTLP_PROTOCOL"], "http/protobuf");
    assert_eq!(configured["OTEL_SERVICE_NAME"], "orders");
    assert_eq!(env.len(), 1);
    env.insert("OTEL_SERVICE_NAME".into(), "another".into());
    assert_eq!(
        application_env("http://collector.test:4318", "orders", &env, &secrets),
        Err(EnvError::Conflict("OTEL_SERVICE_NAME"))
    );
    env.remove("OTEL_SERVICE_NAME");
    env.insert(
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT".into(),
        "http://elsewhere".into(),
    );
    assert_eq!(
        application_env("http://collector.test:4318", "orders", &env, &secrets),
        Err(EnvError::Conflict("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"))
    );
    env.remove("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT");
    let secrets = BTreeSet::from(["OTEL_EXPORTER_OTLP_PROTOCOL".into()]);
    assert_eq!(
        application_env("http://collector.test:4318", "orders", &env, &secrets),
        Err(EnvError::Conflict("OTEL_EXPORTER_OTLP_PROTOCOL"))
    );
    let full = (0..62).map(|n| (format!("VAR_{n}"), "x".into())).collect();
    assert_eq!(
        application_env("http://collector.test", "orders", &full, &BTreeSet::new()),
        Err(EnvError::TooMany)
    );
}

#[tokio::test]
async fn collector_receives_isolated_otlp_events_and_logs_for_each_service() {
    let (collector, url, server) = start_collector(vec![]).await;
    let a_id = Uuid::new_v4();
    let b_id = Uuid::new_v4();
    let a = Exporter::new(&url, a_id, "orders").unwrap();
    let b = Exporter::new(&url, b_id, "billing").unwrap();
    a.try_send(record(RecordKind::Log, "order output")).unwrap();
    a.try_send(record(RecordKind::Event, "deploy complete"))
        .unwrap();
    b.try_send(record(RecordKind::Log, "billing output"))
        .unwrap();
    wait_for(&a, |s| s.delivered == 2).await;
    wait_for(&b, |s| s.delivered == 1).await;
    let requests = collector.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for (path, headers, payload) in requests.iter() {
        assert_eq!(path, "/collector/v1/logs");
        assert_eq!(headers[reqwest::header::CONTENT_TYPE], "application/json");
        assert!(!headers.contains_key(reqwest::header::AUTHORIZATION));
        let resource = &payload["resourceLogs"][0];
        let name = resource["resource"]["attributes"][0]["value"]["stringValue"]
            .as_str()
            .unwrap();
        let id = resource["resource"]["attributes"][1]["value"]["stringValue"]
            .as_str()
            .unwrap();
        let logs = resource["scopeLogs"][0]["logRecords"].as_array().unwrap();
        assert_eq!(logs[0]["timeUnixNano"], "1700000000123000000");
        if name == "orders" {
            assert_eq!(id, a_id.to_string());
            assert_eq!(logs.len(), 2);
            assert_eq!(logs[0]["body"]["stringValue"], "order output");
            assert_eq!(logs[0]["attributes"][0]["value"]["stringValue"], "log");
            assert_eq!(logs[1]["attributes"][0]["value"]["stringValue"], "event");
        } else {
            assert_eq!(name, "billing");
            assert_eq!(id, b_id.to_string());
            assert_eq!(logs.len(), 1);
            assert_eq!(logs[0]["body"]["stringValue"], "billing output");
        }
    }
    drop(requests);
    server.abort();
}

#[tokio::test]
async fn records_preserve_instance_and_stdout_stderr_identity() {
    let (collector, url, server) = start_collector(vec![]).await;
    let exporter = Exporter::new(&url, Uuid::new_v4(), "orders").unwrap();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let mut first = record(RecordKind::Log, "first");
    first.instance_id = Some(a);
    first.stream = Some(Stream::Stdout);
    let mut second = record(RecordKind::Log, "second");
    second.instance_id = Some(b);
    second.stream = Some(Stream::Stderr);
    exporter.try_send(first).unwrap();
    exporter.try_send(second).unwrap();
    wait_for(&exporter, |s| s.delivered == 2).await;
    let requests = collector.requests.lock().unwrap();
    let resources = requests[0].2["resourceLogs"].as_array().unwrap();
    assert_eq!(resources.len(), 2);
    for resource in resources {
        let id = resource["resource"]["attributes"][2]["value"]["stringValue"]
            .as_str()
            .unwrap();
        let stream =
            resource["scopeLogs"][0]["logRecords"][0]["attributes"][1]["value"]["stringValue"]
                .as_str()
                .unwrap();
        if id == a.to_string() {
            assert_eq!(stream, "stdout");
        } else {
            assert_eq!(id, b.to_string());
            assert_eq!(stream, "stderr");
        }
    }
    drop(requests);
    server.abort();
}

#[tokio::test]
async fn transient_failures_retry_with_the_same_batch_and_then_succeed() {
    let (collector, url, server) = start_collector(vec![
        (StatusCode::SERVICE_UNAVAILABLE, "no"),
        (StatusCode::OK, "{}"),
    ])
    .await;
    let exporter = Exporter::new(&url, Uuid::new_v4(), "orders").unwrap();
    exporter
        .try_send(record(RecordKind::Event, "retry"))
        .unwrap();
    let status = wait_for(&exporter, |s| s.delivered == 1).await;
    assert_eq!(status.failed_batches, 0);
    assert_eq!(status.dropped, 0);
    let requests = collector.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].2, requests[1].2);
    drop(requests);
    server.abort();
}

#[tokio::test]
async fn retry_exhaustion_and_permanent_failure_are_counted_without_exposing_response() {
    let replies = vec![
        (StatusCode::SERVICE_UNAVAILABLE, "token-in-response"),
        (StatusCode::SERVICE_UNAVAILABLE, "token-in-response"),
        (StatusCode::SERVICE_UNAVAILABLE, "token-in-response"),
    ];
    let (collector, url, server) = start_collector(replies).await;
    let exporter = Exporter::new(&url, Uuid::new_v4(), "orders").unwrap();
    exporter
        .try_send(record(RecordKind::Event, "sensitive-payload"))
        .unwrap();
    let status = wait_for(&exporter, |s| s.failed_batches == 1).await;
    assert_eq!(status.state, DeliveryState::Failed);
    assert_eq!(status.dropped, 1);
    assert_eq!(collector.requests.lock().unwrap().len(), 3);
    let rendered = serde_json::to_string(&status).unwrap();
    for forbidden in ["token-in-response", "sensitive-payload", &url] {
        assert!(!rendered.contains(forbidden));
    }
    server.abort();

    let (collector, url, server) =
        start_collector(vec![(StatusCode::BAD_REQUEST, "private")]).await;
    let exporter = Exporter::new(&url, Uuid::new_v4(), "orders").unwrap();
    exporter
        .try_send(record(RecordKind::Log, "bad request"))
        .unwrap();
    wait_for(&exporter, |s| s.failed_batches == 1).await;
    assert_eq!(collector.requests.lock().unwrap().len(), 1);
    server.abort();
}

#[tokio::test]
async fn partial_success_is_not_retried_and_rejected_count_is_visible() {
    let (collector, url, server) = start_collector(vec![(
        StatusCode::OK,
        r#"{"partialSuccess":{"rejectedLogRecords":"1","errorMessage":"private collector detail"}}"#,
    )]).await;
    let exporter = Exporter::new(&url, Uuid::new_v4(), "orders").unwrap();
    exporter.try_send(record(RecordKind::Log, "first")).unwrap();
    exporter
        .try_send(record(RecordKind::Event, "second"))
        .unwrap();
    exporter.try_send(record(RecordKind::Log, "third")).unwrap();
    let status = wait_for(&exporter, |s| s.state == DeliveryState::Partial).await;
    assert_eq!(status.delivered, 2);
    assert_eq!(status.rejected, 1);
    assert_eq!(status.dropped, 0);
    assert_eq!(collector.requests.lock().unwrap().len(), 1);
    assert!(
        !serde_json::to_string(&status)
            .unwrap()
            .contains("private collector detail")
    );
    server.abort();
}

#[tokio::test]
async fn queue_is_bounded_and_disabling_counts_pending_records() {
    let exporter = Exporter::new("http://127.0.0.1:1", Uuid::new_v4(), "orders").unwrap();
    for _ in 0..QUEUE_LIMIT {
        exporter.try_send(record(RecordKind::Log, "line")).unwrap();
    }
    assert_eq!(
        exporter.try_send(record(RecordKind::Log, "overflow")),
        Err(SubmitError::Full)
    );
    assert_eq!(
        exporter.try_send(record(RecordKind::Log, &"x".repeat(RECORD_LIMIT + 1))),
        Err(SubmitError::TooLarge)
    );
    assert_eq!(exporter.status().queued, QUEUE_LIMIT as u64);
    exporter.shutdown();
    let status = exporter.status();
    assert_eq!(status.state, DeliveryState::Disabled);
    assert_eq!(status.queued, 0);
    assert_eq!(status.dropped, QUEUE_LIMIT as u64 + 2);
    assert_eq!(
        exporter.try_send(record(RecordKind::Log, "disabled")),
        Err(SubmitError::Disabled)
    );
}

#[test]
fn settings_are_persisted_isolated_validated_and_deleted_with_the_service() {
    use crate::persistence::Store;
    use firecrab_api_types::{MicroServiceResponse, MicroServiceSpec};

    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("state.db");
    let id = Uuid::new_v4();
    let other = Uuid::new_v4();
    let spec: MicroServiceSpec = serde_json::from_value(json!({
        "name":"orders", "image":"ghcr.io/team/orders:v1", "port":8080, "ram":128, "cpu":1
    }))
    .unwrap();
    let store = Store::open(&db).unwrap();
    store
        .insert_micro_service(&MicroServiceResponse { id, spec })
        .unwrap();
    assert!(store.otlp_endpoint(id).unwrap().is_none());
    assert!(
        store
            .set_otlp_endpoint(other, Some("http://collector.test"))
            .is_err()
    );
    store
        .set_otlp_endpoint(id, Some("http://collector.test:4318"))
        .unwrap();
    assert_eq!(
        store.otlp_endpoint(id).unwrap().as_deref(),
        Some("http://collector.test:4318/")
    );
    let invalid = store
        .set_otlp_endpoint(id, Some("https://user:secret@collector.test"))
        .unwrap_err();
    assert!(!format!("{invalid:?}").contains("secret"));
    assert_eq!(
        store.otlp_endpoint(id).unwrap().as_deref(),
        Some("http://collector.test:4318/")
    );
    drop(store);
    let reopened = Store::open(&db).unwrap();
    assert_eq!(
        reopened.otlp_endpoint(id).unwrap().as_deref(),
        Some("http://collector.test:4318/")
    );
    reopened.set_otlp_endpoint(id, None).unwrap();
    assert!(reopened.otlp_endpoint(id).unwrap().is_none());
    reopened
        .set_otlp_endpoint(id, Some("https://collector.test"))
        .unwrap();
    assert!(reopened.delete_micro_service(id).unwrap());
    assert!(reopened.otlp_endpoint(id).unwrap().is_none());
}

#[tokio::test]
async fn guest_boot_uses_current_setting_without_changing_the_release_or_other_vms() {
    use crate::{
        handlers::vms::test_support::record, service_runtime::secrets, state::AppState,
        templates::TemplateRegistry,
    };
    use firecrab_api_types::{
        DeployMicroServiceRequest, MicroServiceDeployment, MicroServiceResponse, MicroServiceSpec,
        ServiceInstance, ServicePhase, VmState,
    };

    let root = tempfile::tempdir().unwrap();
    let state = AppState::with_db_file(
        TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap(),
        root.path().join("state.db"),
    )
    .await
    .unwrap();
    let id = Uuid::new_v4();
    let spec: MicroServiceSpec = serde_json::from_value(json!({
        "name":"orders", "image":"ghcr.io/team/orders:v1", "port":8080, "ram":128, "cpu":1,
        "env":{"APP_MODE":"production"}
    }))
    .unwrap();
    state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id,
            spec: spec.clone(),
        })
        .unwrap();
    let mut vm = record("replica", Uuid::new_v4());
    vm.env = spec.env.clone();
    let deployment = MicroServiceDeployment {
        recovery: None,
        started_at: 0,
        finished_at: None,
        update: None,
        id: Uuid::new_v4(),
        service_id: id,
        phase: ServicePhase::Running,
        domain_route_owned: false,
        internal_route_owned: false,
        spec: spec.clone(),
        request: DeployMicroServiceRequest {
            micro_network_id: None,
            host_port: 0,
        },
        digest: None,
        instances: vec![ServiceInstance {
            vm_id: vm.id,
            ipv4: "127.0.0.1".into(),
            host_port: 0,
            state: VmState::Created,
            healthy: false,
        }],
        log: vec![],
        error: None,
    };
    state.store.save_service_deployment(&deployment).unwrap();
    assert_eq!(secrets::guest_env(&state, &vm).await.unwrap(), spec.env);
    state
        .store
        .set_otlp_endpoint(id, Some("http://collector.test:4318"))
        .unwrap();
    let guest = secrets::guest_env(&state, &vm).await.unwrap();
    assert_eq!(
        guest["OTEL_EXPORTER_OTLP_ENDPOINT"],
        "http://collector.test:4318/"
    );
    assert_eq!(guest["OTEL_SERVICE_NAME"], "orders");
    assert_eq!(guest["APP_MODE"], "production");
    assert_eq!(vm.env, spec.env);
    assert_eq!(
        state
            .store
            .service_deployment(id)
            .unwrap()
            .unwrap()
            .spec
            .env,
        spec.env
    );
    state
        .store
        .set_otlp_endpoint(id, Some("https://new-collector.test"))
        .unwrap();
    assert_eq!(
        secrets::guest_env(&state, &vm).await.unwrap()["OTEL_EXPORTER_OTLP_ENDPOINT"],
        "https://new-collector.test/"
    );
    let mut conflict = vm.clone();
    conflict
        .env
        .insert("OTEL_SERVICE_NAME".into(), "another".into());
    assert!(
        secrets::guest_env(&state, &conflict)
            .await
            .unwrap_err()
            .contains("OTEL_SERVICE_NAME")
    );
    let unrelated = record("other", Uuid::new_v4());
    assert_eq!(
        secrets::guest_env(&state, &unrelated).await.unwrap(),
        unrelated.env
    );
}

#[tokio::test]
async fn redirect_does_not_send_observation_data_to_another_host() {
    let (other, other_url, other_server) = start_collector(vec![]).await;
    let location = format!("{other_url}/v1/logs");
    let (first, url, first_server) = start_collector(vec![(StatusCode::FOUND, &location)]).await;
    let exporter = Exporter::new(&url, Uuid::new_v4(), "orders").unwrap();
    exporter
        .try_send(record(RecordKind::Log, "private log"))
        .unwrap();
    let status = wait_for(&exporter, |s| s.failed_batches == 1).await;
    assert_eq!(status.dropped, 1);
    assert_eq!(first.requests.lock().unwrap().len(), 1);
    assert!(other.requests.lock().unwrap().is_empty());
    first_server.abort();
    other_server.abort();
}

#[tokio::test]
async fn export_accepts_the_maximum_retained_application_log_size() {
    let (collector, url, server) = start_collector(vec![]).await;
    let exporter = Exporter::new(&url, Uuid::new_v4(), "orders").unwrap();
    let line = format!("{}…", "x".repeat(4096));
    exporter.try_send(record(RecordKind::Log, &line)).unwrap();
    wait_for(&exporter, |s| s.delivered == 1).await;
    let requests = collector.requests.lock().unwrap();
    assert_eq!(
        requests[0].2["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["body"]["stringValue"],
        line
    );
    drop(requests);
    server.abort();
}

#[tokio::test]
async fn protobuf_json_null_fields_have_their_default_values() {
    let (collector, url, server) = start_collector(vec![
        (StatusCode::OK, r#"{"partialSuccess":null}"#),
        (
            StatusCode::OK,
            r#"{"partialSuccess":{"rejectedLogRecords":null,"errorMessage":null}}"#,
        ),
    ])
    .await;
    let exporter = Exporter::new(&url, Uuid::new_v4(), "orders").unwrap();
    exporter
        .try_send(record(RecordKind::Event, "first"))
        .unwrap();
    wait_for(&exporter, |s| s.delivered == 1).await;
    exporter
        .try_send(record(RecordKind::Event, "second"))
        .unwrap();
    let status = wait_for(&exporter, |s| s.delivered == 2).await;
    assert_eq!(status.state, DeliveryState::Delivered);
    assert_eq!(collector.requests.lock().unwrap().len(), 2);
    server.abort();
}

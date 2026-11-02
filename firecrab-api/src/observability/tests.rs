use super::*;
use crate::{persistence::Store, templates::TemplateRegistry};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    routing::get as route_get,
};
use firecrab_api_types::MicroServiceResponse;
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn fixture() -> (tempfile::TempDir, AppState, Uuid, Uuid) {
    let root = tempfile::tempdir().unwrap();
    let state = AppState::with_db_file(
        TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap(),
        root.path().join("state.db"),
    )
    .await
    .unwrap();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    for (id, name) in [(a, "api-gateway"), (b, "background-worker")] {
        let spec = serde_json::from_value(serde_json::json!({"name":name,"image":"ghcr.io/team/app:v1","port":8080,"ram":128,"cpu":1})).unwrap();
        state
            .store
            .insert_micro_service(&MicroServiceResponse { id, spec })
            .unwrap();
    }
    (root, state, a, b)
}
fn request() -> Extension<RequestId> {
    Extension(RequestId(Uuid::new_v4()))
}
fn config(service: Uuid) -> SignozConfig {
    SignozConfig {
        endpoint: "http://collector.test:4318".into(),
        service_ids: vec![service],
        paused: false,
    }
}

#[tokio::test]
async fn configuration_is_persistent_atomic_and_does_not_take_over_other_exporters() {
    let (root, state, a, b) = fixture().await;
    state
        .store
        .set_otlp_endpoint(b, Some("http://independent.test"))
        .unwrap();
    let mut draft = config(a);
    draft.service_ids.push(b);
    let error = configure(
        State(state.clone()),
        request(),
        Caller::default(),
        ValidatedJson(draft),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.code(), "validation_failed");
    assert!(error.field_names().contains(&b.to_string()));
    assert!(state.store.signoz_config().unwrap().is_none());
    assert!(state.store.otlp_endpoint(a).unwrap().is_none());
    let _ = configure(
        State(state.clone()),
        request(),
        Caller::default(),
        ValidatedJson(config(a)),
    )
    .await
    .unwrap();
    let reopened = Store::open(&root.path().join("state.db")).unwrap();
    assert_eq!(
        reopened.signoz_config().unwrap().unwrap().service_ids,
        vec![a]
    );
    assert_eq!(
        reopened.otlp_endpoint(b).unwrap().as_deref(),
        Some("http://independent.test/")
    );
    let boundary = state.store.otlp_configuration(a).unwrap().unwrap().1;
    let exporter = state.observations.exporter(a).unwrap();
    let mut paused = config(a);
    paused.paused = true;
    let _ = configure(
        State(state.clone()),
        request(),
        Caller::default(),
        ValidatedJson(paused),
    )
    .await
    .unwrap();
    assert!(exporter.status().paused);
    assert_eq!(
        state.store.otlp_configuration(a).unwrap().unwrap().1,
        boundary
    );
    assert!(
        send_test(
            State(state.clone()),
            request(),
            Caller::default(),
            ValidatedJson(TestLog { service_id: a })
        )
        .await
        .is_err()
    );
    let error = service_observation::configure(
        State(state.clone()),
        request(),
        Caller::default(),
        axum::extract::Path(a.to_string()),
        ValidatedJson(
            serde_json::from_value::<service_observation::Settings>(
                serde_json::json!({"endpoint":null}),
            )
            .unwrap(),
        ),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.code(), "observability_managed");
    let _ = disconnect(State(state.clone()), request(), Caller::default())
        .await
        .unwrap();
    assert!(state.store.signoz_config().unwrap().is_none());
    assert!(state.store.otlp_endpoint(a).unwrap().is_none());
    assert!(state.observations.exporter(a).is_none());
    assert_eq!(
        state.store.otlp_endpoint(b).unwrap().as_deref(),
        Some("http://independent.test/")
    );
}

#[tokio::test]
async fn invalid_and_duplicate_sources_leave_the_saved_configuration_unchanged() {
    let (_root, state, a, b) = fixture().await;
    let _ = configure(
        State(state.clone()),
        request(),
        Caller::default(),
        ValidatedJson(config(a)),
    )
    .await
    .unwrap();
    let before = state.store.signoz_config().unwrap();
    for mut draft in [
        SignozConfig {
            service_ids: vec![],
            ..config(a)
        },
        SignozConfig {
            service_ids: vec![b, b],
            ..config(a)
        },
        SignozConfig {
            service_ids: vec![Uuid::new_v4()],
            ..config(a)
        },
    ] {
        assert!(
            configure(
                State(state.clone()),
                request(),
                Caller::default(),
                ValidatedJson(draft.clone())
            )
            .await
            .is_err()
        );
        draft.endpoint = "http://user:password@collector.test".into();
        assert!(
            configure(
                State(state.clone()),
                request(),
                Caller::default(),
                ValidatedJson(draft)
            )
            .await
            .is_err()
        );
        assert_eq!(state.store.signoz_config().unwrap(), before);
    }
}

#[tokio::test]
async fn read_does_not_create_an_exporter_and_transport_identity_is_required() {
    let (_root, state, _, _) = fixture().await;
    let value = get(State(state.clone()), request(), Caller::default())
        .await
        .unwrap()
        .0;
    assert!(value.config.is_none());
    assert!(value.history.is_empty());
    assert!(value.sources.iter().all(|source| source.status.is_none()));
    let app = Router::new()
        .route("/", get_route())
        .layer(request())
        .with_state(state);
    let response = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("sandbox_identity_required"));
}
fn get_route() -> axum::routing::MethodRouter<AppState> {
    route_get(super::get)
}

#[tokio::test]
async fn delegates_cannot_read_configure_probe_test_or_disconnect() {
    let (_root, state, a, _) = fixture().await;
    let delegate: Caller =
        serde_json::from_value(serde_json::json!({"principal":"local","operator":false})).unwrap();
    assert_eq!(
        get(State(state.clone()), request(), delegate)
            .await
            .err()
            .unwrap()
            .code(),
        "service_operator_required"
    );
    assert_eq!(
        configure(
            State(state.clone()),
            request(),
            delegate,
            ValidatedJson(config(a))
        )
        .await
        .err()
        .unwrap()
        .code(),
        "service_operator_required"
    );
    assert_eq!(
        test_connection(
            State(state.clone()),
            request(),
            delegate,
            ValidatedJson(Probe {
                endpoint: "http://collector.test".into()
            })
        )
        .await
        .err()
        .unwrap()
        .code(),
        "service_operator_required"
    );
    assert_eq!(
        send_test(
            State(state.clone()),
            request(),
            delegate,
            ValidatedJson(TestLog { service_id: a })
        )
        .await
        .err()
        .unwrap()
        .code(),
        "service_operator_required"
    );
    assert_eq!(
        disconnect(State(state.clone()), request(), delegate)
            .await
            .err()
            .unwrap()
            .code(),
        "service_operator_required"
    );
    assert!(state.store.signoz_config().unwrap().is_none());
}

#[tokio::test]
async fn paused_config_survives_restart_and_service_deletion_removes_its_binding() {
    let (root, state, a, _) = fixture().await;
    let mut draft = config(a);
    draft.paused = true;
    let _ = configure(
        State(state.clone()),
        request(),
        Caller::default(),
        ValidatedJson(draft),
    )
    .await
    .unwrap();
    let restarted = AppState::with_db_file(
        TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap(),
        root.path().join("state.db"),
    )
    .await
    .unwrap();
    service_observation::export_pass(&restarted).await.unwrap();
    let exporter = restarted.observations.exporter(a).unwrap();
    assert!(exporter.status().paused);
    assert_eq!(exporter.status().delivered, 0);
    assert!(exporter.status().history.is_empty());
    assert!(restarted.store.delete_micro_service(a).unwrap());
    assert!(
        restarted
            .store
            .signoz_config()
            .unwrap()
            .unwrap()
            .service_ids
            .is_empty()
    );
    service_observation::export_pass(&restarted).await.unwrap();
    assert!(restarted.observations.exporter(a).is_none());
}

#[tokio::test]
async fn operator_probe_and_test_log_report_the_collectors_actual_acknowledgement() {
    use std::sync::{Arc, Mutex};
    let (_root, state, a, _) = fixture().await;
    let received = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let sink = received.clone();
    let collector = Router::new().route(
        "/v1/logs",
        axum::routing::post(move |Json(value): Json<serde_json::Value>| {
            let sink = sink.clone();
            async move {
                sink.lock().unwrap().push(value);
                Json(serde_json::json!({"partialSuccess":{}}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let collector = tokio::spawn(async move { axum::serve(listener, collector).await.unwrap() });
    let response = test_connection(
        State(state.clone()),
        request(),
        Caller::default(),
        ValidatedJson(Probe {
            endpoint: endpoint.clone(),
        }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(response.http_status, Some(200));
    assert!(state.store.signoz_config().unwrap().is_none());
    let _ = configure(
        State(state.clone()),
        request(),
        Caller::default(),
        ValidatedJson(SignozConfig {
            endpoint,
            service_ids: vec![a],
            paused: false,
        }),
    )
    .await
    .unwrap();
    let response = send_test(
        State(state.clone()),
        request(),
        Caller::default(),
        ValidatedJson(TestLog { service_id: a }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(response.http_status, Some(200));
    assert_eq!(response.delivered, 1);
    let snapshot = snapshot(state, Uuid::new_v4()).await.unwrap();
    assert_eq!(snapshot.history.len(), 1);
    assert_eq!(snapshot.history[0].service_id, a);
    assert_eq!(
        snapshot
            .sources
            .iter()
            .find(|source| source.id == a)
            .unwrap()
            .status
            .as_ref()
            .unwrap()
            .delivered,
        1
    );
    let received = received.lock().unwrap();
    assert_eq!(received[0], serde_json::json!({"resourceLogs":[]}));
    assert!(received[1].to_string().contains(&a.to_string()));
    assert!(
        received[1]
            .to_string()
            .contains("Firecrab SigNoz integration test")
    );
    collector.abort();
}

/// Opt-in acceptance against a disposable local SigNoz collector and ClickHouse.
/// Test-generated observation records exercise the production API/export path;
/// this test does not boot a MicroVM or modify an existing installation.
#[tokio::test]
#[ignore = "requires an isolated local SigNoz stack and explicit QA endpoints"]
async fn live_signoz_accepts_selected_records_pause_resume_failure_recovery_and_disconnect() {
    use crate::persistence::service_logs::InputLog;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    let endpoint =
        std::env::var("FIRECRAB_SIGNOZ_QA_ENDPOINT").expect("set the isolated OTLP endpoint");
    let query_endpoint = std::env::var("FIRECRAB_SIGNOZ_QA_QUERY_ENDPOINT")
        .expect("set the isolated ClickHouse HTTP endpoint");
    for endpoint in [&endpoint, &query_endpoint] {
        let url = reqwest::Url::parse(endpoint).unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert!(url.port().is_some_and(|port| port > 1024));
    }
    let (root, state, a, b) = fixture().await;
    let nonce = Uuid::new_v4().to_string();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let probe = test_connection(
        State(state.clone()),
        request(),
        Caller::default(),
        ValidatedJson(Probe {
            endpoint: endpoint.clone(),
        }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(probe.status, otlp::DeliveryState::Delivered);
    let identities = [
        (Uuid::new_v4(), Uuid::new_v4()),
        (Uuid::new_v4(), Uuid::new_v4()),
    ];
    state
        .store
        .append_service_logs(
            a,
            identities[0],
            1,
            "active",
            false,
            &[InputLog {
                sequence: 0,
                timestamp: chrono::Utc::now().timestamp_millis(),
                stream: "stdout".into(),
                message: format!("{nonce}-before-opt-in"),
            }],
        )
        .unwrap();
    let mut draft = SignozConfig {
        endpoint: endpoint.clone(),
        service_ids: vec![a],
        paused: false,
    };
    let _ = configure(
        State(state.clone()),
        request(),
        Caller::default(),
        ValidatedJson(draft.clone()),
    )
    .await
    .unwrap();
    let now = state
        .store
        .otlp_configuration(a)
        .unwrap()
        .unwrap()
        .1
        .max(chrono::Utc::now().timestamp_millis());
    for (service, identity, name) in [
        (a, identities[0], "selected"),
        (b, identities[1], "unselected"),
    ] {
        state
            .store
            .append_service_logs(
                service,
                identity,
                3,
                "active",
                false,
                &[
                    InputLog {
                        sequence: 1,
                        timestamp: now,
                        stream: "stdout".into(),
                        message: format!("{nonce}-{name}-stdout"),
                    },
                    InputLog {
                        sequence: 2,
                        timestamp: now,
                        stream: "stderr".into(),
                        message: format!("{nonce}-{name}-stderr"),
                    },
                ],
            )
            .unwrap();
    }
    let conn = rusqlite::Connection::open(root.path().join("state.db")).unwrap();
    conn.execute("INSERT INTO service_observation_events(service_id,timestamp,kind,severity,message) VALUES (?1,?2,'deploy','info',?3)",rusqlite::params![a.to_string(),chrono::Utc::now().timestamp(),format!("{nonce}-lifecycle-event")]).unwrap();
    service_observation::export_pass(&state).await.unwrap();
    let receipt = send_test(
        State(state.clone()),
        request(),
        Caller::default(),
        ValidatedJson(TestLog { service_id: a }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(receipt.status, otlp::DeliveryState::Delivered);
    assert!(state.observations.exporter(b).is_none());
    let exporter = state.observations.exporter(a).unwrap();
    assert_eq!(exporter.status().delivered, 4);
    draft.paused = true;
    let _ = configure(
        State(state.clone()),
        request(),
        Caller::default(),
        ValidatedJson(draft.clone()),
    )
    .await
    .unwrap();
    state
        .store
        .append_service_logs(
            a,
            identities[0],
            4,
            "active",
            false,
            &[InputLog {
                sequence: 3,
                timestamp: chrono::Utc::now().timestamp_millis().max(now),
                stream: "stdout".into(),
                message: format!("{nonce}-after-resume"),
            }],
        )
        .unwrap();
    service_observation::export_pass(&state).await.unwrap();
    assert_eq!(exporter.status().delivered, 4);
    assert_eq!(exporter.status().queued, 1);
    draft.paused = false;
    let _ = configure(
        State(state.clone()),
        request(),
        Caller::default(),
        ValidatedJson(draft.clone()),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while exporter.status().delivered < 5 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    // Fault injection is local and forwards successful requests to the real sink.
    let failing = Arc::new(AtomicBool::new(true));
    let attempts = Arc::new(AtomicUsize::new(0));
    let target = format!("{}/v1/logs", endpoint.trim_end_matches('/'));
    let (fail, seen, forward) = (failing.clone(), attempts.clone(), client.clone());
    let proxy = Router::new().route(
        "/v1/logs",
        axum::routing::post(move |body: axum::body::Bytes| {
            let (fail, seen, forward, target) =
                (fail.clone(), seen.clone(), forward.clone(), target.clone());
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
                if fail.load(Ordering::SeqCst) {
                    return (StatusCode::SERVICE_UNAVAILABLE, "{}".to_string());
                }
                let response = forward
                    .post(target)
                    .header("Content-Type", "application/json")
                    .body(body)
                    .send()
                    .await
                    .unwrap();
                (response.status(), response.text().await.unwrap())
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    draft.endpoint = format!("http://{}", listener.local_addr().unwrap());
    let proxy = tokio::spawn(async move { axum::serve(listener, proxy).await.unwrap() });
    let _ = configure(
        State(state.clone()),
        request(),
        Caller::default(),
        ValidatedJson(draft.clone()),
    )
    .await
    .unwrap();
    let failed = send_test(
        State(state.clone()),
        request(),
        Caller::default(),
        ValidatedJson(TestLog { service_id: a }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(failed.status, otlp::DeliveryState::Failed);
    assert_eq!(failed.http_status, Some(503));
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    failing.store(false, Ordering::SeqCst);
    let recovered = send_test(
        State(state.clone()),
        request(),
        Caller::default(),
        ValidatedJson(TestLog { service_id: a }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(recovered.status, otlp::DeliveryState::Delivered);
    let view = snapshot(state.clone(), Uuid::new_v4()).await.unwrap();
    assert!(
        view.history
            .iter()
            .any(|batch| batch.batch.http_status == Some(503))
    );
    assert!(
        view.history
            .iter()
            .any(|batch| batch.batch.http_status == Some(200))
    );
    let _ = disconnect(State(state.clone()), request(), Caller::default())
        .await
        .unwrap();
    assert!(state.observations.exporter(a).is_none());
    assert!(state.store.signoz_config().unwrap().is_none());

    // Collector acknowledgement alone is insufficient: wait for searchable sink rows.
    let query = format!(
        "SELECT countIf(position(concat(body,toJSONString(body_v2)), '{nonce}-selected-') > 0), countIf(position(concat(body,toJSONString(body_v2)), '{nonce}-unselected-') > 0), countIf(position(concat(body,toJSONString(body_v2)), '{nonce}-before-opt-in') > 0), countIf(position(concat(body,toJSONString(body_v2)), '{nonce}-lifecycle-event') > 0), countIf(position(concat(body,toJSONString(body_v2)), '{nonce}-after-resume') > 0) FROM signoz_logs.logs_v2"
    );
    let counts = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let result = client
                .post(&query_endpoint)
                .body(query.clone())
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .text()
                .await
                .unwrap();
            if result.trim() == "2\t0\t0\t1\t1" {
                break result;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .expect("SigNoz did not index the expected isolated records");
    println!(
        "SigNoz indexed selected stdout/stderr, event and resumed record; unselected/pre-opt-in counts remain zero: {}",
        counts.trim()
    );
    proxy.abort();
}

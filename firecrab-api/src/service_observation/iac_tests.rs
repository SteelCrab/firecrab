use super::*;
use crate::{
    ai_sandbox::access::LocalOperator,
    persistence::service_logs::InputLog,
    server::{HttpConfig, build_router},
    templates::TemplateRegistry,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
    routing::post,
};
use firecrab_stack::{
    ApplyResourcePlan, Manager, PlanResource, ResourceConfiguration, ResourceOwner, ResourcePlan,
    ServiceOtlpConfiguration,
};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn fixture(
    root: &std::path::Path,
    manager: Manager,
    endpoint: String,
) -> (AppState, Router, PlanResource) {
    let state = AppState::with_db_file(
        TemplateRegistry::from_specs(root, std::iter::empty()).unwrap(),
        root.join("state.db"),
    )
    .await
    .unwrap();
    let service = Uuid::new_v4();
    state.store.insert_micro_service(&firecrab_api_types::MicroServiceResponse{id:service,
        spec:serde_json::from_value(json!({"name":"shared-telemetry","image":"ghcr.io/example/app:v1","port":8080,"ram":128,"cpu":1})).unwrap()}).unwrap();
    let input = PlanResource {
        operation_id: Uuid::new_v4(),
        expected_endpoint_id: state.store.iac_endpoint_id().unwrap(),
        owner: ResourceOwner {
            manager,
            id: if manager == Manager::Stack {
                state.store.register_stack("shared-otlp").unwrap().id
            } else {
                Uuid::new_v4()
            },
            key: "telemetry".into(),
        },
        resource: None,
        removal: None,
        desired: Some(ResourceConfiguration::ServiceOtlp(
            ServiceOtlpConfiguration {
                service_id: service,
                endpoint: Some(endpoint),
            },
        )),
    };
    let app = build_router(
        state.clone(),
        &HttpConfig {
            bind_addr: "127.0.0.1:5523".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 128,
            request_timeout: Duration::from_secs(10),
            static_root: None,
        },
    )
    .layer(Extension(LocalOperator));
    (state, app, input)
}

async fn apply_api(app: &Router, plan: &ResourcePlan) -> (StatusCode, Value) {
    let command = ApplyResourcePlan {
        expected_endpoint_id: plan.ownership.endpoint_id,
        plan_sha256: plan.plan_sha256.clone(),
    };
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/iac/resource-plans/{}/apply",
                    plan.operation_id
                ))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&command).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 128 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn append(
    state: &AppState,
    service: Uuid,
    runtime: (Uuid, Uuid),
    sequence: u64,
    at: i64,
    message: &str,
) {
    state
        .store
        .append_service_logs(
            service,
            runtime,
            sequence,
            "active",
            false,
            &[InputLog {
                sequence,
                timestamp: at,
                stream: "stdout".into(),
                message: message.into(),
            }],
        )
        .unwrap();
}

async fn delivered(exporter: &Exporter, count: u64) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while exporter.status().delivered < count {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("collector did not acknowledge the expected records");
}

#[tokio::test]
async fn iac_resource_otlp_runtime_both_managers_keep_noop_session_and_never_replay_disabled_history()
 {
    for manager in [Manager::Stack, Manager::Terraform] {
        let received = std::sync::Arc::new(Mutex::new(Vec::<Value>::new()));
        let sink = received.clone();
        let collector = Router::new().route(
            "/v1/logs",
            post(move |Json(value): Json<Value>| {
                let sink = sink.clone();
                async move {
                    sink.lock().unwrap().push(value);
                    Json(json!({}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, collector).await.unwrap() });
        let root = tempfile::tempdir().unwrap();
        let (state, app, mut request) = fixture(root.path(), manager, endpoint.clone()).await;
        let Some(ResourceConfiguration::ServiceOtlp(config)) = &request.desired else {
            unreachable!()
        };
        let service = config.service_id;
        let runtime = (Uuid::new_v4(), Uuid::new_v4());
        append(
            &state,
            service,
            runtime,
            1,
            chrono::Utc::now().timestamp_millis(),
            "before-opt-in",
        );
        let create = state
            .store
            .plan_resource(request.clone(), crate::secrets::now_ms())
            .unwrap();
        let (code, receipt) = apply_api(&app, &create).await;
        assert_eq!(code, StatusCode::OK, "{receipt}");
        let original = receipt.clone();
        let boundary = state.store.otlp_configuration(service).unwrap().unwrap().1;
        let exporter = state.observations.sessions.lock().unwrap()[&service]
            .exporter
            .clone();
        export_pass(&state).await.unwrap();
        assert_eq!(exporter.status().accepted, 0);
        append(&state, service, runtime, 2, boundary + 1, "after-opt-in");
        export_pass(&state).await.unwrap();
        delivered(&exporter, 1).await;
        assert_eq!(received.lock().unwrap().len(), 1);
        let payload = received.lock().unwrap()[0].to_string();
        assert!(payload.contains("after-opt-in"));
        assert!(!payload.contains("before-opt-in"));
        assert!(payload.contains(&service.to_string()));
        request.resource = Some(create.ownership.resource.clone());
        request.operation_id = Uuid::new_v4();
        let noop = state
            .store
            .plan_resource(request.clone(), crate::secrets::now_ms())
            .unwrap();
        assert_eq!(noop.action, firecrab_stack::ResourcePlanAction::Noop);
        assert_eq!(apply_api(&app, &noop).await.0, StatusCode::OK);
        assert_eq!(
            state.store.otlp_configuration(service).unwrap().unwrap().1,
            boundary
        );
        assert_eq!(
            state.observations.sessions.lock().unwrap()[&service]
                .exporter
                .status()
                .delivered,
            1
        );
        export_pass(&state).await.unwrap();
        assert_eq!(exporter.status().accepted, 1);
        request.operation_id = Uuid::new_v4();
        let Some(ResourceConfiguration::ServiceOtlp(config)) = request.desired.as_mut() else {
            unreachable!()
        };
        config.endpoint = None;
        let disable = state
            .store
            .plan_resource(request.clone(), crate::secrets::now_ms())
            .unwrap();
        assert_eq!(apply_api(&app, &disable).await.0, StatusCode::OK);
        assert!(!exporter.status().enabled);
        assert!(
            !state
                .observations
                .sessions
                .lock()
                .unwrap()
                .contains_key(&service)
        );
        assert_eq!(apply_api(&app, &create).await, (StatusCode::OK, original));
        assert!(
            !state
                .observations
                .sessions
                .lock()
                .unwrap()
                .contains_key(&service)
        );
        append(&state, service, runtime, 3, boundary + 2, "while-disabled");
        export_pass(&state).await.unwrap();
        request.operation_id = Uuid::new_v4();
        let Some(ResourceConfiguration::ServiceOtlp(config)) = request.desired.as_mut() else {
            unreachable!()
        };
        config.endpoint = Some(endpoint);
        let enable = state
            .store
            .plan_resource(request, crate::secrets::now_ms())
            .unwrap();
        assert_eq!(apply_api(&app, &enable).await.0, StatusCode::OK);
        let exporter = state.observations.sessions.lock().unwrap()[&service]
            .exporter
            .clone();
        export_pass(&state).await.unwrap();
        assert_eq!(exporter.status().accepted, 0);
        let boundary = state.store.otlp_configuration(service).unwrap().unwrap().1;
        append(
            &state,
            service,
            runtime,
            4,
            boundary + 1,
            "after-new-opt-in",
        );
        export_pass(&state).await.unwrap();
        delivered(&exporter, 1).await;
        let payload = serde_json::to_string(&*received.lock().unwrap()).unwrap();
        assert!(!payload.contains("while-disabled"));
        assert!(payload.contains("after-new-opt-in"));
        state.observations.remove(service);
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn iac_resource_otlp_runtime_late_activation_cannot_restore_an_old_endpoint() {
    let root = tempfile::tempdir().unwrap();
    let (state, _app, mut request) = fixture(
        root.path(),
        Manager::Terraform,
        "http://127.0.0.1:9/old".into(),
    )
    .await;
    let now = crate::secrets::now_ms();
    let plan = state.store.plan_resource(request.clone(), now).unwrap();
    let command = ApplyResourcePlan {
        expected_endpoint_id: plan.ownership.endpoint_id,
        plan_sha256: plan.plan_sha256.clone(),
    };
    let preparation = state
        .store
        .resource_plan_otlp_input(plan.operation_id, &command, now)
        .unwrap()
        .unwrap();
    let prepared = prepare_plan(&state, preparation, Uuid::new_v4())
        .await
        .unwrap();
    let receipt = state
        .store
        .apply_resource_plan(plan.operation_id, &command, now + 1)
        .unwrap();
    request.resource = Some(receipt.ownership.resource.clone());
    request.operation_id = Uuid::new_v4();
    let Some(ResourceConfiguration::ServiceOtlp(config)) = request.desired.as_mut() else {
        unreachable!()
    };
    config.endpoint = Some("http://127.0.0.1:9/new".into());
    let next = state.store.plan_resource(request, now + 2).unwrap();
    state
        .store
        .apply_resource_plan(
            next.operation_id,
            &ApplyResourcePlan {
                expected_endpoint_id: next.ownership.endpoint_id,
                plan_sha256: next.plan_sha256.clone(),
            },
            now + 3,
        )
        .unwrap();
    activate_plan(&state, &receipt, prepared, Uuid::new_v4()).await;
    let service = receipt.ownership.resource.id.parse().unwrap();
    assert!(
        !state
            .observations
            .sessions
            .lock()
            .unwrap()
            .contains_key(&service)
    );
    assert_eq!(
        state.store.otlp_endpoint(service).unwrap().unwrap(),
        "http://127.0.0.1:9/new"
    );
}

#[tokio::test]
async fn iac_resource_otlp_runtime_failed_sql_ack_leaves_no_setting_or_exporter() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, request) = fixture(
        root.path(),
        Manager::Terraform,
        "http://127.0.0.1:9/collector".into(),
    )
    .await;
    let plan = state
        .store
        .plan_resource(request, crate::secrets::now_ms())
        .unwrap();
    let service = plan.ownership.resource.id.parse().unwrap();
    let conn = rusqlite::Connection::open(root.path().join("state.db")).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_otlp_ack BEFORE UPDATE OF receipt ON iac_resource_plans BEGIN SELECT RAISE(ABORT,'injected_lost_ack'); END").unwrap();
    assert!(apply_api(&app, &plan).await.0.is_server_error());
    assert!(state.store.otlp_configuration(service).unwrap().is_none());
    assert!(
        !state
            .observations
            .sessions
            .lock()
            .unwrap()
            .contains_key(&service)
    );
    conn.execute_batch("DROP TRIGGER reject_otlp_ack").unwrap();
    assert_eq!(apply_api(&app, &plan).await.0, StatusCode::OK);
    assert!(
        state
            .observations
            .sessions
            .lock()
            .unwrap()
            .contains_key(&service)
    );
    state.observations.remove(service);
}

#[tokio::test]
async fn iac_resource_otlp_settings_read_excludes_application_data_and_does_not_start_export() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, request) = fixture(
        root.path(),
        Manager::Terraform,
        "https://collector.example.test".into(),
    )
    .await;
    let plan = state
        .store
        .plan_resource(request, crate::secrets::now_ms())
        .unwrap();
    let service: Uuid = plan.ownership.resource.id.parse().unwrap();
    // Persist the setting without installing a process-local exporter.
    state
        .store
        .apply_resource_plan(
            plan.operation_id,
            &ApplyResourcePlan {
                expected_endpoint_id: plan.ownership.endpoint_id,
                plan_sha256: plan.plan_sha256,
            },
            crate::secrets::now_ms(),
        )
        .unwrap();
    append(
        &state,
        service,
        (Uuid::new_v4(), Uuid::new_v4()),
        1,
        1,
        "private-application-data",
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/micro-services/{service}/observability/otlp"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 128 * 1024).await.unwrap();
    let view: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(view["endpoint"], "https://collector.example.test/");
    assert_eq!(view["serviceId"], service.to_string());
    assert_eq!(view["ownership"]["owner"]["manager"], "terraform");
    assert!(view["enabledAtMs"].as_i64().unwrap() > 0);
    assert_eq!(view.as_object().unwrap().len(), 4);
    assert!(
        !String::from_utf8(body.to_vec())
            .unwrap()
            .contains("private-application-data")
    );
    assert!(
        !state
            .observations
            .sessions
            .lock()
            .unwrap()
            .contains_key(&service)
    );
}

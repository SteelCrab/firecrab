use super::*;
use crate::templates::TemplateRegistry;
use axum::response::IntoResponse;
use firecrab_api_types::{MicroServiceResponse, MicroServiceSpec};

fn spec() -> MicroServiceSpec {
    serde_json::from_value(serde_json::json!({
        "name": "web", "image": "127.0.0.1:1/app:v1", "port": 8080,
        "ram": 128, "cpu": 1,
    }))
    .unwrap()
}

async fn state(root: &std::path::Path) -> AppState {
    AppState::with_db_file(
        TemplateRegistry::from_specs(root, std::iter::empty()).unwrap(),
        root.join("state.db"),
    )
    .await
    .unwrap()
}

fn deployment(id: Uuid) -> MicroServiceDeployment {
    MicroServiceDeployment {
        id: Uuid::new_v4(),
        service_id: id,
        phase: ServicePhase::Queued,
        spec: spec(),
        request: DeployMicroServiceRequest {
            micro_network_id: None,
            host_port: 30100,
        },
        digest: None,
        instances: vec![],
        log: vec![],
        error: None,
    }
}

#[test]
fn deployments_reject_unsupported_configuration_and_port_overflow_before_claiming() {
    let request = DeployMicroServiceRequest {
        micro_network_id: None,
        host_port: 30100,
    };
    assert!(deployment_fields(&spec(), &request).is_empty());
    let mut spec = spec();
    spec.domain = Some("example.com".into());
    spec.secrets.insert("PASSWORD".into(), Uuid::new_v4());
    let fields = deployment_fields(&spec, &request);
    assert!(fields.contains_key("domain"));
    assert!(fields.contains_key("secrets"));
    spec.replicas = 2;
    for port in [0, 80, 5522, 5523, 65535] {
        assert!(
            deployment_fields(
                &spec,
                &DeployMicroServiceRequest {
                    host_port: port,
                    ..request.clone()
                }
            )
            .contains_key("hostPort")
        );
    }
}

#[tokio::test]
async fn queued_deployment_blocks_duplicate_deploy_edit_delete_and_stop() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let id = Uuid::new_v4();
    state
        .store
        .insert_micro_service(&MicroServiceResponse { id, spec: spec() })
        .unwrap();
    let _queue = state.service_deploy_permits.acquire().await.unwrap();
    let request = DeployMicroServiceRequest {
        micro_network_id: None,
        host_port: 30100,
    };
    let (status, Json(record)) = deploy(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(id.to_string()),
        ValidatedJson(request.clone()),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(record.phase, ServicePhase::Queued);
    assert_eq!(
        state.store.service_deployment(id).unwrap().unwrap().id,
        record.id
    );
    let error = deploy(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(id.to_string()),
        ValidatedJson(request),
    )
    .await
    .unwrap_err();
    assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    assert!(ensure_editable(&state, id, Uuid::new_v4()).await.is_err());
    let error = stop(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(id.to_string()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn health_checks_require_http_success_and_do_not_follow_redirects() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = axum::Router::new()
        .route("/health", axum::routing::get(|| async { StatusCode::OK }))
        .route(
            "/redirect",
            axum::routing::get(|| async { axum::response::Redirect::temporary("/health") }),
        )
        .route(
            "/failed",
            axum::routing::get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut spec = spec();
    spec.port = port;
    let instance = ServiceInstance {
        vm_id: Uuid::new_v4(),
        ipv4: "127.0.0.1".into(),
        host_port: 30100,
        state: VmState::Running,
        healthy: false,
    };
    let client = health_client();
    assert!(healthy(&client, &spec, &instance).await);
    spec.health_path = "/redirect".into();
    assert!(!healthy(&client, &spec, &instance).await);
    spec.health_path = "/failed".into();
    spec.startup_timeout = 1;
    assert!(wait_healthy(&spec, &instance).await.is_err());
    server.abort();
}

#[tokio::test]
async fn restart_records_interrupted_operations_and_cleanup_releases_ownership() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let id = Uuid::new_v4();
    state
        .store
        .insert_micro_service(&MicroServiceResponse { id, spec: spec() })
        .unwrap();
    let mut record = deployment(id);
    record.instances.push(ServiceInstance {
        vm_id: Uuid::new_v4(),
        ipv4: "172.30.0.2".into(),
        host_port: 30100,
        state: VmState::Running,
        healthy: true,
    });
    save(&state, &record).await.unwrap();
    recover(&state).await;
    let mut record = state.store.service_deployment(id).unwrap().unwrap();
    assert_eq!(record.phase, ServicePhase::Failed);
    assert!(record.error.as_ref().unwrap().contains("interrupted"));
    assert_eq!(record.instances.len(), 1);
    assert!(ensure_editable(&state, id, Uuid::new_v4()).await.is_err());
    cleanup(&state, &mut record).await.unwrap();
    assert!(
        state
            .store
            .service_deployment(id)
            .unwrap()
            .unwrap()
            .instances
            .is_empty()
    );
    assert!(ensure_editable(&state, id, Uuid::new_v4()).await.is_ok());
}

#[tokio::test]
async fn live_status_does_not_claim_a_missing_vm_is_running() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut record = deployment(Uuid::new_v4());
    record.phase = ServicePhase::Running;
    record.instances.push(ServiceInstance {
        vm_id: Uuid::new_v4(),
        ipv4: "172.30.0.2".into(),
        host_port: 30100,
        state: VmState::Running,
        healthy: true,
    });
    refresh_instances(&state, &mut record).await;
    assert_eq!(record.phase, ServicePhase::Degraded);
    assert_eq!(record.instances[0].state, VmState::Stopped);
    assert!(!record.instances[0].healthy);
}

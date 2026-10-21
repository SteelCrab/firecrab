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
    assert!(deployment_fields(&spec(), &request, 5523).is_empty());
    let mut spec = spec();
    spec.domain = Some("example.com".into());
    spec.secrets.insert("PASSWORD".into(), Uuid::new_v4());
    let fields = deployment_fields(&spec, &request, 5523);
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
                },
                5523,
            )
            .contains_key("hostPort")
        );
    }
}

#[test]
fn deployments_reserve_the_bound_api_port_across_the_replica_range() {
    let mut spec = spec();
    spec.replicas = 2;
    let request = DeployMicroServiceRequest {
        micro_network_id: None,
        host_port: 30100,
    };
    assert!(deployment_fields(&spec, &request, 30101).contains_key("hostPort"));
    assert!(deployment_fields(&spec, &request, 30102).is_empty());
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

#[tokio::test]
async fn stop_does_not_wait_for_another_deployment_permit() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let id = Uuid::new_v4();
    let mut record = deployment(id);
    record.phase = ServicePhase::Failed;
    save(&state, &record).await.unwrap();
    let _other_deployment = state.service_deploy_permits.acquire().await.unwrap();
    let (status, _) = stop(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(id.to_string()),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::ACCEPTED);
    wait_for_phase(&state, id, ServicePhase::Stopped).await;
}

/// Poll the stored deployment until the background task reaches `want`.
async fn wait_for_phase(state: &AppState, id: Uuid, want: ServicePhase) -> MicroServiceDeployment {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let record = state.store.service_deployment(id).unwrap().unwrap();
            if record.phase == want {
                return record;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("deployment never reached {want:?}"))
}

/// Serve one single-platform manifest at `/v2/app/manifests/v1` and return the
/// registry host and the manifest digest the service pins.
async fn manifest_registry() -> (String, String) {
    let body = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": format!("sha256:{}", "c".repeat(64)),
            "size": 7
        },
        "layers": [{
            "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
            "digest": format!("sha256:{}", "d".repeat(64)),
            "size": 99
        }]
    }))
    .unwrap();
    let digest = format!("sha256:{:x}", <sha2::Sha256 as sha2::Digest>::digest(&body));
    let app = axum::Router::new().route(
        "/v2/app/manifests/v1",
        axum::routing::get(move || {
            let body = body.clone();
            async move {
                (
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "application/vnd.oci.image.manifest.v1+json",
                    )],
                    body,
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (address.to_string(), digest)
}

/// A VM test state whose fake Firecracker boots, plus a registered template
/// for a loopback image so deployment skips the OCI import.
async fn bootable_service(root: &std::path::Path) -> (AppState, Uuid, MicroServiceSpec, String) {
    use crate::firecracker::test_support::{SERVE_LOOP, fake_firecracker};
    let binary = fake_firecracker(
        root,
        &format!("signal.signal(signal.SIGTERM, lambda *_: sys.exit(0)){SERVE_LOOP}"),
    );
    let state = vms::test_support::test_state_with_binary(root, binary).await;
    let (registry, digest) = manifest_registry().await;
    let mut spec = spec();
    spec.image = format!("{registry}/app:v1");
    spec.startup_timeout = 1;
    let pinned = ImageReference {
        version: ImageVersion::Digest(Sha256Digest::parse(&digest).unwrap()),
        ..ImageReference::parse(&spec.image).unwrap()
    };
    let alias = oci::template_name_from_reference(&pinned).unwrap().alias;
    state
        .templates
        .register_spec(crate::templates::TemplateSpec {
            alias: alias.clone(),
            version: digest,
            kernel: "kernel".into(),
            initrd: None,
            rootfs: "rootfs".into(),
            boot_args: "console=ttyS0".into(),
        })
        .unwrap();
    let id = Uuid::new_v4();
    state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id,
            spec: spec.clone(),
        })
        .unwrap();
    (state, id, spec, alias)
}

/// A free host port for a replica's forward.
fn free_port() -> u16 {
    std::net::TcpListener::bind("0.0.0.0:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn failed_app_health_deletes_the_booted_replica_and_records_the_reason() {
    let directory = crate::firecracker::test_support::short_tempdir();
    let (state, id, _, _) = bootable_service(directory.path()).await;
    let (status, Json(queued)) = deploy(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(id.to_string()),
        ValidatedJson(DeployMicroServiceRequest {
            micro_network_id: None,
            host_port: free_port(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(queued.phase, ServicePhase::Queued);
    let record = wait_for_phase(&state, id, ServicePhase::Failed).await;
    let error = record.error.unwrap();
    assert!(error.contains("App health check /health failed"), "{error}");
    assert!(record.digest.is_some());
    assert!(
        record.instances.is_empty(),
        "owned replicas must be deleted"
    );
    assert!(state.vms.lock().unwrap().is_empty());
    assert!(
        record
            .log
            .iter()
            .any(|line| line == "Creating replica 1 of 1")
    );
    assert!(
        record
            .log
            .iter()
            .any(|line| line == "Waiting for replica 1 app health")
    );
    let networks = state.store.list_micro_networks().unwrap();
    let services = networks
        .iter()
        .find(|network| network.name == "services")
        .unwrap();
    assert_eq!(record.request.micro_network_id, Some(services.id));
    assert_eq!(
        default_network(&state, Uuid::new_v4()).await.unwrap(),
        services.id,
        "the services network is reused"
    );
}

#[tokio::test]
async fn stop_withdraws_ports_drains_and_deletes_a_running_replica() {
    let directory = crate::firecracker::test_support::short_tempdir();
    let (state, id, mut spec, alias) = bootable_service(directory.path()).await;
    spec.drain_timeout = 1;
    let network_id = default_network(&state, Uuid::new_v4()).await.unwrap();
    let host_port = free_port();
    let (_, Json(vm)) = vms::create_vm(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        ValidatedJson(CreateVmRequest {
            name: "ms-stop-test".into(),
            template: alias,
            ram: spec.ram,
            cpu: spec.cpu,
            disk_gb: 1,
            egress_policy: EgressPolicy::Internet,
            micro_network_id: network_id,
            storage_root: None,
            shell_ids: vec![],
            env: BTreeMap::new(),
            port_forwards: vec![PortForward {
                host_port,
                guest_port: spec.port,
                protocol: PortProtocol::Tcp,
            }],
        }),
    )
    .await
    .unwrap();
    let Json(vm) = vms::start_vm(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(vm.id.to_string()),
    )
    .await
    .unwrap();
    assert_eq!(vm.state, VmState::Running);
    let mut record = deployment(id);
    record.spec = spec;
    record.phase = ServicePhase::Running;
    record.instances.push(ServiceInstance {
        vm_id: vm.id,
        ipv4: vm.ipv4.unwrap_or_default(),
        host_port,
        state: VmState::Running,
        healthy: true,
    });
    save(&state, &record).await.unwrap();
    let (status, _) = stop(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(id.to_string()),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::ACCEPTED);
    let record = wait_for_phase(&state, id, ServicePhase::Stopped).await;
    assert!(record.instances.is_empty());
    assert!(
        record
            .log
            .iter()
            .any(|line| line.starts_with("Withdrew host ports"))
    );
    assert!(state.vms.lock().unwrap().is_empty());
}

#[tokio::test]
async fn deploy_rejects_unknown_services_networks_and_unsupported_settings() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let request = DeployMicroServiceRequest {
        micro_network_id: None,
        host_port: 30100,
    };
    let status = |result: Result<(StatusCode, Json<MicroServiceDeployment>), AppError>| {
        result.unwrap_err().into_response().status()
    };
    let missing = Uuid::new_v4().to_string();
    let call = |id: String, request: DeployMicroServiceRequest| {
        deploy(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(id),
            ValidatedJson(request),
        )
    };
    assert_eq!(
        status(call(missing, request.clone()).await),
        StatusCode::NOT_FOUND
    );
    let id = Uuid::new_v4();
    let mut routed = spec();
    routed.name = "routed".into();
    routed.domain = Some("app.example.com".into());
    state
        .store
        .insert_micro_service(&MicroServiceResponse { id, spec: routed })
        .unwrap();
    assert_eq!(
        status(call(id.to_string(), request.clone()).await),
        StatusCode::BAD_REQUEST
    );
    let id = Uuid::new_v4();
    state
        .store
        .insert_micro_service(&MicroServiceResponse { id, spec: spec() })
        .unwrap();
    let unknown_network = DeployMicroServiceRequest {
        micro_network_id: Some(Uuid::new_v4()),
        ..request
    };
    assert_eq!(
        status(call(id.to_string(), unknown_network).await),
        StatusCode::BAD_REQUEST
    );
    assert!(state.store.service_deployment(id).unwrap().is_none());
}

#[tokio::test]
async fn get_reports_live_instance_state_and_rejects_unknown_services() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let call = |id: Uuid| {
        get(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(id.to_string()),
        )
    };
    let error = call(Uuid::new_v4()).await.unwrap_err();
    assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    let id = Uuid::new_v4();
    state
        .store
        .insert_micro_service(&MicroServiceResponse { id, spec: spec() })
        .unwrap();
    assert!(call(id).await.unwrap().0.is_none());
    let mut record = deployment(id);
    record.phase = ServicePhase::Running;
    record.instances.push(ServiceInstance {
        vm_id: Uuid::new_v4(),
        ipv4: "172.30.0.2".into(),
        host_port: 30100,
        state: VmState::Running,
        healthy: true,
    });
    save(&state, &record).await.unwrap();
    let Json(Some(live)) = call(id).await.unwrap() else {
        panic!("saved deployment must be returned");
    };
    assert_eq!(live.phase, ServicePhase::Degraded);
    assert_eq!(live.instances[0].state, VmState::Stopped);
}

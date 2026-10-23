use super::*;
use crate::templates::TemplateRegistry;
use axum::response::IntoResponse;
use firecrab_api_types::{MicroServiceResponse, MicroServiceSpec};

fn spec() -> MicroServiceSpec {
    serde_json::from_value(serde_json::json!({
        "name": "web", "image": "ghcr.io/team/app:v1", "port": 8080,
        "ram": 128, "cpu": 1,
    }))
    .unwrap()
}

async fn state(root: &std::path::Path) -> AppState {
    let state = AppState::with_db_file(
        TemplateRegistry::from_specs(root, std::iter::empty()).unwrap(),
        root.join("state.db"),
    )
    .await
    .unwrap();
    state
        .store
        .put_pipe_auth(&crate::pipe_auth::SavedAuth::Github {
            username: "team".into(),
            token: "test-only-secret".into(),
        })
        .unwrap();
    state
}

fn deployment(id: Uuid) -> MicroServiceDeployment {
    MicroServiceDeployment {
        id: Uuid::new_v4(),
        service_id: id,
        phase: ServicePhase::Queued,
        domain_route_owned: false,
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
    assert!(!fields.contains_key("domain"));
    spec.domain = None;
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
    let (state, id, spec, _) = bootable_service(directory.path()).await;
    // Exercise VM lifecycle with the loopback manifest fixture after the AUTH
    // boundary. Separate API tests reject loopback images and revoked accounts.
    let mut queued = deployment(id);
    queued.spec = spec;
    queued.request.host_port = free_port();
    save(&state, &queued).await.unwrap();
    let task_state = state.clone();
    tokio::spawn(async move {
        let credential = oci::RegistryCredential {
            registry: ImageReference::parse(&queued.spec.image).unwrap().registry,
            username: "fixture".into(),
            secret: "fixture".into(),
        };
        if let Err(error) = execute(&task_state, &mut queued, credential).await {
            fail_deployment(&task_state, &mut queued, error).await;
        }
    });
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
        StatusCode::SERVICE_UNAVAILABLE
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

#[tokio::test]
async fn service_runtime_auth_rejects_unowned_saved_images_before_queuing() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let _queue = state.service_deploy_permits.acquire().await.unwrap();
    for image in [
        "nginx:1.27",
        "127.0.0.1:5000/app:v1",
        "ghcr.io/other/app:v1",
    ] {
        let id = Uuid::new_v4();
        let mut spec = spec();
        spec.name = format!("service-{}", id.simple());
        spec.image = image.into();
        state
            .store
            .insert_micro_service(&MicroServiceResponse { id, spec })
            .unwrap();
        let error = deploy(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(id.to_string()),
            ValidatedJson(DeployMicroServiceRequest {
                micro_network_id: None,
                host_port: 30100,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
        assert!(state.store.service_deployment(id).unwrap().is_none());
    }
    assert!(state.vms.lock().unwrap().is_empty());
}

#[tokio::test]
async fn service_runtime_auth_rechecks_revoked_and_replaced_accounts_after_queueing() {
    for replacement in [None, Some("other")] {
        let root = tempfile::tempdir().unwrap();
        let state = state(root.path()).await;
        let id = Uuid::new_v4();
        state
            .store
            .insert_micro_service(&MicroServiceResponse { id, spec: spec() })
            .unwrap();
        state
            .store
            .put_registry_login(
                &oci::RegistryCredential {
                    registry: "ghcr.io".into(),
                    username: "team".into(),
                    secret: "fallback-must-not-be-used".into(),
                },
                None,
            )
            .unwrap();
        let queue = state.service_deploy_permits.acquire().await.unwrap();
        let (_, Json(queued)) = deploy(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(id.to_string()),
            ValidatedJson(DeployMicroServiceRequest {
                micro_network_id: None,
                host_port: 30100,
            }),
        )
        .await
        .unwrap();
        assert_eq!(queued.phase, ServicePhase::Queued);
        state
            .store
            .delete_pipe_auth(crate::pipe_auth::Provider::Github)
            .unwrap();
        if let Some(username) = replacement {
            state
                .store
                .put_pipe_auth(&crate::pipe_auth::SavedAuth::Github {
                    username: username.into(),
                    token: "other-private-token".into(),
                })
                .unwrap();
        }
        drop(queue);
        let failed = wait_for_phase(&state, id, ServicePhase::Failed).await;
        assert_eq!(
            failed.error.as_deref(),
            Some("Image account is no longer connected in Pipe Auth")
        );
        assert!(failed.digest.is_none());
        assert!(failed.instances.is_empty());
        assert!(
            !failed
                .log
                .iter()
                .any(|line| line.contains("Resolving image"))
        );
        assert!(
            !serde_json::to_string(&failed)
                .unwrap()
                .contains("fallback-must-not-be-used")
        );
        assert!(state.vms.lock().unwrap().is_empty());
    }
}

/// A framed helper with an isolated Pipe manifest. No host services or networking are changed.
async fn domain_helper(
    root: &std::path::Path,
    fail_after_publish: bool,
) -> (
    crate::network::NetworkClient,
    std::sync::Arc<std::sync::Mutex<firecrab_helper_protocol::pipe::Config>>,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use firecrab_helper_protocol::{
        PROTOCOL_VERSION,
        framing::{read_frame, write_frame},
        network::{HelperFailure, NetworkRequest, NetworkRequestEnvelope, NetworkResponseEnvelope},
        pipe::{Config, Operation, ServiceRouteRequest, Snapshot},
    };
    let path = root.join("domain.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let config = std::sync::Arc::new(std::sync::Mutex::new(
        serde_json::from_value::<Config>(serde_json::json!({"routes":[],"tunnel":null})).unwrap(),
    ));
    let reject_remove = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let saved = config.clone();
    let reject = reject_remove.clone();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let saved = saved.clone();
            let reject = reject.clone();
            tokio::spawn(async move {
                let envelope: NetworkRequestEnvelope = read_frame(&mut stream).await.unwrap();
                let mut result = Ok(());
                let snapshot = {
                    let mut config = saved.lock().unwrap();
                    if let NetworkRequest::Pipe {
                        request: Operation::ManageServiceRoute { request },
                    } = envelope.request
                    {
                        if matches!(request, ServiceRouteRequest::Remove { .. })
                            && reject.load(std::sync::atomic::Ordering::SeqCst)
                        {
                            result = Err(HelperFailure::Internal {
                                detail: "isolated withdraw failure".into(),
                            });
                        } else {
                            match request.updated_config(&config) {
                                Ok(next) => *config = next,
                                Err(detail) => result = Err(HelperFailure::Internal { detail }),
                            }
                            if fail_after_publish
                                && matches!(request, ServiceRouteRequest::Publish { .. })
                            {
                                result = Err(HelperFailure::Internal {
                                    detail: "response lost after publication".into(),
                                });
                            }
                        }
                    }
                    Snapshot {
                        config: Some(config.clone()),
                        units: vec![],
                        nginx_available: true,
                        cloudflared_available: false,
                        quick_tunnel: None,
                        quick_tunnels: vec![],
                        firecrab_tunnels: vec![],
                    }
                };
                write_frame(
                    &mut stream,
                    &NetworkResponseEnvelope {
                        version: PROTOCOL_VERSION,
                        request_id: envelope.request_id,
                        result,
                        pipe: Some(snapshot),
                    },
                )
                .await
                .unwrap();
            });
        }
    });
    (
        crate::network::NetworkClient::with_socket_path(path),
        config,
        reject_remove,
    )
}

#[tokio::test]
async fn domain_deployments_accept_empty_requests_and_reject_existing_routes_before_queueing() {
    let root = crate::firecracker::test_support::short_tempdir();
    let mut state = state(root.path()).await;
    let (helper, config, _) = domain_helper(root.path(), false).await;
    state.network = helper;
    let request: DeployMicroServiceRequest = serde_json::from_value(serde_json::json!({})).unwrap();
    let mut spec = spec();
    spec.domain = Some("web.example.com".into());
    assert!(deployment_fields(&spec, &request, 5523).is_empty());
    let id = Uuid::new_v4();
    state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id,
            spec: spec.clone(),
        })
        .unwrap();
    domain::preflight(&state, id, &spec, Uuid::new_v4())
        .await
        .unwrap();
    config.lock().unwrap().routes.push(
        serde_json::from_value(
            serde_json::json!({"host":"web.example.com","upstreams":["127.0.0.1:9000"]}),
        )
        .unwrap(),
    );
    let error = deploy(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(id.to_string()),
        ValidatedJson(request),
    )
    .await
    .unwrap_err();
    assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
    assert!(state.store.service_deployment(id).unwrap().is_none());
    assert_eq!(config.lock().unwrap().routes.len(), 1);
}

#[tokio::test]
async fn domain_publication_is_durable_and_cleanup_retries_without_abandoning_instances() {
    let root = crate::firecracker::test_support::short_tempdir();
    let mut state = state(root.path()).await;
    let (helper, config, reject_remove) = domain_helper(root.path(), true).await;
    state.network = helper;
    let id = Uuid::new_v4();
    let mut record = deployment(id);
    record.spec.domain = Some("web.example.com".into());
    record.instances.push(ServiceInstance {
        vm_id: Uuid::new_v4(),
        ipv4: "172.30.0.2".into(),
        host_port: 0,
        state: VmState::Running,
        healthy: true,
    });
    state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id,
            spec: record.spec.clone(),
        })
        .unwrap();
    assert!(domain::publish(&state, &mut record).await.is_err());
    let persisted = state.store.service_deployment(id).unwrap().unwrap();
    assert!(persisted.domain_route_owned);
    assert_eq!(persisted.phase, ServicePhase::ConnectingDomain);
    assert_eq!(
        config.lock().unwrap().routes[0].upstreams[0].to_string(),
        "172.30.0.2:8080"
    );
    recover(&state).await;
    assert_eq!(
        state.store.service_deployment(id).unwrap().unwrap().phase,
        ServicePhase::Failed
    );
    reject_remove.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(cleanup(&state, &mut record).await.is_err());
    assert!(record.domain_route_owned);
    assert_eq!(record.instances.len(), 1);
    assert!(ensure_editable(&state, id, Uuid::new_v4()).await.is_err());
    reject_remove.store(false, std::sync::atomic::Ordering::SeqCst);
    cleanup(&state, &mut record).await.unwrap();
    assert!(!record.domain_route_owned);
    assert!(record.instances.is_empty());
    assert!(config.lock().unwrap().routes.is_empty());
    assert!(
        !state
            .store
            .service_deployment(id)
            .unwrap()
            .unwrap()
            .domain_route_owned
    );
}

/// Return scripted publication failures through the real framed helper transport.
async fn publication_helper(
    root: &std::path::Path,
    failures: Vec<firecrab_helper_protocol::network::HelperFailure>,
) -> (
    crate::network::NetworkClient,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    use firecrab_helper_protocol::{
        PROTOCOL_VERSION,
        framing::{read_frame, write_frame},
        network::{NetworkRequest, NetworkRequestEnvelope, NetworkResponseEnvelope},
        pipe::{Operation, ServiceRouteRequest, Snapshot},
    };
    let path = root.join("publication.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls = attempts.clone();
    let task = tokio::spawn(async move {
        let mut failures = failures.into_iter();
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let envelope: NetworkRequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert!(matches!(
                envelope.request,
                NetworkRequest::Pipe {
                    request: Operation::ManageServiceRoute {
                        request: ServiceRouteRequest::Publish { .. }
                    }
                }
            ));
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            write_frame(
                &mut stream,
                &NetworkResponseEnvelope {
                    version: PROTOCOL_VERSION,
                    request_id: envelope.request_id,
                    result: failures.next().map_or(Ok(()), Err),
                    pipe: Some(Snapshot {
                        config: None,
                        units: vec![],
                        nginx_available: true,
                        cloudflared_available: false,
                        quick_tunnel: None,
                        quick_tunnels: vec![],
                        firecrab_tunnels: vec![],
                    }),
                },
            )
            .await
            .unwrap();
        }
    });
    (
        crate::network::NetworkClient::with_socket_path(path),
        attempts,
        task,
    )
}

/// Retry bounded lock contention, but fail immediately for conflicts and other helper errors.
#[tokio::test]
async fn domain_publication_retries_only_exact_transient_lock_errors() {
    use firecrab_helper_protocol::network::HelperFailure;
    let busy = || HelperFailure::Internal {
        detail: "[ERROR] another pipe change is running\n".into(),
    };
    let conflict = || HelperFailure::Internal {
        detail: "Domain is already used by another Pipe route or service".into(),
    };
    let cases = [
        ("busy then success", vec![busy()], 2, true),
        (
            "retry budget exhausted",
            vec![busy(), busy(), busy()],
            3,
            false,
        ),
        ("domain conflict", vec![conflict()], 1, false),
        (
            "busy then domain conflict",
            vec![busy(), conflict()],
            2,
            false,
        ),
        (
            "different error detail",
            vec![HelperFailure::Internal {
                detail: "[ERROR] another pipe change is running\nadditional failure".into(),
            }],
            1,
            false,
        ),
        (
            "different error type",
            vec![HelperFailure::InvalidRequest {
                detail: "[ERROR] another pipe change is running\n".into(),
            }],
            1,
            false,
        ),
    ];
    for (name, failures, expected_attempts, succeeds) in cases {
        let root = crate::firecracker::test_support::short_tempdir();
        let mut state = state(root.path()).await;
        let (client, attempts, helper) = publication_helper(root.path(), failures).await;
        state.network = client;
        let id = Uuid::new_v4();
        let mut record = deployment(id);
        record.spec.domain = Some("web.example.com".into());
        record.instances.push(ServiceInstance {
            vm_id: Uuid::new_v4(),
            ipv4: "172.30.0.2".into(),
            host_port: 0,
            state: VmState::Running,
            healthy: true,
        });
        state
            .store
            .insert_micro_service(&MicroServiceResponse {
                id,
                spec: record.spec.clone(),
            })
            .unwrap();
        let result = domain::publish(&state, &mut record).await;
        helper.abort();
        assert_eq!(result.is_ok(), succeeds, "{name}: {result:?}");
        assert_eq!(
            attempts.load(std::sync::atomic::Ordering::SeqCst),
            expected_attempts,
            "{name}"
        );
        assert!(
            state
                .store
                .service_deployment(id)
                .unwrap()
                .unwrap()
                .domain_route_owned,
            "{name}"
        );
    }
}

#[tokio::test]
async fn placement_checks_reserved_vm_tcp_ports_across_all_replicas_but_allows_udp() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(directory.path()).await;
    let owner = Uuid::new_v4();
    let port = free_port().min(65534);
    let request = DeployMicroServiceRequest {
        micro_network_id: None,
        host_port: port,
    };
    let forward = |protocol| PortForward {
        host_port: port + 1,
        guest_port: 8080,
        protocol,
    };
    state
        .store
        .set_vm_port_forwards(owner, &[forward(PortProtocol::Udp)])
        .unwrap();
    assert!(
        host_port_conflict(&state, &request, 2, Uuid::new_v4())
            .await
            .unwrap()
            .is_none()
    );
    state
        .store
        .set_vm_port_forwards(owner, &[forward(PortProtocol::Tcp)])
        .unwrap();
    let conflict = host_port_conflict(&state, &request, 2, Uuid::new_v4())
        .await
        .unwrap()
        .unwrap();
    assert!(conflict.contains(&(port + 1).to_string()));
    assert!(conflict.contains("MicroVM"));
}

#[tokio::test]
async fn domain_health_checks_use_the_requested_hostname() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = axum::Router::new().route(
        "/health",
        axum::routing::get(|headers: axum::http::HeaderMap| async move {
            if headers
                .get("host")
                .is_some_and(|value| value == "web.example.com")
            {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            }
        }),
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut spec = spec();
    spec.port = port;
    spec.domain = Some("web.example.com".into());
    let instance = ServiceInstance {
        vm_id: Uuid::new_v4(),
        ipv4: "127.0.0.1".into(),
        host_port: 0,
        state: VmState::Running,
        healthy: false,
    };
    assert!(healthy(&health_client(), &spec, &instance).await);
    spec.domain = None;
    assert!(!healthy(&health_client(), &spec, &instance).await);
    task.abort();
}

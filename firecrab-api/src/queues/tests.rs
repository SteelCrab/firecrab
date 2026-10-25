use super::*;
use crate::{handlers::vms::test_support::record, ipam::SubnetSpec, templates::TemplateRegistry};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;

#[test]
fn engine_template_matches_the_alias_registered_by_the_oci_importer() {
    let reference = crate::oci::ImageReference::parse(IMAGE).unwrap();
    let name = crate::oci::template_name_from_reference(&reference).unwrap();
    assert_eq!(name.alias, TEMPLATE);
    assert_eq!(name.version, IMAGE.split_once('@').unwrap().1);
}

async fn state(root: &std::path::Path) -> AppState {
    let templates = TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
    AppState::with_db_file(templates, root.join("state.db"))
        .await
        .unwrap()
}
fn fixtures(state: &AppState) -> (EngineRecord, QueueRecord) {
    let vm_id = Uuid::new_v4();
    let network_id = Uuid::new_v4();
    let engine = EngineRecord {
        vm_id,
        vm_created: true,
        spec: CreateQueueEngineRequest {
            micro_network_id: network_id,
            cpu: 4,
            ram: 4096,
            disk_gb: 8,
            storage_root: None,
        },
        phase: QueueEnginePhase::Running,
        error: None,
        username: "test_admin".into(),
        password: "private-engine-password".into(),
    };
    let mut vm = record("queue-engine", vm_id);
    vm.purpose = crate::model::VmPurpose::Queue;
    vm.micro_network_id = network_id;
    state.store.insert(&vm).unwrap();
    state.vms.lock().unwrap().insert(vm_id, vm);
    state
        .store
        .allocate_lease(vm_id, SubnetSpec::legacy_default_subnet(network_id))
        .unwrap();
    state.store.save_queue_engine(&engine).unwrap();
    let queue = QueueRecord {
        id: Uuid::new_v4(),
        name: "notifications".into(),
        delivery_limit: 5,
        max_bytes: 64 * 1024 * 1024,
        phase: QueuePhase::Ready,
        error: None,
        publish_password: "private-publisher-password".into(),
        consume_password: "private-consumer-password".into(),
    };
    state.store.save_queue(&queue).unwrap();
    (engine, queue)
}
fn spec(queue_id: Uuid) -> MicroServiceSpec {
    serde_json::from_value(json!({"name":"app","image":"ghcr.io/team/app:1","port":8080,"cpu":1,"ram":512,"queues":{"NOTIFICATIONS":{"queueId":queue_id,"access":"consume"}}})).unwrap()
}
fn router(state: AppState) -> Router {
    crate::server::build_router(
        state,
        &crate::server::HttpConfig {
            bind_addr: "127.0.0.1:5523".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 128,
            request_timeout: std::time::Duration::from_secs(10),
            static_root: None,
        },
    )
}
async fn request(
    router: Router,
    method: &str,
    path: &str,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let response = router
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (
        status,
        headers,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}
#[tokio::test]
async fn queue_records_survive_restart_without_exposing_passwords() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    let (engine, queue) = fixtures(&state);
    let (status, _, json) = request(router(state.clone()), "GET", "/api/queues").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json[0]["name"], "notifications");
    assert!(!json.to_string().contains("private"));
    let (_, _, json) = request(router(state.clone()), "GET", "/api/queue/engine").await;
    assert!(!json.to_string().contains("private"));
    drop(state);
    let store = Store::open(&dir.path().join("state.db")).unwrap();
    assert_eq!(store.queue_engine().unwrap().unwrap().vm_id, engine.vm_id);
    let saved = store.queue(queue.id).unwrap().unwrap();
    assert_eq!(saved.publish_password, queue.publish_password);
    assert_eq!(saved.phase, QueuePhase::Ready);
}
#[tokio::test]
async fn offline_cookie_repair_preserves_node_identity_and_existing_broker_data() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    let (engine, _) = fixtures(&state);
    let image = dir.path().join("broker.ext4");
    assert!(
        std::process::Command::new("mkfs.ext4")
            .args(["-q", "-F"])
            .arg(&image)
            .arg("16M")
            .status()
            .unwrap()
            .success()
    );
    for path in ["/etc", "/var", "/var/lib", "/var/lib/rabbitmq"] {
        crate::rootfs::run_debugfs(&image, &format!("mkdir {path}")).unwrap();
    }
    crate::rootfs::write_into_image(
        &image,
        "/etc/passwd",
        b"rabbitmq:x:999:999:RabbitMQ:/var/lib/rabbitmq:/bin/sh\n",
    )
    .unwrap();
    let work = "/var/lib/rabbitmq/work";
    crate::rootfs::write_into_image(&image, work, b"existing-broker-data").unwrap();
    prepare_engine_disk(&state.store, engine.vm_id, &image).unwrap();
    let cookie_path = "/var/lib/rabbitmq/.erlang.cookie";
    let read = |path: &str| {
        let output = dir.path().join("dump");
        crate::rootfs::dump_from_image(&image, path, &output).unwrap();
        std::fs::read(output).unwrap()
    };
    let original = read(cookie_path);
    assert_eq!(original.len(), 64);
    assert!(original.iter().all(u8::is_ascii_hexdigit));
    let stat = crate::rootfs::run_debugfs(&image, &format!("stat {cookie_path}")).unwrap();
    let stat = String::from_utf8_lossy(&stat.stdout);
    let fields: Vec<_> = stat.split_whitespace().collect();
    for (key, value) in [("Mode:", "0400"), ("User:", "999"), ("Group:", "999")] {
        assert!(fields.windows(2).any(|pair| pair == [key, value]), "{stat}");
    }
    // Reproduce the empty authentication file found after an early guest power-off.
    crate::rootfs::write_into_image(&image, cookie_path, b"").unwrap();
    prepare_engine_disk(&state.store, engine.vm_id, &image).unwrap();
    assert_eq!(read(cookie_path), original);
    assert_eq!(read(work), b"existing-broker-data");
}

#[tokio::test]
async fn binding_credentials_are_private_role_scoped_and_references_cannot_be_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    let (engine, queue) = fixtures(&state);
    let service = MicroServiceResponse {
        id: Uuid::new_v4(),
        spec: spec(queue.id),
    };
    state.store.insert_micro_service(&service).unwrap();
    let vm_id = Uuid::new_v4();
    state
        .store
        .save_service_deployment(&MicroServiceDeployment {
            id: Uuid::new_v4(),
            service_id: service.id,
            spec: service.spec.clone(),
            request: DeployMicroServiceRequest {
                micro_network_id: Some(engine.spec.micro_network_id),
                host_port: 18080,
            },
            phase: ServicePhase::Running,
            digest: None,
            instances: vec![ServiceInstance {
                vm_id,
                ipv4: "10.0.0.3".into(),
                host_port: 18080,
                state: VmState::Running,
                healthy: true,
            }],
            log: vec![],
            domain_route_owned: false,
            error: None,
        })
        .unwrap();
    let env = environment_for_vm(&state.store, vm_id).unwrap();
    assert!(env["FIRECRAB_QUEUE_NOTIFICATIONS_URL"].contains(&segment(&queue.consume_password)));
    assert!(!env["FIRECRAB_QUEUE_NOTIFICATIONS_URL"].contains(&segment(&queue.publish_password)));
    assert_eq!(env["FIRECRAB_QUEUE_NOTIFICATIONS_NAME"], "notifications");
    assert!(!serde_json::to_string(&service).unwrap().contains("private"));
    let (status, _, json) = request(
        router(state.clone()),
        "DELETE",
        &format!("/api/queues/{}", queue.id),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json["error"]["code"], "queue_in_use");
    assert!(state.store.queue(queue.id).unwrap().is_some());
    let (status, headers, json) = request(
        router(state.clone()),
        "POST",
        &format!("/api/queues/{}/connection/consume", queue.id),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["cache-control"], "no-store");
    assert!(
        json["url"]
            .as_str()
            .unwrap()
            .contains(&segment(&queue.consume_password))
    );
}
#[tokio::test]
async fn bindings_reject_unknown_queues_reserved_variables_and_cross_network_placement() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    let (engine, queue) = fixtures(&state);
    let id = Uuid::new_v4();
    let service = spec(queue.id);
    validate_bindings(&state, &service, id).await.unwrap();
    let mut invalid = service.clone();
    invalid
        .env
        .insert("FIRECRAB_QUEUE_NOTIFICATIONS_URL".into(), "override".into());
    assert!(validate_bindings(&state, &invalid, id).await.is_err());
    let mut invalid = service.clone();
    invalid.queues.get_mut("NOTIFICATIONS").unwrap().queue_id = Uuid::new_v4();
    assert!(validate_bindings(&state, &invalid, id).await.is_err());
    let mut request = DeployMicroServiceRequest {
        micro_network_id: None,
        host_port: 18080,
    };
    placement(&state, &service, &mut request, id).unwrap();
    assert_eq!(request.micro_network_id, Some(engine.spec.micro_network_id));
    request.micro_network_id = Some(Uuid::new_v4());
    assert!(placement(&state, &service, &mut request, id).is_err());
}
#[tokio::test]
async fn ordinary_vm_mutations_cannot_delete_or_stop_the_queue_engine() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    let (engine, _) = fixtures(&state);
    for (method, suffix) in [("DELETE", ""), ("POST", "/stop"), ("POST", "/start")] {
        let (status, _, json) = request(
            router(state.clone()),
            method,
            &format!("/api/vms/{}{suffix}", engine.vm_id),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(json["error"]["code"], "queue_owned");
    }
    assert!(state.store.load_all().unwrap().contains_key(&engine.vm_id));
}
#[tokio::test]
async fn restart_preserves_engine_and_queue_credentials_for_idempotent_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    let (mut engine, mut queue) = fixtures(&state);
    engine.phase = QueueEnginePhase::Installing;
    engine.vm_created = false; // API stopped after VM insertion, before its checkpoint.
    queue.phase = QueuePhase::Provisioning;
    state.store.save_queue_engine(&engine).unwrap();
    state.store.save_queue(&queue).unwrap();
    recover(&state).await;
    let saved = state.store.queue_engine().unwrap().unwrap();
    assert_eq!(saved.vm_id, engine.vm_id);
    assert_eq!(saved.password, engine.password);
    assert!(saved.vm_created);
    assert_eq!(saved.phase, QueueEnginePhase::Failed);
    let saved = state.store.queue(queue.id).unwrap().unwrap();
    assert_eq!(saved.consume_password, queue.consume_password);
    assert_eq!(saved.phase, QueuePhase::Failed);
}
async fn wait_for_engine_failure(state: &AppState) -> EngineRecord {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let engine = state.store.queue_engine().unwrap().unwrap();
            if engine.phase == QueueEnginePhase::Failed {
                break engine;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn retry_checkpoints_existing_vm_before_boot_and_never_replaces_it_if_missing() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    let (mut engine, _) = fixtures(&state);
    engine.vm_created = false;
    state.store.save_queue_engine(&engine).unwrap();

    // The fixture has no bootable template. Ownership must survive this boot failure.
    start_engine(state.clone(), Uuid::new_v4()).await.unwrap();
    let saved = wait_for_engine_failure(&state).await;
    assert!(saved.vm_created);
    assert_eq!(saved.vm_id, engine.vm_id);
    assert_eq!(saved.password, engine.password);

    state.vms.lock().unwrap().remove(&engine.vm_id);
    state.store.delete(engine.vm_id).unwrap();
    start_engine(state.clone(), Uuid::new_v4()).await.unwrap();
    let saved = wait_for_engine_failure(&state).await;
    assert!(
        saved
            .error
            .unwrap()
            .contains("restore its records and disk")
    );
    assert!(state.vms.lock().unwrap().is_empty());
    assert!(state.templates.resolve_alias(TEMPLATE).is_none());
}

#[tokio::test]
async fn missing_stateful_disk_is_never_rebuilt_from_the_template() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    let (engine, _) = fixtures(&state);
    let vm = {
        let mut vms = state.vms.lock().unwrap();
        let vm = vms.get_mut(&engine.vm_id).unwrap();
        vm.disk_generation = Some(Uuid::new_v4());
        vm.clone()
    };
    state.store.update(&vm).unwrap();
    start_engine(state.clone(), Uuid::new_v4()).await.unwrap();
    let saved = wait_for_engine_failure(&state).await;
    assert!(saved.error.unwrap().contains("Queue disk is missing"));
    assert_eq!(saved.vm_id, engine.vm_id);
    assert_eq!(saved.password, engine.password);
    assert_eq!(
        state.vms.lock().unwrap()[&engine.vm_id].disk_generation,
        vm.disk_generation
    );
}

#[tokio::test]
async fn deletion_of_inflight_work_never_sends_a_destructive_request() {
    use std::sync::{Arc, Mutex};
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    let (engine, queue) = fixtures(&state);
    let methods = Arc::new(Mutex::new(Vec::new()));
    let seen = methods.clone();
    let app = Router::new().fallback(move |request: Request<Body>| {
        let seen = seen.clone();
        async move {
            seen.lock().unwrap().push(request.method().clone());
            axum::Json(
                json!({"messages_ready":0,"messages_unacknowledged":1,"consumers":0,"messages":0}),
            )
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = RabbitClient::at(format!("http://{addr}"), engine).unwrap();
    assert!(client.delete(&queue, Uuid::new_v4()).await.is_err());
    assert!(
        methods
            .lock()
            .unwrap()
            .iter()
            .all(|m| m == reqwest::Method::GET)
    );
    server.abort();
}

/// Run only against a disposable broker. This creates and removes a random vhost.
#[tokio::test]
#[ignore = "requires a disposable RabbitMQ broker and npm ci --prefix examples/queue"]
async fn rabbitmq_live_disconnects_keep_confirmed_and_unacknowledged_messages() {
    let http = std::env::var("FIRECRAB_TEST_RABBITMQ_HTTP")
        .expect("set the disposable RabbitMQ HTTP origin");
    let amqp = std::env::var("FIRECRAB_TEST_RABBITMQ_AMQP")
        .expect("set the disposable RabbitMQ host:port");
    let username =
        std::env::var("FIRECRAB_TEST_RABBITMQ_USER").expect("set the disposable admin user");
    let password = std::env::var("FIRECRAB_TEST_RABBITMQ_PASSWORD")
        .expect("set the disposable admin password");
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    let (mut engine, mut queue) = fixtures(&state);
    engine.username = username;
    engine.password = password;
    queue.delivery_limit = 2;
    let client = RabbitClient::at(http, engine).unwrap();
    let id = Uuid::new_v4();
    client.declare(&queue, id).await.unwrap();
    let url = |access| {
        format!(
            "amqp://{}:{}@{}/{}?heartbeat=5",
            segment(&queue.username(access)),
            segment(queue.password(access)),
            amqp,
            segment(&queue.vhost())
        )
    };
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../examples/queue/test-disconnect.mjs");
    let child = tokio::process::Command::new("node")
        .arg(script)
        .env("FIRECRAB_TEST_AMQP_PUBLISH_URL", url(QueueAccess::Publish))
        .env("FIRECRAB_TEST_AMQP_CONSUME_URL", url(QueueAccess::Consume))
        .env("FIRECRAB_TEST_QUEUE", &queue.name)
        .kill_on_drop(true)
        .output();
    let result = tokio::time::timeout(std::time::Duration::from_secs(60), child)
        .await
        .unwrap()
        .unwrap();
    let metrics = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let metrics = client.metrics(&queue, id).await.unwrap();
            if metrics.failed == 1 {
                break metrics;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    })
    .await;
    // This vhost is test-owned and random. Production deletion refuses pending work.
    client
        .request(
            reqwest::Method::DELETE,
            &format!("vhosts/{}", segment(&queue.vhost())),
            None,
            id,
        )
        .await
        .unwrap();
    for access in [QueueAccess::Publish, QueueAccess::Consume] {
        client
            .request(
                reqwest::Method::DELETE,
                &format!("users/{}", segment(&queue.username(access))),
                None,
                id,
            )
            .await
            .unwrap();
    }
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    println!("{}", String::from_utf8_lossy(&result.stdout));
    let metrics = metrics.expect("repeated failures should be dead-lettered");
    assert_eq!(metrics.ready, 0);
    assert_eq!(metrics.unacknowledged, 0);
    assert_eq!(metrics.failed, 1);
}

//! Actual local API/dashboard evidence with injected uncertainty and a retained fixture disk.
use super::*;

#[tokio::test]
#[ignore = "manual isolated broker screenshot fixture; no guest, image download or RabbitMQ execution"]
async fn iac_managed_database_broker_browser_fixture() {
    let info = std::path::PathBuf::from(std::env::var_os("FIRECRAB_IAC_QA_INFO").unwrap());
    let assets = std::path::PathBuf::from(std::env::var_os("FIRECRAB_IAC_QA_ASSETS").unwrap());
    assert!(assets.join("index.html").is_file());
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let mut value = cluster();
    let mut vm =
        crate::handlers::vms::test_support::record("Retained broker data", value.brokers[0].id);
    let storage = Uuid::new_v4();
    state
        .store
        .insert_micro_storage(
            storage,
            "Isolated broker evidence",
            root.path().to_str().unwrap(),
        )
        .unwrap();
    vm.storage_root = storage.to_string();
    vm.micro_network_id = network;
    vm.state = firecrab_api_types::VmState::Stopped;
    vm.disk_generation = Some(Uuid::new_v4());
    state.store.insert(&vm).unwrap();
    state.vms.lock().unwrap().insert(vm.id, vm.clone());
    value.brokers[0].disk_generation = vm.disk_generation;
    let disk =
        crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id)
            .rootfs(vm.disk_generation.unwrap());
    assert!(disk.starts_with(root.path()));
    std::fs::create_dir_all(disk.parent().unwrap()).unwrap();
    std::fs::write(
        &disk,
        b"Disposable broker disk fixture: retained by disconnect recovery",
    )
    .unwrap();
    install(&state, &value);
    let operation_id = Uuid::new_v4();
    let intent = ready(
        state
            .store
            .begin_managed_database_broker(operation_id, ManagedBrokerCommand::Disconnect, None)
            .unwrap(),
        true,
    );
    state
        .store
        .managed_database_broker_effects(&intent)
        .unwrap();
    // Inject only the uncertain checkpoint; recovery itself uses the real typed disconnect actor.
    state.store.fail_managed_database_broker(&intent).unwrap();
    let router = build_router(
        state.clone(),
        &HttpConfig {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 128,
            request_timeout: std::time::Duration::from_secs(10),
            static_root: Some(assets),
        },
    )
    .layer(axum::Extension(crate::ai_sandbox::access::LocalOperator));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let finished = std::sync::Arc::new(tokio::sync::Notify::new());
    let shutdown = finished.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { shutdown.notified().await })
            .await
            .unwrap();
    });
    std::fs::write(&info,serde_json::to_vec_pretty(&json!({"fixture":true,"url":url,"operationId":operation_id,"brokerId":vm.id,"diskGeneration":vm.disk_generation,"diskPath":disk,"scope":"injected uncertain disconnect; seeded stopped VM and private TLS; actual local API recovery; plain fixture disk retained; no KVM, guest, image download, RabbitMQ, SQL or Terraform execution"})).unwrap()).unwrap();
    let stop = info.with_extension("stop");
    let started = std::time::Instant::now();
    while !stop.exists() {
        assert!(started.elapsed() < std::time::Duration::from_secs(300));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    finished.notify_one();
    server.await.unwrap();
    assert!(disk.is_file());
    assert!(state.store.load_all().unwrap().contains_key(&vm.id));
    std::fs::remove_file(info).unwrap();
    std::fs::remove_file(stop).unwrap();
}

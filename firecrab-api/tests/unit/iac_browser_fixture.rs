use super::*;

#[tokio::test]
#[ignore = "manual local browser evidence; serves isolated fixture records and compiled dashboard"]
async fn iac_owner_browser_fixture() {
    let info = std::path::PathBuf::from(
        std::env::var_os("FIRECRAB_IAC_QA_INFO").expect("set a temporary QA info path"),
    );
    let assets = std::path::PathBuf::from(
        std::env::var_os("FIRECRAB_IAC_QA_ASSETS").expect("set compiled dashboard directory"),
    );
    assert!(assets.join("index.html").is_file());
    let root = tempfile::tempdir().unwrap();
    let (state, _router, id) = fixture(root.path()).await;
    let socket = root.path().join("qa-network.sock");
    crate::network::test_support::spawn_always_ok_helper(&socket);
    let runtime = crate::state::RuntimeConfig {
        vms_dir: root.path().join("private/vms"),
        ..(*state.runtime).clone()
    };
    let state = state
        .with_test_runtime(runtime)
        .with_test_network(crate::network::NetworkClient::with_socket_path(socket));
    std::fs::create_dir_all(root.path().join("private")).unwrap();
    let resource = ManagedResource {
        kind: "network".into(),
        id: id.to_string(),
    };
    let before = state.store.iac_resource_owner(&resource).unwrap();
    let change = ChangeResourceOwner {
        expected_endpoint_id: before.endpoint_id,
        operation_id: Uuid::new_v4(),
        resource: resource.clone(),
        expected_generation: before.generation,
        expected_owner: before.owner,
        owner: Some(firecrab_stack::ResourceOwner {
            manager: firecrab_stack::Manager::Terraform,
            id: Uuid::new_v4(),
            key: "firecrab_network.application".into(),
        }),
    };
    let ownership = state.store.change_iac_resource_owner(&change).unwrap();
    let mut ownerships = vec![ownership.clone()];
    let second_state = Uuid::new_v4();
    let mut volume_proof = serde_json::Value::Null;
    for kind in [
        "storage_root",
        "volume",
        "security_group",
        "shell",
        "schedule",
        "pool",
        "queue",
        "database_connection",
        "storage_connection",
        "secret",
        "secret_connection",
        "sandbox_project",
        "sandbox_access",
        "queue_engine",
    ] {
        let resource = if kind == "volume" {
            let (resource, attachment, proof) =
                seed_attached_volume(&state, root.path(), change.owner.as_ref().unwrap().id).await;
            ownerships.push(attachment);
            volume_proof = proof;
            resource
        } else {
            configuration_resource(&state, root.path(), id, kind)
        };
        let before = state.store.iac_resource_owner(&resource).unwrap();
        let transfer = ChangeResourceOwner {
            expected_endpoint_id: before.endpoint_id,
            operation_id: Uuid::new_v4(),
            resource,
            expected_generation: before.generation,
            expected_owner: before.owner,
            owner: Some(firecrab_stack::ResourceOwner {
                manager: firecrab_stack::Manager::Terraform,
                id: if matches!(kind, "pool" | "schedule" | "shell") {
                    second_state
                } else {
                    change.owner.as_ref().unwrap().id
                },
                key: format!("firecrab_{kind}.application"),
            }),
        };
        ownerships.push(if matches!(kind, "secret" | "secret_connection") {
            state
                .store
                .change_iac_vault_owner(&transfer, &state.secrets)
                .unwrap()
        } else {
            state.store.change_iac_resource_owner(&transfer).unwrap()
        });
    }
    ownerships.sort_by(|a, b| {
        (&a.resource.kind, &a.resource.id).cmp(&(&b.resource.kind, &b.resource.id))
    });
    let recovery_project = configuration_resource(&state, root.path(), id, "sandbox_project");
    let recovery_id = Uuid::parse_str(&recovery_project.id).unwrap();
    let borrowed = root.path().join("borrowed-source");
    std::fs::create_dir(&borrowed).unwrap();
    std::fs::write(borrowed.join("keep.txt"), "borrowed contents").unwrap();
    let mut saved = state.store.sandbox_project(recovery_id).unwrap().unwrap();
    saved.spec.source_path = Some(borrowed.to_string_lossy().into());
    state.store.save_sandbox_project(&saved).unwrap();
    std::fs::create_dir_all(&state.sandbox.root).unwrap();
    let recovery_workspace = state.sandbox.root.join(&recovery_project.id);
    std::fs::write(&recovery_workspace, "fixture cleanup failure").unwrap();
    let source_stack = state.store.register_stack("source-proof").unwrap();
    let source_network = Uuid::new_v4();
    state.store.insert_micro_network(&serde_json::from_value(json!({"id":source_network,"name":"retained-source-network","subnetCidr":"172.31.21.0/24","gateway":"172.31.21.1","internetEnabled":true})).unwrap()).unwrap();
    state
        .store
        .bind_stack_resource(
            &source_stack.name,
            &Binding {
                kind: ResourceKind::Network,
                key: "retained".into(),
                resource_id: source_network,
            },
        )
        .unwrap();
    state.store.configure_stack_source(&source_stack.name,&serde_json::from_value(json!({"expectedGeneration":0,"source":{"repository":"https://github.com/example/infra","ref":"main","manifestPath":"stack.yaml","paused":true}})).unwrap(),1).unwrap();
    let checked = state
        .store
        .begin_stack_check(
            &source_stack.name,
            1,
            1,
            crate::persistence::SourceTrigger::Check,
        )
        .unwrap()
        .unwrap()
        .0;
    let mut check = checked.latest.unwrap();
    check.state = firecrab_stack::GitCheckState::Failed;
    check.finished_at = Some(2);
    check.error = Some("fixture_no_network".into());
    state
        .store
        .finish_stack_check(source_stack.id, &check, 2)
        .unwrap();
    let vm_proof = seed_vm_cleanup(&state, root.path());
    // Deliberately nonresponsive TCP fixture: no PostgreSQL/MySQL server or SQL runs.
    let database_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let database_input: crate::pipe_databases::DatabaseInput = serde_json::from_value(json!({
        "name":"Native database recovery fixture", "provider":"local", "tlsMode":"disable", "writeEnabled":true,
        "uri":format!("postgresql://fixture:private-fixture-credential@{}/iac_fixture",database_listener.local_addr().unwrap())
    })).unwrap();
    let database_id = Uuid::new_v4();
    state
        .store
        .save_pipe_database(database_id, &database_input)
        .unwrap();
    let database_operation_id = Uuid::new_v4();
    let crate::persistence::iac_database_operations::Admission::Ready(database_intent, true) =
        state
            .store
            .begin_database_change(
                database_operation_id,
                crate::persistence::iac_database_operations::DatabaseChange::Update(
                    database_id,
                    database_input,
                ),
            )
            .unwrap()
    else {
        panic!("database fixture must admit its own native operation")
    };
    // Inject the uncertain-after-checkpoint state. Never label it as live SQL evidence.
    state
        .store
        .begin_database_connection_effects(&database_intent)
        .unwrap();
    state
        .store
        .fail_database_connection(&database_intent)
        .unwrap();
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
    std::fs::write(&info,serde_json::to_vec_pretty(&json!({"url":url,"ownership":ownership,"ownerships":ownerships,"operationId":change.operation_id,"fixture":true,
        "nativeRecoveryProjectId":recovery_id,"nativeRecoveryWorkspace":recovery_workspace,"nativeRecoveryBorrowedFile":borrowed.join("keep.txt"),
        "nativeSourceStackId":source_stack.id,"nativeSourceStackName":source_stack.name,"nativeSourceNetworkId":source_network,"volumeProof":volume_proof,"vmProof":vm_proof,"databaseOperationId":database_operation_id,"databaseId":database_id,"databaseFixtureAddress":database_listener.local_addr().unwrap().to_string(),"databaseFixture":"nonresponsive local TCP endpoint; uncertain SQL checkpoint injected; no PostgreSQL/MySQL server or SQL execution"})).unwrap()).unwrap();
    let stop = info.with_extension("stop");
    let started = std::time::Instant::now();
    while !stop.exists() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(600),
            "browser fixture exceeded its deadline"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    finished.notify_one();
    server.await.unwrap();
    std::fs::remove_file(&info).unwrap();
    std::fs::remove_file(stop).unwrap();
}

#[path = "iac_browser_resources.rs"]
mod resources;
use resources::{seed_attached_volume, seed_vm_cleanup};

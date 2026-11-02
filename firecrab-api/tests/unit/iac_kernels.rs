use super::*;
use crate::{image_install::Architecture, oci::kernel, persistence::iac_kernels::KernelRequest};
use sha2::{Digest, Sha256};
fn installed(state: &AppState, version: &str) -> std::path::PathBuf {
    let mut bytes = vec![0; 4096];
    match Architecture::HOST {
        Architecture::X86_64 => {
            bytes[..4].copy_from_slice(b"\x7fELF");
            bytes[4] = 2;
            bytes[5] = 1;
            bytes[6] = 1;
            bytes[16..18].copy_from_slice(&2_u16.to_le_bytes());
            bytes[18..20].copy_from_slice(&0x3e_u16.to_le_bytes());
        }
        Architecture::Aarch64 => {
            bytes[..2].copy_from_slice(b"MZ");
            bytes[56..60].copy_from_slice(&0x644d_5241_u32.to_le_bytes());
        }
    }
    let path = state
        .templates
        .image_root_path()
        .join(kernel::cache_relative(Architecture::HOST, version).unwrap());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let receipt = kernel::BuiltKernel {
        version: version.into(),
        architecture: Architecture::HOST.as_str().into(),
        source_url: kernel::source_url(version).unwrap(),
        source_sha256: "a".repeat(64),
        image_sha256: format!("{:x}", Sha256::digest(&bytes)),
        build_revision: kernel::BUILD_REVISION.into(),
    };
    std::fs::write(
        path.with_added_extension("json"),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
    path
}
async fn request(app: &Router, method: &str, version: &str, id: Uuid) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!(
                    "/api/kernels/{version}{}",
                    if method == "POST" { "/install" } else { "" }
                ))
                .header("x-firecrab-operation-id", id.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}
async fn finished(state: &AppState, id: Uuid) -> firecrab_stack::NativeOperation {
    tokio::time::timeout(std::time::Duration::from_secs(4), async {
        loop {
            let operation = state.store.iac_native_operation(id).unwrap().unwrap();
            if operation.state != firecrab_stack::NativeOperationState::Running {
                return operation;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn iac_kernel_api_retries_keep_original_receipt_after_artifact_removal() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let version = "7.2.12";
    let path = installed(&state, version);
    let id = Uuid::new_v4();
    assert_eq!(
        request(&app, "POST", version, id).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        finished(&state, id).await.state,
        firecrab_stack::NativeOperationState::Succeeded
    );
    let mut attempts = 0;
    loop {
        let code = request(&app, "DELETE", version, Uuid::new_v4()).await;
        if code == StatusCode::ACCEPTED {
            break;
        }
        attempts += 1;
        assert!(attempts < 10, "{code}");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(request(&app, "POST", version, id).await, StatusCode::OK);
    assert!(!path.exists());
    let (_, receipt) = call(
        &app,
        "GET",
        &format!("/api/kernels/{version}/install"),
        Value::Null,
    )
    .await;
    assert_eq!(receipt["status"], "succeeded");
}
#[tokio::test]
async fn iac_kernel_borrow_blocks_removal_until_the_referencing_image_is_published() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let version = "7.2.12";
    let path = installed(&state, version);
    let lease = crate::kernel_lifecycle::borrow_existing(&state.templates, version)
        .await
        .unwrap();
    let code = request(&app, "DELETE", version, Uuid::new_v4()).await;
    assert!(!code.is_success());
    assert!(path.exists());
    drop(lease);
    let id = Uuid::new_v4();
    assert_eq!(
        request(&app, "DELETE", version, id).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        finished(&state, id).await.state,
        firecrab_stack::NativeOperationState::Succeeded
    );
    assert!(!path.exists());
}
#[tokio::test]
async fn iac_kernel_delete_receipt_failure_keeps_original_scope_for_recovery() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let version = "7.2.12";
    let path = installed(&state, version);
    let id = Uuid::new_v4();
    let inject = rusqlite::Connection::open(root.path().join("state.db")).unwrap();
    inject.execute_batch("CREATE TRIGGER reject_kernel_receipt BEFORE UPDATE ON iac_native_operations WHEN NEW.state='succeeded' AND NEW.kind='kernel' BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;").unwrap();
    assert_eq!(
        request(&app, "DELETE", version, id).await,
        StatusCode::ACCEPTED
    );
    let unknown = finished(&state, id).await;
    assert_eq!(
        unknown.state,
        firecrab_stack::NativeOperationState::NeedsRecovery
    );
    assert!(!path.exists());
    inject
        .execute_batch("DROP TRIGGER reject_kernel_receipt")
        .unwrap();
    let completed = crate::handlers::kernel_mutations::recover(
        state.clone(),
        crate::server::RequestId(Uuid::new_v4()),
        id,
        0,
    )
    .await
    .unwrap();
    assert_eq!(
        completed.state,
        firecrab_stack::NativeOperationState::Succeeded
    );
    assert_eq!(completed.execution_generation, 1);
    assert_eq!(
        request(&app, "DELETE", version, id).await,
        StatusCode::NO_CONTENT
    );
}
#[tokio::test]
async fn iac_kernel_independent_api_database_cannot_replace_a_claimed_physical_namespace() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let version = "7.2.12";
    let path = installed(&state, version);
    let id = Uuid::new_v4();
    assert_eq!(
        request(&app, "POST", version, id).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        finished(&state, id).await.state,
        firecrab_stack::NativeOperationState::Succeeded
    );
    let templates =
        crate::templates::TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap();
    let other = AppState::with_db_file(templates, root.path().join("other.db"))
        .await
        .unwrap();
    let definition = crate::kernel_lifecycle::definition(&other.templates, version.into(), false)
        .await
        .unwrap();
    let (intent, _) = other
        .store
        .begin_kernel(
            Uuid::new_v4(),
            KernelRequest::Delete(version.into()),
            definition,
        )
        .unwrap();
    let guard =
        crate::kernel_lifecycle::runtime_lock(other.templates.image_root_path(), version).unwrap();
    assert!(
        crate::kernel_lifecycle::authorize_namespace(&guard, &other.templates, &intent).is_err()
    );
    assert!(path.exists());
}

#[tokio::test]
async fn iac_kernel_transfer_preserves_bytes_blocks_direct_writers_and_releases_missing_metadata() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let version = "7.2.12";
    let artifact = installed(&state, version);
    let bytes = std::fs::read(&artifact).unwrap();
    let target = crate::persistence::iac_kernels::resource(Architecture::HOST.as_str(), version);
    let owner_path = format!("/api/iac/owners/kernel/{}", target.id);
    let (_, before) = call(&app, "GET", &owner_path, Value::Null).await;
    let change = json!({"operationId":Uuid::new_v4(),"expectedEndpointId":before["endpointId"],
        "resource":target,"expectedGeneration":before["generation"],"expectedOwner":before["owner"],
        "owner":{"manager":"terraform","id":Uuid::new_v4(),"key":"firecrab_kernel.host"}});
    let (code, owned) = call(&app, "POST", "/api/iac/owners/transition", change.clone()).await;
    assert_eq!(code, StatusCode::OK, "{owned}");
    assert_eq!(std::fs::read(&artifact).unwrap(), bytes);
    assert_eq!(
        request(&app, "POST", version, Uuid::new_v4()).await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(&app, "DELETE", version, Uuid::new_v4()).await,
        StatusCode::CONFLICT
    );
    // Existing verified bytes may still be borrowed by an image without granting write authority.
    let lease = crate::kernel_lifecycle::borrow_existing(&state.templates, version)
        .await
        .unwrap();
    drop(lease);
    std::fs::remove_file(&artifact).unwrap();
    assert_eq!(
        call(&app, "POST", "/api/iac/owners/transition", change)
            .await
            .1,
        owned
    );
    let release = json!({"operationId":Uuid::new_v4(),"expectedEndpointId":owned["endpointId"],
        "resource":target,"expectedGeneration":owned["generation"],"expectedOwner":owned["owner"],"owner":null});
    let (code, released) = call(&app, "POST", "/api/iac/owners/transition", release).await;
    assert_eq!(code, StatusCode::OK, "{released}");
    assert!(released["owner"].is_null());
    assert!(!artifact.exists());
}

#[tokio::test]
async fn iac_kernel_transfer_rejects_replaced_artifact_and_unfinished_native_effects() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let version = "7.2.12";
    let artifact = installed(&state, version);
    let target = crate::persistence::iac_kernels::resource(Architecture::HOST.as_str(), version);
    let before = state.store.iac_resource_owner(&target).unwrap();
    let change: firecrab_stack::ChangeResourceOwner = serde_json::from_value(json!({
        "operationId":Uuid::new_v4(),"expectedEndpointId":before.endpoint_id,"resource":target,
        "expectedGeneration":before.generation,"expectedOwner":null,
        "owner":{"manager":"terraform","id":Uuid::new_v4(),"key":"firecrab_kernel.host"}}))
    .unwrap();
    let presence = crate::kernel_lifecycle::prepare_transfer(&state.templates, &target)
        .await
        .unwrap();
    let replacement = artifact.with_added_extension("replacement");
    std::fs::write(&replacement, std::fs::read(&artifact).unwrap()).unwrap();
    std::fs::rename(&replacement, &artifact).unwrap();
    assert!(matches!(
        state.store.change_iac_kernel_owner(&change, &presence),
        Err(crate::persistence::PersistenceError::StaleIacOwner)
    ));
    drop(presence);
    let definition = crate::kernel_lifecycle::definition(&state.templates, version.into(), false)
        .await
        .unwrap();
    let (intent, _) = state
        .store
        .begin_kernel(
            Uuid::new_v4(),
            KernelRequest::Delete(version.into()),
            definition,
        )
        .unwrap();
    state.store.kernel_effects(&intent).unwrap();
    state.store.fail_kernel(&intent).unwrap();
    let (code, _) = call(
        &app,
        "POST",
        "/api/iac/owners/transition",
        serde_json::to_value(&change).unwrap(),
    )
    .await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(state.store.iac_resource_owner(&target).unwrap(), before);
    assert!(artifact.exists());
}

#[tokio::test]
#[ignore = "manual isolated API/React kernel evidence; no kernel build or guest boot"]
async fn iac_kernel_browser_fixture() {
    use crate::persistence::iac_kernels::KernelRequest;
    let info = std::path::PathBuf::from(std::env::var_os("FIRECRAB_IAC_QA_INFO").unwrap());
    let assets = std::path::PathBuf::from(std::env::var_os("FIRECRAB_IAC_QA_ASSETS").unwrap());
    assert!(assets.join("index.html").is_file());
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let retained = installed(&state, "7.2.12");
    let owned = crate::persistence::iac_kernels::resource(Architecture::HOST.as_str(), "7.2.12");
    let before = state.store.iac_resource_owner(&owned).unwrap();
    let presence = crate::kernel_lifecycle::prepare_transfer(&state.templates, &owned)
        .await
        .unwrap();
    let change = serde_json::from_value(
        json!({"operationId":Uuid::new_v4(),"expectedEndpointId":before.endpoint_id,
        "resource":owned,"expectedGeneration":before.generation,"expectedOwner":null,
        "owner":{"manager":"terraform","id":Uuid::new_v4(),"key":"firecrab_kernel.retained"}}),
    )
    .unwrap();
    let ownership = state
        .store
        .change_iac_kernel_owner(&change, &presence)
        .unwrap();
    drop(presence);
    let removable = installed(&state, "7.2.13");
    let definition = crate::kernel_lifecycle::definition(&state.templates, "7.2.13".into(), false)
        .await
        .unwrap();
    let id = Uuid::new_v4();
    let (intent, _) = state
        .store
        .begin_kernel(id, KernelRequest::Delete("7.2.13".into()), definition)
        .unwrap();
    state.store.kernel_effects(&intent).unwrap();
    state.store.fail_kernel(&intent).unwrap();
    let router = build_router(
        state,
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
    std::fs::write(&info,serde_json::to_vec_pretty(&json!({
        "fixture":true,"url":url,"operationId":id,"retainedPath":retained,"removedPath":removable,
        "ownership":ownership,"scope":"Actual local Axum/SQLite/production React and architecture-valid tiny kernel fixtures. Uncertain deletion checkpoint is injected. Recovery deletes only the original unused fixture artifact and retains the separate Terraform-owned kernel. No Linux kernel compilation, guest boot, remote registry, Stack apply or Terraform provider runs."
    })).unwrap()).unwrap();
    let stop = info.with_extension("stop");
    let started = std::time::Instant::now();
    while !stop.exists() {
        assert!(started.elapsed() < std::time::Duration::from_secs(600));
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    finished.notify_one();
    server.await.unwrap();
    std::fs::remove_file(info).unwrap();
    std::fs::remove_file(stop).unwrap();
}

#[tokio::test]
async fn iac_kernel_removal_preserves_historical_image_dependencies_after_alias_replacement() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let retained = installed(&state, "7.2.12");
    installed(&state, "7.2.13");
    std::fs::write(root.path().join("rootfs"), b"retained fixture rootfs").unwrap();
    let first = crate::templates::TemplateSpec {
        alias: "retained-versions".into(),
        version: "v1".into(),
        kernel: kernel::cache_relative(Architecture::HOST, "7.2.12").unwrap(),
        rootfs: "rootfs".into(),
        initrd: None,
        boot_args: String::new(),
    };
    state.templates.register_spec(first.clone()).unwrap();
    let mut second = first;
    second.version = "v2".into();
    second.kernel = kernel::cache_relative(Architecture::HOST, "7.2.13").unwrap();
    state.templates.replace_alias(second).unwrap();
    assert_eq!(
        state
            .templates
            .resolve_alias("retained-versions")
            .unwrap()
            .version,
        "v2"
    );
    assert_eq!(
        request(&app, "DELETE", "7.2.12", Uuid::new_v4()).await,
        StatusCode::CONFLICT
    );
    assert!(retained.exists());
    let previous = state
        .templates
        .resolve_version("retained-versions", "v1")
        .unwrap();
    state.templates.open_verified(&previous.kernel).unwrap();
}

#[tokio::test]
async fn iac_kernel_removal_does_not_treat_a_corrupt_image_catalog_as_unreferenced() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let path = installed(&state, "7.2.12");
    std::fs::write(
        root.path().join(".templates.json"),
        b"corrupt image catalog",
    )
    .unwrap();
    assert_eq!(
        request(&app, "DELETE", "7.2.12", Uuid::new_v4()).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(path.exists());
}

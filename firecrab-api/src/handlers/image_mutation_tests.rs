use super::*;
use crate::persistence::iac_images::ImageVersion;
use firecrab_stack::{ChangeResourceOwner, Manager, ResourceOwner};

async fn fixture() -> (tempfile::TempDir, AppState, TemplateSpec) {
    let directory = tempfile::tempdir().unwrap();
    let images = directory.path().join("images");
    std::fs::create_dir_all(images.join("kernel")).unwrap();
    std::fs::create_dir(images.join("rootfs")).unwrap();
    std::fs::write(images.join("kernel/demo"), b"fixture kernel").unwrap();
    std::fs::write(images.join("rootfs/demo"), b"fixture rootfs").unwrap();
    let spec = TemplateSpec {
        alias: "demo".into(),
        version: "v1".into(),
        kernel: "kernel/demo".into(),
        rootfs: "rootfs/demo".into(),
        initrd: None,
        boot_args: "console=ttyS0".into(),
    };
    let templates = TemplateRegistry::from_specs(&images, [spec.clone()]).unwrap();
    let state = AppState::with_db_file(templates, directory.path().join("state.db"))
        .await
        .unwrap();
    (directory, state, spec)
}
fn headers(id: Uuid) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("x-firecrab-operation-id", id.to_string().parse().unwrap());
    headers
}
fn change(state: &AppState, generation: u64, owner: Option<ResourceOwner>) -> ChangeResourceOwner {
    ChangeResourceOwner {
        resource: crate::persistence::iac_images::resource("demo"),
        owner,
        expected_endpoint_id: state.store.iac_endpoint_id().unwrap(),
        expected_generation: generation,
        expected_owner: state
            .store
            .iac_resource_owner(&crate::persistence::iac_images::resource("demo"))
            .unwrap()
            .owner,
        operation_id: Uuid::new_v4(),
    }
}
fn transfer(state: &AppState, change: &ChangeResourceOwner) {
    let presence = image_lifecycle::prepare_transfer(
        &state.templates,
        &change.resource,
        change.owner.is_some(),
    )
    .unwrap();
    state
        .store
        .change_iac_image_owner(change, &presence)
        .unwrap();
}
#[tokio::test]
async fn iac_image_delete_retries_original_receipt_after_files_are_gone() {
    let (_directory, state, _) = fixture().await;
    let id = Uuid::new_v4();
    for _ in 0..2 {
        let response = delete(
            State(state.clone()),
            Path("demo".into()),
            Extension(RequestId(Uuid::new_v4())),
            headers(id),
            Query(DeleteImageQuery::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response.headers()["x-firecrab-operation-id"],
            id.to_string()
        );
    }
    assert!(
        !state
            .templates
            .image_root_path()
            .join("rootfs/demo")
            .exists()
    );
    assert!(state.templates.resolve_alias("demo").is_none());
    assert!(state.store.image_receipt(id).unwrap().is_some());
    let response = delete(
        State(state),
        Path("demo".into()),
        Extension(RequestId(Uuid::new_v4())),
        headers(Uuid::new_v4()),
        Query(DeleteImageQuery::default()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
}
#[tokio::test]
async fn iac_image_old_producer_cannot_publish_after_claim_and_release() {
    let (_directory, state, mut spec) = fixture().await;
    let authority = image_lifecycle::capture(&state.templates, "demo").unwrap();
    let old_worker = (*state.templates)
        .clone()
        .with_image_authority(authority)
        .unwrap();
    transfer(
        &state,
        &change(
            &state,
            0,
            Some(ResourceOwner {
                manager: Manager::Terraform,
                id: Uuid::new_v4(),
                key: "firecrab_image.demo".into(),
            }),
        ),
    );
    transfer(&state, &change(&state, 1, None));
    spec.version = "v2".into();
    assert!(old_worker.register_spec(spec.clone()).is_err());
    assert_eq!(state.templates.resolve_alias("demo").unwrap().version, "v1");
    state.templates.register_spec(spec).unwrap();
}
#[tokio::test]
async fn iac_image_original_install_receipt_survives_newer_alias_and_ownership() {
    let (_directory, state, spec) = fixture().await;
    let version = state.templates.resolve_alias("demo").unwrap();
    let definition = ImageDefinition {
        alias: "demo".into(),
        architecture: Architecture::HOST.as_str().into(),
        root: state.templates.physical_root().unwrap(),
        before: None,
        source: ImageSource::Package {
            spec: spec.clone(),
            archive_sha256: "a".repeat(64),
        },
        dependencies: vec![],
    };
    let id = Uuid::new_v4();
    let (intent, _) = state
        .store
        .begin_image(
            id,
            ImageRequest::Install {
                alias: "demo".into(),
            },
            definition,
        )
        .unwrap();
    state
        .store
        .prepare_image(
            &intent,
            &ImageVersion {
                spec,
                pin: version.stack_pin(),
            },
        )
        .unwrap();
    state.store.image_effects(&intent).unwrap();
    state
        .store
        .finish_image(
            &intent,
            &ImageOutcome {
                alias: "demo".into(),
                installed: true,
                image: Some(images::installed_response_for_state(&state, &version)),
            },
        )
        .unwrap();
    transfer(
        &state,
        &change(
            &state,
            0,
            Some(ResourceOwner {
                manager: Manager::Terraform,
                id: Uuid::new_v4(),
                key: "firecrab_image.demo".into(),
            }),
        ),
    );
    let response = install(
        State(state),
        Path("demo".into()),
        Extension(RequestId(Uuid::new_v4())),
        headers(id),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}
#[tokio::test]
async fn iac_image_claim_rejects_changed_bytes_and_retains_native_generation() {
    let (_directory, state, _) = fixture().await;
    let request = change(
        &state,
        0,
        Some(ResourceOwner {
            manager: Manager::Terraform,
            id: Uuid::new_v4(),
            key: "firecrab_image.demo".into(),
        }),
    );
    let presence =
        image_lifecycle::prepare_transfer(&state.templates, &request.resource, true).unwrap();
    std::fs::write(
        state.templates.image_root_path().join("rootfs/demo"),
        b"different rootfs",
    )
    .unwrap();
    assert!(
        state
            .store
            .change_iac_image_owner(&request, &presence)
            .is_err()
    );
    assert_eq!(
        state
            .store
            .iac_resource_owner(&request.resource)
            .unwrap()
            .generation,
        0
    );
}
#[tokio::test]
async fn iac_image_stale_registry_cannot_publish_into_a_replaced_root() {
    let (directory, state, mut spec) = fixture().await;
    let original = state.templates.image_root_path().to_owned();
    std::fs::rename(&original, directory.path().join("detached-images")).unwrap();
    std::fs::create_dir(&original).unwrap();
    spec.version = "v2".into();
    assert!(state.templates.register_spec(spec).is_err());
    assert!(std::fs::read_dir(&original).unwrap().next().is_none());
}

#[tokio::test]
async fn iac_image_recovery_reuses_the_original_completed_oci_rootfs_without_network() {
    let (_directory, state, _) = fixture().await;
    let original = "docker.io/library/recovery-demo:v1";
    let parsed = crate::oci::ImageReference::parse(original).unwrap();
    let alias = crate::oci::template_name_from_reference(&parsed)
        .unwrap()
        .alias;
    crate::oci::kernel_tests::install_fixture_kernel(
        state.templates.image_root_path(),
        Architecture::HOST,
        "7.2.2",
    )
    .await;
    let id = Uuid::new_v4();
    let definition = ImageDefinition {
        alias: alias.clone(),
        architecture: Architecture::HOST.as_str().into(),
        root: state.templates.physical_root().unwrap(),
        before: None,
        source: ImageSource::Oci {
            reference: format!("docker.io/library/recovery-demo@sha256:{}", "a".repeat(64)),
            naming_reference: original.into(),
            display_version: "v1".into(),
            kernel_version: "7.2.2".into(),
            kernel_override: None,
            credential: None,
            authentication: None,
        },
        dependencies: vec![],
    };
    let (intent, _) = state
        .store
        .begin_image(
            id,
            ImageRequest::Import {
                reference: original.into(),
            },
            definition,
        )
        .unwrap();
    let spec = crate::oci::retained_import_spec(&intent).unwrap();
    let sha = crate::oci::kernel::inspect_kernel(
        &state.templates.image_root_path().join(&spec.kernel),
        Architecture::HOST,
    )
    .await
    .unwrap();
    state
        .store
        .prepare_image_kernel(
            &intent,
            &crate::persistence::iac_images::ImageKernelPin {
                relative: spec.kernel.clone(),
                sha256: sha.encoded().into(),
                boot_args: spec.boot_args.clone(),
            },
        )
        .unwrap();
    let rootfs = state.templates.image_root_path().join(&spec.rootfs);
    std::fs::create_dir_all(rootfs.parent().unwrap()).unwrap();
    std::fs::write(&rootfs, b"original nondeterministic rootfs").unwrap();
    state.store.image_effects(&intent).unwrap();
    state.store.interrupt_native_iac_operations().unwrap();
    let (resumed, _) = state.store.recover_image_at(id, 0).unwrap();
    let guard = image_lifecycle::lock(&state.templates).unwrap();
    execute(&state, resumed.clone(), guard).await.unwrap();
    assert_eq!(
        std::fs::read(&rootfs).unwrap(),
        b"original nondeterministic rootfs"
    );
    assert!(state.store.image_receipt(id).unwrap().unwrap().installed);
    let prepared = state
        .store
        .image_receipt(id)
        .unwrap()
        .unwrap()
        .image
        .unwrap();
    assert_eq!(prepared.version, "v1");
    assert_eq!(prepared.alias, alias);
    assert!(
        state
            .store
            .finish_image(
                &intent,
                &ImageOutcome {
                    alias,
                    installed: false,
                    image: None
                }
            )
            .is_err()
    );
}

#[tokio::test]
async fn iac_image_pending_delete_excludes_vm_insertion_and_releases_after_no_effect_failure() {
    let (_directory, state, spec) = fixture().await;
    let before = state.templates.observed_images("demo").unwrap();
    let definition = ImageDefinition {
        alias: "demo".into(),
        architecture: Architecture::HOST.as_str().into(),
        root: state.templates.physical_root().unwrap(),
        before: before.last().cloned(),
        source: ImageSource::Delete {
            versions: before,
            artifacts: vec![],
            retain_artifacts: true,
        },
        dependencies: vec![],
    };
    let (intent, _) = state
        .store
        .begin_image(
            Uuid::new_v4(),
            ImageRequest::Delete {
                alias: spec.alias,
                retain_artifacts: true,
            },
            definition,
        )
        .unwrap();
    let mut vm: crate::model::VmRecord = serde_json::from_value(serde_json::json!({
        "id":Uuid::new_v4(),"name":"racing-vm","state":crate::model::VmState::Stopped,
        "template":"demo","cpu":1,"ram":128,"micro_network_id":Uuid::new_v4()
    }))
    .unwrap();
    vm.template = "demo".into();
    assert!(matches!(
        state.store.insert(&vm),
        Err(crate::persistence::PersistenceError::IacResourceBusy)
    ));
    state.store.fail_image(&intent).unwrap();
    state.store.insert(&vm).unwrap();
}

#[tokio::test]
async fn iac_image_sql_creation_fence_remains_after_a_completed_delete() {
    let (_directory, state, _) = fixture().await;
    let response = delete(
        State(state.clone()),
        Path("demo".into()),
        Extension(RequestId(Uuid::new_v4())),
        headers(Uuid::new_v4()),
        Query(DeleteImageQuery::default()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let vm: crate::model::VmRecord = serde_json::from_value(serde_json::json!({
        "id":Uuid::new_v4(),"name":"late-vm","state":crate::model::VmState::Stopped,
        "template":"demo","cpu":1,"ram":128,"micro_network_id":Uuid::new_v4()
    }))
    .unwrap();
    assert!(state.store.insert(&vm).is_err());
}

#[tokio::test]
async fn iac_image_kernel_update_can_read_its_reserved_kernel_and_replay_the_original_response() {
    let (_directory, state, _) = fixture().await;
    crate::oci::kernel_tests::install_fixture_kernel(
        state.templates.image_root_path(),
        Architecture::HOST,
        "7.2.2",
    )
    .await;
    let original_rootfs =
        std::fs::read(state.templates.image_root_path().join("rootfs/demo")).unwrap();
    let id = Uuid::new_v4();
    for _ in 0..2 {
        let response = kernel(
            State(state.clone()),
            Path("demo".into()),
            Extension(RequestId(Uuid::new_v4())),
            headers(id),
            ValidatedJson(UpdateImageKernelRequest {
                kernel_version: "7.2.2".into(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let receipt: firecrab_api_types::ImageResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(receipt.kernel_version.as_deref(), Some("7.2.2"));
        assert_eq!(receipt.version, "v1-kernel-7.2.2");
    }
    assert_eq!(
        std::fs::read(state.templates.image_root_path().join("rootfs/demo")).unwrap(),
        original_rootfs
    );
    assert!(state.templates.resolve_version("demo", "v1").is_some());
}

#[tokio::test]
async fn iac_image_retention_is_part_of_the_original_delete_request() {
    let (_directory, state, spec) = fixture().await;
    let old = (*state.templates)
        .clone()
        .with_image_authority(image_lifecycle::capture(&state.templates, "demo").unwrap())
        .unwrap();
    let id = Uuid::new_v4();
    let rootfs = state.templates.image_root_path().join("rootfs/demo");
    let before = std::fs::read(&rootfs).unwrap();
    let response = delete(
        State(state.clone()),
        Path("demo".into()),
        Extension(RequestId(Uuid::new_v4())),
        headers(id),
        Query(DeleteImageQuery {
            retain_artifacts: true,
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(std::fs::read(&rootfs).unwrap(), before);
    assert!(state.templates.resolve_alias("demo").is_none());
    assert!(
        old.register_spec(spec).is_err(),
        "native removal fences an older producer without an owner change"
    );
    let response = delete(
        State(state.clone()),
        Path("demo".into()),
        Extension(RequestId(Uuid::new_v4())),
        headers(id),
        Query(DeleteImageQuery {
            retain_artifacts: false,
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(std::fs::read(&rootfs).unwrap(), before);
}

#[tokio::test]
async fn iac_image_unready_version_cannot_replace_its_recorded_content_after_restart() {
    let (_directory, state, spec) = fixture().await;
    state.templates.register_spec(spec.clone()).unwrap();
    let root = state.templates.image_root_path();
    std::fs::remove_file(root.join(&spec.rootfs)).unwrap();
    let restored = TemplateRegistry::load_from(root)
        .unwrap()
        .with_iac_store(&state.store);
    assert!(restored.resolve_alias("demo").is_none());
    std::fs::write(root.join(&spec.rootfs), b"different rebuilt bytes").unwrap();
    assert!(matches!(
        restored.register_spec(spec),
        Err(crate::templates::TemplateError::ImmutableVersion(_, _))
    ));
    assert!(restored.resolve_alias("demo").is_none());
}

#[tokio::test]
async fn iac_image_partial_legacy_registration_removes_only_its_captured_remaining_files() {
    let (directory, state, spec) = fixture().await;
    let root = state.templates.image_root_path();
    std::fs::write(
        root.join(".templates.json"),
        serde_json::to_vec(&vec![spec.clone()]).unwrap(),
    )
    .unwrap();
    std::fs::remove_file(root.join(&spec.rootfs)).unwrap();
    let restored = TemplateRegistry::load_from(root).unwrap();
    let restarted = AppState::with_db_file(restored, directory.path().join("state.db"))
        .await
        .unwrap();
    assert!(restarted.templates.resolve_alias("demo").is_none());
    let response = delete(
        State(restarted.clone()),
        Path("demo".into()),
        Extension(RequestId(Uuid::new_v4())),
        headers(Uuid::new_v4()),
        Query(DeleteImageQuery::default()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(!root.join(&spec.kernel).exists());
    assert!(directory.path().join("state.db").exists());
}

#[tokio::test]
async fn iac_image_parent_control_follows_the_actual_deploy_file_not_an_alias_collision() {
    let (_directory, state, mut cached) = fixture().await;
    cached.version = "cached-v2".into();
    cached.rootfs = std::path::Path::new("rootfs/deploy").join(format!("{}.ext4", Uuid::new_v4()));
    std::fs::create_dir_all(state.templates.image_root_path().join("rootfs/deploy")).unwrap();
    std::fs::write(
        state.templates.image_root_path().join(&cached.rootfs),
        b"parent-controlled image",
    )
    .unwrap();
    state.store.remember_deploy_image(&cached).unwrap();
    transfer(
        &state,
        &change(
            &state,
            0,
            Some(ResourceOwner {
                manager: Manager::Terraform,
                id: Uuid::new_v4(),
                key: "firecrab_image.demo".into(),
            }),
        ),
    );
    transfer(&state, &change(&state, 1, None));
    state.templates.register_spec(cached.clone()).unwrap();
    assert!(
        image_lifecycle::prepare_transfer(
            &state.templates,
            &crate::persistence::iac_images::resource("demo"),
            true
        )
        .is_err()
    );
    let response = delete(
        State(state.clone()),
        Path("demo".into()),
        Extension(RequestId(Uuid::new_v4())),
        headers(Uuid::new_v4()),
        Query(DeleteImageQuery::default()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(
        state
            .templates
            .image_root_path()
            .join(&cached.rootfs)
            .exists()
    );
}

#[tokio::test]
#[ignore = "isolated actual API/React image recovery evidence; no guest boot or registry access"]
async fn iac_image_browser_fixture() {
    let info = std::path::PathBuf::from(std::env::var_os("FIRECRAB_IAC_QA_INFO").unwrap());
    let assets = std::path::PathBuf::from(std::env::var_os("FIRECRAB_IAC_QA_ASSETS").unwrap());
    assert!(assets.join("index.html").is_file());
    let (_directory, state, mut removable) = fixture().await;
    let change = change(
        &state,
        0,
        Some(ResourceOwner {
            manager: Manager::Terraform,
            id: Uuid::new_v4(),
            key: "firecrab_image.retained".into(),
        }),
    );
    let presence =
        image_lifecycle::prepare_transfer(&state.templates, &change.resource, true).unwrap();
    let ownership = state
        .store
        .change_iac_image_owner(&change, &presence)
        .unwrap();
    drop(presence);
    removable.alias = "unused-image".into();
    removable.rootfs = "rootfs/unused".into();
    std::fs::write(
        state.templates.image_root_path().join(&removable.rootfs),
        b"original unused fixture image",
    )
    .unwrap();
    let installed = state.templates.register_spec(removable.clone()).unwrap();
    let versions = state.templates.observed_images(&removable.alias).unwrap();
    let definition = ImageDefinition {
        alias: removable.alias.clone(),
        architecture: Architecture::HOST.as_str().into(),
        root: state.templates.physical_root().unwrap(),
        before: versions.last().cloned(),
        source: ImageSource::Delete {
            versions,
            artifacts: vec![
                ImageArtifact {
                    relative: removable.rootfs.clone(),
                    sha256: installed.rootfs.sha256().into(),
                },
                ImageArtifact {
                    relative: removable.kernel.clone(),
                    sha256: installed.kernel.sha256().into(),
                },
            ],
            retain_artifacts: false,
        },
        dependencies: vec![],
    };
    let id = Uuid::new_v4();
    let (intent, _) = state
        .store
        .begin_image(
            id,
            ImageRequest::Delete {
                alias: removable.alias,
                retain_artifacts: false,
            },
            definition,
        )
        .unwrap();
    state.store.image_effects(&intent).unwrap();
    state.store.fail_image(&intent).unwrap();
    let retained = state.templates.image_root_path().join("rootfs/demo");
    let shared_kernel = state.templates.image_root_path().join("kernel/demo");
    let original_kernel = std::fs::read(&shared_kernel).unwrap();
    let removed = state.templates.image_root_path().join(&removable.rootfs);
    let router = crate::server::build_router(
        state,
        &crate::server::HttpConfig {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 128,
            request_timeout: std::time::Duration::from_secs(10),
            static_root: Some(assets),
        },
    )
    .layer(Extension(crate::ai_sandbox::access::LocalOperator));
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
    std::fs::write(&info,serde_json::to_vec_pretty(&serde_json::json!({
        "fixture":true,"url":url,"operationId":id,"ownership":ownership,"retainedPath":retained,"removedPath":removed,
        "scope":"Actual local Axum/SQLite/production React and tiny non-bootable image fixtures. Uncertain deletion is injected. Recovery removes the original unused image and preserves the separate Terraform-owned image and its shared kernel. No Linux/KVM boot, registry access, Stack apply or Terraform provider runs."
    })).unwrap()).unwrap();
    let stop = info.with_extension("stop");
    let started = std::time::Instant::now();
    while !stop.exists() {
        assert!(started.elapsed() < std::time::Duration::from_secs(600));
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    finished.notify_one();
    server.await.unwrap();
    assert_eq!(std::fs::read(shared_kernel).unwrap(), original_kernel);
    std::fs::remove_file(info).unwrap();
    std::fs::remove_file(stop).unwrap();
}

use super::*;
use crate::templates::TemplateRegistry;
use firecrab_api_types::MicroNetworkResponse;

async fn fixture() -> (tempfile::TempDir, AppState) {
    let temp = tempfile::tempdir().unwrap();
    let templates = TemplateRegistry::from_specs(temp.path(), []).unwrap();
    let state = AppState::with_db_file(templates, temp.path().join("workspaces.sqlite"))
        .await
        .unwrap();
    (temp, state)
}
fn input() -> CreateWorkspaceRequest {
    serde_json::from_value(serde_json::json!({"id":Uuid::new_v4()})).unwrap()
}
async fn create_without_worker(
    state: &AppState,
    input: CreateWorkspaceRequest,
) -> Result<WorkspaceSetup, AppError> {
    // Admission and persistence run normally, but no host image/network actions run in this fixture.
    state
        .workspace_setups
        .workers
        .lock()
        .unwrap()
        .insert(input.id);
    create(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        ValidatedJson(input),
    )
    .await
    .map(|(_, Json(job))| job)
}

fn stored_network(
    state: &AppState,
    id: Uuid,
    name: &str,
    cidr: &str,
    internet_enabled: bool,
) -> MicroNetworkResponse {
    let network = MicroNetworkResponse {
        id,
        name: name.into(),
        subnet_cidr: cidr.into(),
        gateway: crate::ipam::SubnetSpec::parse(id, cidr)
            .unwrap()
            .gateway()
            .to_string(),
        internet_enabled,
        dns_mode: Default::default(),
        uplink: None,
        ipv6_cidr: None,
        ipv6_gateway: None,
        ipv6_address_mode: None,
        ipv6_egress: None,
    };
    state.store.insert_micro_network(&network).unwrap();
    network
}

#[tokio::test]
async fn automatic_selection_prefers_the_default_internet_network_and_persists_it() {
    let (_temp, state) = fixture().await;
    stored_network(&state, Uuid::from_u128(1), "custom", "10.31.0.0/24", true);
    let default = stored_network(&state, Uuid::from_u128(2), "default", "10.32.0.0/24", true);
    stored_network(&state, Uuid::from_u128(3), "offline", "10.33.0.0/24", false);
    let input = input();
    create_without_worker(&state, input.clone()).await.unwrap();
    let mut job = read(&state, input.id).await.unwrap();

    assert_eq!(
        network(&state, &mut job, RequestId(input.id))
            .await
            .unwrap(),
        default.id
    );
    assert_eq!(
        read(&state, input.id)
            .await
            .unwrap()
            .progress
            .micro_network_id,
        Some(default.id)
    );
    assert_eq!(state.store.list_micro_networks().unwrap().len(), 3);
    assert!(state.vms.lock().unwrap().is_empty());
}

#[tokio::test]
async fn recovery_reuses_the_reserved_network_and_does_not_switch_when_it_loses_internet() {
    let (temp, state) = fixture().await;
    let input = input();
    let first = create_without_worker(&state, input.clone()).await.unwrap();
    let mut job = read(&state, input.id).await.unwrap();
    let reserved = stored_network(
        &state,
        job.reserved_network,
        "workspace-reserved",
        "10.34.0.0/24",
        true,
    );
    stored_network(&state, Uuid::new_v4(), "default", "10.35.0.0/24", true);

    assert_eq!(
        network(&state, &mut job, RequestId(input.id))
            .await
            .unwrap(),
        reserved.id
    );
    let reopened = AppState::with_db_file(
        TemplateRegistry::from_specs(temp.path(), []).unwrap(),
        temp.path().join("workspaces.sqlite"),
    )
    .await
    .unwrap();
    let mut restored = read(&reopened, input.id).await.unwrap();
    assert_eq!(
        network(&reopened, &mut restored, RequestId(input.id))
            .await
            .unwrap(),
        reserved.id
    );
    assert_eq!(restored.progress.vm_id, first.vm_id);

    reopened
        .store
        .set_micro_network_internet(reserved.id, false)
        .unwrap();
    assert_eq!(
        network(&reopened, &mut restored, RequestId(input.id))
            .await
            .unwrap_err()
            .code(),
        "workspace_network_unavailable"
    );
    assert_eq!(
        read(&reopened, input.id)
            .await
            .unwrap()
            .progress
            .micro_network_id,
        Some(reserved.id)
    );
    assert_eq!(reopened.store.list_micro_networks().unwrap().len(), 2);
    assert!(reopened.vms.lock().unwrap().is_empty());
}

#[tokio::test]
async fn automatic_creation_skips_an_occupied_subnet_and_keeps_the_reserved_identity() {
    let (temp, state) = fixture().await;
    let socket = temp.path().join("network.sock");
    let _helper = crate::network::test_support::spawn_always_ok_helper(&socket);
    let state = state.with_test_network(crate::network::NetworkClient::with_socket_path(socket));
    let occupied = stored_network(&state, Uuid::new_v4(), "offline", "172.30.240.0/24", false);
    let input = input();
    create_without_worker(&state, input.clone()).await.unwrap();
    let mut job = read(&state, input.id).await.unwrap();

    let id = network(&state, &mut job, RequestId(input.id))
        .await
        .unwrap();
    let created = state.store.micro_network(id).unwrap().unwrap();
    assert_eq!(id, job.reserved_network);
    assert_ne!(created.subnet_cidr, occupied.subnet_cidr);
    assert!(created.internet_enabled);
    assert_eq!(
        read(&state, input.id)
            .await
            .unwrap()
            .progress
            .micro_network_id,
        Some(id)
    );
    assert_eq!(
        network(&state, &mut job, RequestId(input.id))
            .await
            .unwrap(),
        id
    );
    assert_eq!(state.store.list_micro_networks().unwrap().len(), 2);
    assert!(state.vms.lock().unwrap().is_empty());
}
#[tokio::test]
async fn retries_and_restart_keep_the_same_reserved_vm_and_network() {
    let (temp, state) = fixture().await;
    let input = input();
    assert_eq!((input.cpu, input.ram, input.disk_gb), (2, 4096, 32));
    let first = create_without_worker(&state, input.clone()).await.unwrap();
    let second = create_without_worker(&state, input.clone()).await.unwrap();
    assert_eq!(first.vm_id, second.vm_id);
    let saved = state.store.workspace_setup(input.id).unwrap().unwrap();
    let reopened = AppState::with_db_file(
        TemplateRegistry::from_specs(temp.path(), []).unwrap(),
        temp.path().join("workspaces.sqlite"),
    )
    .await
    .unwrap();
    let third = create_without_worker(&reopened, input.clone())
        .await
        .unwrap();
    assert_eq!(third.vm_id, first.vm_id);
    assert_eq!(
        reopened
            .store
            .workspace_setup(input.id)
            .unwrap()
            .unwrap()
            .reserved_network,
        saved.reserved_network
    );
    assert!(state.vms.lock().unwrap().is_empty());
    let mut changed = input;
    changed.cpu = 4;
    assert_eq!(
        create_without_worker(&state, changed)
            .await
            .unwrap_err()
            .code(),
        "workspace_request_changed"
    );
}
#[tokio::test]
async fn inspect_is_read_only_and_failed_jobs_require_an_explicit_retry() {
    let (_temp, state) = fixture().await;
    let input = input();
    let first = create_without_worker(&state, input.clone()).await.unwrap();
    let mut saved = read(&state, input.id).await.unwrap();
    saved.progress.state = Status::Failed;
    saved.progress.error = Some("image download failed".into());
    save(&state, &saved).await.unwrap();
    state.workspace_setups.workers.lock().unwrap().clear();
    recover(&state).await;
    let Json(inspected) = get(State(state.clone()), Path(input.id)).await.unwrap();
    assert_eq!(inspected.state, Status::Failed);
    assert!(state.workspace_setups.workers.lock().unwrap().is_empty());
    let retried = create_without_worker(&state, input).await.unwrap();
    assert_eq!(retried.vm_id, first.vm_id);
    assert_eq!(retried.state, Status::Queued);
    assert_eq!(retried.error, None);
}
#[tokio::test]
async fn queue_limits_and_invalid_resources_do_not_create_records() {
    let (_temp, state) = fixture().await;
    let mut invalid = input();
    invalid.ram = 1536;
    invalid.cpu = 0;
    invalid.disk_gb = 1;
    let id = invalid.id;
    let error = create_without_worker(&state, invalid).await.unwrap_err();
    assert_eq!(error.code(), "validation_failed");
    assert_eq!(error.field_names().len(), 3);
    assert!(state.store.workspace_setup(id).unwrap().is_none());
    for _ in 0..8 {
        create_without_worker(&state, input()).await.unwrap();
    }
    assert_eq!(
        create_without_worker(&state, input())
            .await
            .unwrap_err()
            .code(),
        "workspace_queue_full"
    );
    assert_eq!(state.store.workspace_setups().unwrap().len(), 8);
}
#[tokio::test]
async fn persistence_rejects_replacing_a_reserved_vm_identity() {
    let (_temp, state) = fixture().await;
    let input = input();
    create_without_worker(&state, input.clone()).await.unwrap();
    let mut saved = read(&state, input.id).await.unwrap();
    let vm = saved.progress.vm_id;
    saved.progress.vm_id = Uuid::new_v4();
    assert!(state.store.save_workspace_setup(&saved).is_err());
    assert_eq!(read(&state, input.id).await.unwrap().progress.vm_id, vm);
}

#[tokio::test]
async fn restart_resumes_a_queued_job_and_image_failure_cannot_create_a_vm() {
    let (temp, state) = fixture().await;
    let input = input();
    let first = create_without_worker(&state, input.clone()).await.unwrap();
    let mut saved = read(&state, input.id).await.unwrap();
    saved.image_reference = Some(format!(
        "127.0.0.1:1/library/ubuntu@sha256:{}",
        "a".repeat(64)
    ));
    saved.kernel_version = Some("7.3.1".into());
    save(&state, &saved).await.unwrap();
    let mut reopened = AppState::with_db_file(
        TemplateRegistry::from_specs(temp.path(), []).unwrap(),
        temp.path().join("workspaces.sqlite"),
    )
    .await
    .unwrap();
    reopened.image_packages = crate::image_install::ImageInstallTracker::disabled();
    reopened.image_installs = crate::image_install::ImageInstallTracker::disabled();
    reopened.kernel_installs = crate::image_install::ImageInstallTracker::disabled();
    recover(&reopened).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let job = read(&reopened, input.id).await.unwrap();
        if job.progress.state == Status::Failed {
            assert_eq!(job.progress.vm_id, first.vm_id);
            assert!(job.progress.error.is_some());
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "recovered worker did not finish"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(reopened.vms.lock().unwrap().is_empty());
    assert!(reopened.store.list_micro_networks().unwrap().is_empty());
}

#[tokio::test]
async fn old_failed_image_records_remain_retryable_without_changing_the_reserved_vm() {
    let (_temp, state) = fixture().await;
    let input = input();
    let first = create_without_worker(&state, input.clone()).await.unwrap();
    let saved = read(&state, input.id).await.unwrap();
    let mut old = serde_json::to_value(saved).unwrap();
    old.as_object_mut().unwrap().remove("image_reference");
    old.as_object_mut().unwrap().remove("kernel_version");
    old["progress"].as_object_mut().unwrap().remove("log");
    old["progress"]["state"] = "failed".into();
    old["progress"]["error"] = "workspace_image_failed: old package HTTP 404".into();
    let restored: SavedSetup = serde_json::from_value(old).unwrap();
    assert!(restored.image_reference.is_none());
    assert!(restored.kernel_version.is_none());
    assert!(restored.progress.log.is_empty());
    save(&state, &restored).await.unwrap();
    let retried = create_without_worker(&state, input).await.unwrap();
    assert_eq!(retried.vm_id, first.vm_id);
    assert_eq!(retried.state, Status::Queued);
}

#[tokio::test]
async fn cached_oci_rootfs_uses_the_latest_host_kernel_and_pins_it_for_retries() {
    use crate::image_install::{Architecture, ImageInstallTracker};
    use crate::oci::{self, ImageReference};
    use crate::templates::TemplateSpec;

    let (temp, mut state) = fixture().await;
    state.kernel_installs = ImageInstallTracker::disabled();
    state.image_packages = ImageInstallTracker::disabled();
    state.image_installs = ImageInstallTracker::disabled();
    for version in ["7.2.12", "7.3.1"] {
        oci::kernel_tests::install_fixture_kernel(temp.path(), Architecture::HOST, version).await;
    }
    let entries = [
        ("7.2.12", Architecture::HOST, false),
        ("7.3.1", Architecture::HOST, false),
        ("7.4.0", Architecture::HOST, true),
        ("8.0.0", Architecture::HOST.other(), false),
    ].map(|(version, architecture, eol)| serde_json::json!({
        "alias": format!("vmlinux-{version}"), "version": version,
        "architecture": architecture.as_str(), "revision": 2,
        "package": format!("kernel/{version}/r2/{}/vmlinux-{version}.tar.zst", architecture.as_str()),
        "sha256": "a".repeat(64),
        "image": oci::kernel::image_name(architecture, version).unwrap(),
        "format": if architecture == Architecture::Aarch64 { "arm64-image" } else { "elf" },
        "source": { "url": oci::kernel::source_url(version).unwrap(), "sha256": "b".repeat(64) },
        "iseol": eol,
    }));
    let catalog =
        serde_json::to_vec(&serde_json::json!({"schemaVersion":1,"kernels":entries})).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    state.kernel_installs =
        ImageInstallTracker::with_base_url(format!("http://{}", listener.local_addr().unwrap()));
    let router = axum::Router::new().route(
        "/kernel/catalog.json",
        axum::routing::get(move || {
            let catalog = catalog.clone();
            async move { catalog }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let reference = format!("docker.io/library/ubuntu@sha256:{}", "a".repeat(64));
    let original_alias =
        oci::template_name_from_reference(&ImageReference::parse(&reference).unwrap())
            .unwrap()
            .alias;
    let original_kernel = crate::kernel_manager::cache_path("7.2.12").unwrap();
    std::fs::write(temp.path().join("oci-rootfs"), b"OCI Ubuntu rootfs").unwrap();
    state
        .templates
        .register_spec(TemplateSpec {
            alias: original_alias.clone(),
            version: "oci-digest".into(),
            kernel: original_kernel.clone(),
            initrd: None,
            rootfs: "oci-rootfs".into(),
            boot_args: "root=/dev/vda rw".into(),
        })
        .unwrap();
    let input = input();
    let reserved = create_without_worker(&state, input.clone()).await.unwrap();
    let mut job = read(&state, input.id).await.unwrap();
    job.image_reference = Some(reference.clone());
    save(&state, &job).await.unwrap();

    let alias = image::prepare(&state, &mut job).await.unwrap();
    assert_eq!(job.kernel_version.as_deref(), Some("7.3.1"));
    server.abort();
    let _ = server.await;
    let prepared = state.templates.resolve_alias(&alias).unwrap();
    assert_eq!(
        prepared.kernel.relative_path(),
        crate::kernel_manager::cache_path("7.3.1").unwrap()
    );
    assert_eq!(
        prepared.rootfs.relative_path(),
        std::path::Path::new("oci-rootfs")
    );
    assert_eq!(
        state
            .templates
            .resolve_alias(&original_alias)
            .unwrap()
            .kernel
            .relative_path(),
        original_kernel
    );
    let count = state.templates.list_aliases().len();
    assert_eq!(image::prepare(&state, &mut job).await.unwrap(), alias);
    assert_eq!(state.templates.list_aliases().len(), count);
    assert_eq!(
        read(&state, input.id).await.unwrap().progress.vm_id,
        reserved.vm_id
    );
    assert_eq!(
        read(&state, input.id)
            .await
            .unwrap()
            .image_reference
            .as_deref(),
        Some(reference.as_str())
    );
    assert!(state.vms.lock().unwrap().is_empty());
    assert!(state.store.list_micro_networks().unwrap().is_empty());
    let Json(progress) = get(State(state), Path(input.id)).await.unwrap();
    assert!(progress.log.contains("Linux kernel 7.3.1"));
    assert!(!progress.log.contains("registry.firecrab.dev/ubuntu"));
}

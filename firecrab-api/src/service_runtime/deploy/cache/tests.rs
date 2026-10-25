use super::*;
use crate::service_runtime::tests::{deployment, state};
use crate::templates::TemplateSpec;
use firecrab_api_types::MicroServiceResponse;
use std::fs;

fn release(service: Uuid, image: u8) -> MicroServiceDeployment {
    let mut record = deployment(service);
    record.phase = ServicePhase::Running;
    record.digest = Some(format!("sha256:{}", format!("{image:02x}").repeat(32)));
    record
}

fn owned_image(state: &AppState, record: &MicroServiceDeployment, register: bool) -> TemplateSpec {
    let spec = TemplateSpec {
        alias: template_alias(record).unwrap(),
        version: record.digest.clone().unwrap(),
        kernel: "kernel/test".into(),
        initrd: None,
        rootfs: format!("rootfs/deploy/{}.ext4", Uuid::new_v4()).into(),
        boot_args: "console=ttyS0 root=/dev/vda rw".into(),
    };
    let root = state.templates.image_root_path();
    fs::create_dir_all(root.join("kernel")).unwrap();
    fs::create_dir_all(root.join("rootfs/deploy")).unwrap();
    fs::write(root.join(&spec.kernel), b"unclassified-test-kernel").unwrap();
    state.store.remember_deploy_image(&spec).unwrap();
    fs::write(root.join(&spec.rootfs), b"owned-ext4-fixture").unwrap();
    if register {
        state.templates.register_spec(spec.clone()).unwrap();
    }
    spec
}

#[tokio::test]
async fn history_retains_twenty_rootfs_files_and_keeps_manual_images_and_kernels() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let service = Uuid::new_v4();
    let mut images = vec![];
    for index in 0..23 {
        let record = release(service, index);
        images.push(owned_image(&state, &record, true));
        state.store.save_service_deployment(&record).unwrap();
    }
    fs::write(root.path().join("rootfs/manual.ext4"), b"manual-image").unwrap();
    let mut manual = images[0].clone();
    manual.alias = "manual".into();
    manual.rootfs = "rootfs/manual.ext4".into();
    state.templates.register_spec(manual.clone()).unwrap();
    assert_eq!(collect(&state).await.unwrap(), 3);
    assert_eq!(state.store.deploy_images().unwrap().len(), 20);
    for (index, spec) in images.iter().enumerate() {
        assert_eq!(root.path().join(&spec.rootfs).exists(), index >= 3);
        assert_eq!(
            state.templates.resolve_alias(&spec.alias).is_some(),
            index >= 3
        );
    }
    assert!(root.path().join(&manual.rootfs).exists());
    assert!(root.path().join(&manual.kernel).exists());
    assert_eq!(collect(&state).await.unwrap(), 0);
}

#[tokio::test]
async fn one_shared_image_survives_until_both_services_release_it() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let first = release(Uuid::new_v4(), 1);
    let spec = owned_image(&state, &first, true);
    let mut second = release(Uuid::new_v4(), 1);
    second.spec.name = "second-service".into();
    for record in [&first, &second] {
        state
            .store
            .insert_micro_service(&MicroServiceResponse {
                id: record.service_id,
                spec: record.spec.clone(),
            })
            .unwrap();
        state.store.save_service_deployment(record).unwrap();
    }
    state.store.delete_micro_service(first.service_id).unwrap();
    assert_eq!(collect(&state).await.unwrap(), 0);
    state.store.delete_micro_service(second.service_id).unwrap();
    assert_eq!(collect(&state).await.unwrap(), 1);
    assert!(!root.path().join(&spec.rootfs).exists());
}

#[tokio::test]
async fn pinned_configuration_is_retained_but_clean_failed_attempts_do_not_pin_garbage() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut record = release(Uuid::new_v4(), 7);
    let spec = owned_image(&state, &record, true);
    record.phase = ServicePhase::Failed;
    state.store.save_service_deployment(&record).unwrap();
    let mut config = record.spec.clone();
    config.image = pinned_reference(&record).unwrap();
    state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id: record.service_id,
            spec: config,
        })
        .unwrap();
    assert_eq!(collect(&state).await.unwrap(), 0);
    state.store.delete_micro_service(record.service_id).unwrap();
    state.store.save_service_deployment(&record).unwrap();
    assert_eq!(collect(&state).await.unwrap(), 1);
    assert!(!root.path().join(spec.rootfs).exists());
}

#[tokio::test]
async fn durable_vm_and_pool_users_and_inflight_template_readers_block_collection() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let record = release(Uuid::new_v4(), 2);
    let spec = owned_image(&state, &record, true);
    let mut vm = vms::test_support::record("standalone", Uuid::new_v4());
    vm.template = spec.alias.clone();
    state.store.insert(&vm).unwrap();
    // The durable row protects the image even before the in-memory VM map loads.
    assert_eq!(collect(&state).await.unwrap(), 0);
    state.store.delete(vm.id).unwrap();
    let handle = state.templates.resolve_alias(&spec.alias).unwrap();
    assert_eq!(collect(&state).await.unwrap(), 0);
    drop(handle);
    let pool = crate::persistence::PoolRow {
        id: Uuid::new_v4(),
        name: "cached-pool".into(),
        template: spec.alias.clone(),
        template_version: spec.version.clone(),
        cpu: 1,
        ram: 128,
        disk_gb: 1,
        egress_policy: EgressPolicy::Internet,
        micro_network_id: Uuid::new_v4(),
        storage_root: "default".into(),
        min_ready: 0,
        max_size: 1,
        lease_ttl_seconds: 60,
        deleting: false,
        created_at_ms: 0,
    };
    state.store.insert_pool(&pool).unwrap();
    assert_eq!(collect(&state).await.unwrap(), 0);
    assert!(root.path().join(&spec.rootfs).exists());
}

#[tokio::test]
async fn incomplete_updates_pin_both_generations_after_a_drain_failure() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut record = crate::service_runtime::deploy::tests::journal();
    let previous = owned_image(&state, &record.update.as_ref().unwrap().previous, true);
    let current = owned_image(&state, &record, true);
    record.phase = ServicePhase::Failed;
    state.store.save_service_deployment(&record).unwrap();
    assert_eq!(collect(&state).await.unwrap(), 0);
    record.update.as_mut().unwrap().completed = true;
    record.update.as_mut().unwrap().previous.instances.clear();
    state.store.save_service_deployment(&record).unwrap();
    assert_eq!(collect(&state).await.unwrap(), 1);
    assert!(!root.path().join(previous.rootfs).exists());
    assert!(root.path().join(current.rootfs).exists());
}

#[tokio::test]
async fn published_image_before_registration_is_recovered_without_an_import() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let record = release(Uuid::new_v4(), 3);
    let spec = owned_image(&state, &record, false);
    state.store.save_service_deployment(&record).unwrap();
    drop(state);
    let state = crate::service_runtime::tests::state(root.path()).await;
    assert!(state.templates.resolve_alias(&spec.alias).is_none());
    assert!(
        restore_cached(&state, &spec.alias, &spec.version)
            .await
            .unwrap()
    );
    assert_eq!(
        state
            .templates
            .resolve_alias(&spec.alias)
            .unwrap()
            .rootfs
            .relative_path(),
        spec.rootfs
    );
    assert_eq!(collect(&state).await.unwrap(), 0);
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn initial_deploy_recovers_a_published_image_without_registry_access() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut record = release(Uuid::new_v4(), 8);
    let spec = owned_image(&state, &record, false);
    record.spec.image = pinned_reference(&record).unwrap();
    record.digest = None;
    record.phase = ServicePhase::Queued;
    record.spec.domain = Some("web.example.test".into());
    record.request.micro_network_id = Some(Uuid::new_v4());
    // Stop after image preparation at the normal placement validation boundary.
    record.spec.security_group_id = Some(Uuid::new_v4());
    let credential = require_image_auth(&state, &record.spec.image, record.id)
        .await
        .unwrap()
        .registry_credential()
        .await
        .unwrap();
    let error = execute(&state, &mut record, credential).await.unwrap_err();
    assert_eq!(error, "security group does not exist");
    assert_eq!(record.digest.as_deref(), Some(spec.version.as_str()));
    assert!(state.templates.resolve_alias(&spec.alias).is_some());
    assert_eq!(
        state.oci_imports.snapshot(&spec.alias).status,
        ImageInstallStatus::Idle
    );
    assert_eq!(collect(&state).await.unwrap(), 0);
}

#[tokio::test]
async fn explicit_digest_reuses_verified_cache_without_registry_access() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut record = release(Uuid::new_v4(), 6);
    let spec = owned_image(&state, &record, true);
    record.spec.image = pinned_reference(&record).unwrap();
    record.digest = None;
    record.phase = ServicePhase::Queued;
    prepare_template(&state, &mut record, false).await.unwrap();
    assert_eq!(record.digest.as_deref(), Some(spec.version.as_str()));
    assert_eq!(
        state.oci_imports.snapshot(&spec.alias).status,
        ImageInstallStatus::Idle
    );
}

#[tokio::test]
async fn shared_artifact_registration_and_corrupt_manifests_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let spec = owned_image(&state, &release(Uuid::new_v4(), 4), true);
    let mut shared = spec.clone();
    shared.alias = "manual-shared-rootfs".into();
    state.templates.register_spec(shared.clone()).unwrap();
    assert_eq!(collect(&state).await.unwrap(), 0);
    state.templates.unregister_alias(&shared.alias).unwrap();
    let manifest = root.path().join(".templates.json");
    let saved = fs::read(&manifest).unwrap();
    fs::write(&manifest, b"damaged-json").unwrap();
    assert!(collect(&state).await.is_err());
    assert!(root.path().join(&spec.rootfs).exists());
    assert!(state.templates.resolve_alias(&spec.alias).is_some());
    fs::write(manifest, saved).unwrap();
    assert_eq!(collect(&state).await.unwrap(), 1);
}

#[tokio::test]
async fn failed_unlink_keeps_the_journal_for_retry_and_rejects_symlink_parents() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let spec = owned_image(&state, &release(Uuid::new_v4(), 5), false);
    let path = root.path().join(&spec.rootfs);
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(collect(&state).await.is_err());
    assert_eq!(state.store.deploy_images().unwrap().len(), 1);
    fs::remove_dir(&path).unwrap();
    fs::write(&path, b"retry-owned-file").unwrap();
    let directory = root.path().join("rootfs/deploy");
    let moved = root.path().join("owned-files");
    fs::rename(&directory, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &directory).unwrap();
    assert!(collect(&state).await.is_err());
    assert!(path.exists());
    fs::remove_file(&directory).unwrap();
    fs::rename(moved, directory).unwrap();
    assert_eq!(collect(&state).await.unwrap(), 1);
    assert!(state.store.deploy_images().unwrap().is_empty());
}

use super::*;
fn fixture() -> (tempfile::TempDir, Store, KernelDefinition) {
    let root = tempfile::tempdir().unwrap();
    let images = root.path().join("images");
    std::fs::create_dir(&images).unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let definition = KernelDefinition {
        version: "7.2.12".into(),
        architecture: crate::image_install::Architecture::HOST.as_str().into(),
        root: KernelRootPin::capture(&images).unwrap(),
        releases_enabled: true,
        expected_image_sha256: None,
        recipe: crate::oci::kernel::BUILD_REVISION.into(),
        initial_source: None,
    };
    (root, store, definition)
}
fn source() -> KernelSourcePin {
    KernelSourcePin {
        url: crate::oci::kernel::source_url("7.2.12").unwrap(),
        sha256: "a".repeat(64),
    }
}
#[test]
fn iac_kernel_source_and_target_survive_restart_without_resolving_stable_again() {
    let (root, store, definition) = fixture();
    let id = Uuid::new_v4();
    let (intent, dispatch) = store
        .begin_kernel(
            id,
            KernelRequest::Install("stable".into()),
            definition.clone(),
        )
        .unwrap();
    assert!(dispatch);
    store.prepare_kernel_source(&intent, &source()).unwrap();
    store.kernel_effects(&intent).unwrap();
    store.fail_kernel(&intent).unwrap();
    let second = Store::open(&root.path().join("state.db")).unwrap();
    let (recovered, dispatch) = second.recover_kernel_at(id, 0).unwrap();
    assert!(dispatch);
    assert_eq!(recovered.operation.execution_generation, 1);
    assert_eq!(
        second.kernel_source(&recovered).unwrap().unwrap().sha256,
        source().sha256
    );
    assert!(store.check_kernel(&intent).is_err());
    let mut changed = source();
    changed.sha256 = "b".repeat(64);
    assert!(second.prepare_kernel_source(&recovered, &changed).is_err());
    let outcome = KernelOutcome {
        version: "7.2.12".into(),
        architecture: definition.architecture.clone(),
        installed: true,
        image_sha256: Some("c".repeat(64)),
        source_sha256: Some(source().sha256),
    };
    second.finish_kernel(&recovered, &outcome).unwrap();
    let mut newer = definition;
    newer.version = "7.3.0".into();
    let (retry, dispatch) = store
        .begin_kernel(id, KernelRequest::Install("stable".into()), newer)
        .unwrap();
    assert!(!dispatch);
    assert_eq!(retry.definition().version, "7.2.12");
    assert_eq!(
        store.kernel_receipt(id).unwrap().unwrap().image_sha256,
        outcome.image_sha256
    );
}
#[test]
fn iac_kernel_unknown_work_reserves_host_configuration_and_rejects_stale_recovery() {
    let (_root, store, definition) = fixture();
    let id = Uuid::new_v4();
    let (intent, _) = store
        .begin_kernel(id, KernelRequest::Install("7.2.12".into()), definition)
        .unwrap();
    store.prepare_kernel_source(&intent, &source()).unwrap();
    store.kernel_effects(&intent).unwrap();
    store.fail_kernel(&intent).unwrap();
    assert!(matches!(
        store.iac_public_write_allowed(&ManagedResource {
            kind: "host_configuration".into(),
            id: "default".into()
        }),
        Err(PersistenceError::IacResourceBusy)
    ));
    let (next, _) = store.recover_kernel_at(id, 0).unwrap();
    store.fail_kernel(&next).unwrap();
    assert!(matches!(
        store.recover_kernel_at(id, 0),
        Err(PersistenceError::StaleIacExecution)
    ));
    assert_eq!(
        store
            .iac_native_operation(id)
            .unwrap()
            .unwrap()
            .execution_generation,
        1
    );
}
#[test]
fn iac_kernel_known_no_effect_failure_releases_only_its_original_scope() {
    let (_root, store, definition) = fixture();
    let id = Uuid::new_v4();
    let (intent, _) = store
        .begin_kernel(
            id,
            KernelRequest::Install("7.2.12".into()),
            definition.clone(),
        )
        .unwrap();
    store.fail_kernel(&intent).unwrap();
    assert!(
        store
            .iac_public_write_allowed(&ManagedResource {
                kind: "host_configuration".into(),
                id: "default".into()
            })
            .unwrap()
    );
    let retry = store
        .kernel_retry(id, &KernelRequest::Install("7.2.12".into()))
        .unwrap()
        .unwrap();
    assert_eq!(retry.operation.state, NativeOperationState::Failed);
    assert!(!store.recover_kernel_at(id, 0).unwrap().1);
    assert!(
        store
            .begin_kernel(
                Uuid::new_v4(),
                KernelRequest::Install("7.2.12".into()),
                definition
            )
            .unwrap()
            .1
    );
}
#[test]
fn iac_kernel_physical_scope_rejects_same_path_with_a_replaced_directory() {
    let (root, _store, definition) = fixture();
    let images = root.path().join("images");
    definition.root.check(&images).unwrap();
    std::fs::rename(&images, root.path().join("original-images")).unwrap();
    std::fs::create_dir(&images).unwrap();
    assert!(definition.root.check(&images).is_err());
}

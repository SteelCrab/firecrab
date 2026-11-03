use super::*;
fn fixture() -> (tempfile::TempDir, Store, ImageDefinition) {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let definition = ImageDefinition {
        alias: "test-image".into(),
        architecture: Architecture::HOST.as_str().into(),
        root: super::super::iac_kernels::KernelRootPin::capture(root.path()).unwrap(),
        before: None,
        source: ImageSource::Package {
            spec: version().spec,
            archive_sha256: "a".repeat(64),
        },
        dependencies: vec![],
    };
    (root, store, definition)
}
fn request() -> ImageRequest {
    ImageRequest::Install {
        alias: "test-image".into(),
    }
}

#[test]
fn iac_image_source_login_is_frozen_before_admission_and_leased_through_uncertainty() {
    let (_root, store, mut definition) = fixture();
    let login = crate::oci::RegistryCredential {
        registry: "oci.example.test".into(),
        username: "fixture".into(),
        secret: "private-image-fixture-token".into(),
    };
    store.put_registry_login(&login, None).unwrap();
    let target = ManagedResource {
        kind: "registry".into(),
        id: login.registry.clone(),
    };
    let pin = store.image_auth_pin(&target).unwrap().unwrap();
    let original = "oci.example.test/app:v1";
    let named = crate::oci::template_name_from_reference(
        &crate::oci::ImageReference::parse(original).unwrap(),
    )
    .unwrap();
    definition.alias = named.alias;
    definition.source = ImageSource::Oci {
        reference: format!("oci.example.test/app@sha256:{}", "a".repeat(64)),
        naming_reference: original.into(),
        display_version: "v1".into(),
        kernel_version: "7.2.2".into(),
        kernel_override: None,
        credential: Some(ImageCredential {
            registry: login.registry.clone(),
            username: login.username.clone(),
            secret: login.secret.clone(),
            refresh_auth: None,
        }),
        authentication: Some(pin),
    };
    let mut rotated = login.clone();
    rotated.secret = "rotated-private-image-fixture-token".into();
    store.put_registry_login(&rotated, None).unwrap();
    assert!(matches!(
        store.begin_image(
            Uuid::new_v4(),
            ImageRequest::Import {
                reference: original.into()
            },
            definition.clone()
        ),
        Err(PersistenceError::StaleIacExecution)
    ));
    store.put_registry_login(&login, None).unwrap();
    let (intent, _) = store
        .begin_image(
            Uuid::new_v4(),
            ImageRequest::Import {
                reference: original.into(),
            },
            definition,
        )
        .unwrap();
    assert!(matches!(
        store.put_registry_login(&rotated, None),
        Err(PersistenceError::IacResourceBusy)
    ));
    store.image_effects(&intent).unwrap();
    store.fail_image(&intent).unwrap();
    assert!(matches!(
        store.delete_registry_credential(&target.id),
        Err(PersistenceError::IacResourceBusy)
    ));
    let public = serde_json::to_string(
        &store
            .iac_native_operation(intent.operation.operation_id)
            .unwrap(),
    )
    .unwrap();
    assert!(!public.contains(&login.secret));
    assert!(!public.contains(&rotated.secret));
}
#[test]
fn iac_image_ecr_refresh_keeps_original_signing_input_and_rejects_changed_binding() {
    use crate::pipe_auth::{AwsCredentials, SavedAuth};
    let (root, store, mut definition) = fixture();
    let source = SavedAuth::Aws {
        account_id: "123456789012".into(),
        credentials: AwsCredentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_access_key: "private-original-signing-key".into(),
            session_token: "private-original-session-token".into(),
            region: "ap-northeast-2".into(),
        },
    };
    store.put_pipe_auth(&source).unwrap();
    let registry = source.connection().registry.unwrap();
    let target = ManagedResource {
        kind: "auth".into(),
        id: "aws".into(),
    };
    let pin = store.image_auth_pin(&target).unwrap().unwrap();
    let login = ImageCredential {
        registry: registry.clone(),
        username: "AWS".into(),
        secret: "private-expiring-ecr-login".into(),
        refresh_auth: Some(source.clone()),
    };
    assert!(login.matches_source(&registry, Some(&pin)));
    let mut invalid = login.clone();
    if let Some(SavedAuth::Aws { credentials, .. }) = invalid.refresh_auth.as_mut() {
        credentials.secret_access_key = "different-original-key".into();
    }
    assert!(!invalid.matches_source(&registry, Some(&pin)));
    let original = format!("{registry}/app:v1");
    let name = crate::oci::template_name_from_reference(
        &crate::oci::ImageReference::parse(&original).unwrap(),
    )
    .unwrap();
    definition.alias = name.alias;
    definition.dependencies.push(target);
    definition.source = ImageSource::Oci {
        reference: format!("{registry}/app@sha256:{}", "a".repeat(64)),
        naming_reference: original.clone(),
        display_version: "v1".into(),
        kernel_version: "7.2.2".into(),
        kernel_override: None,
        credential: Some(login),
        authentication: Some(pin),
    };
    let id = Uuid::new_v4();
    let (intent, _) = store
        .begin_image(
            id,
            ImageRequest::Import {
                reference: original,
            },
            definition,
        )
        .unwrap();
    store.image_effects(&intent).unwrap();
    store.fail_image(&intent).unwrap();
    drop(store);
    let reopened = Store::open(&root.path().join("state.db")).unwrap();
    let retained = reopened.image_intent(id).unwrap().unwrap();
    let ImageSource::Oci {
        credential: Some(login),
        authentication: Some(pin),
        ..
    } = &retained.definition().source
    else {
        panic!("missing original input")
    };
    assert!(login.matches_source(&registry, Some(pin)));
    assert_eq!(
        serde_json::to_value(login.refresh_auth.as_ref().unwrap()).unwrap(),
        serde_json::to_value(source).unwrap()
    );
    let public = serde_json::to_string(&retained.operation).unwrap();
    for private in [
        "private-original-signing-key",
        "private-original-session-token",
        "private-expiring-ecr-login",
    ] {
        assert!(!public.contains(private));
    }
    assert!(matches!(
        reopened.delete_pipe_auth(crate::pipe_auth::Provider::Aws),
        Err(PersistenceError::IacResourceBusy)
    ));
}

#[tokio::test]
async fn iac_image_legacy_static_credential_keeps_its_exact_wire_shape_and_login() {
    let legacy = serde_json::json!({"registry":"oci.example.test", "username":"original", "secret":"private-static-login"});
    let login: ImageCredential = serde_json::from_value(legacy.clone()).unwrap();
    assert!(login.refresh_auth.is_none());
    assert_eq!(serde_json::to_value(&login).unwrap(), legacy);
    let resolved = login.login().await.unwrap();
    assert_eq!(resolved.registry, "oci.example.test");
    assert_eq!(resolved.secret, "private-static-login");
}

fn version() -> ImageVersion {
    ImageVersion {
        spec: TemplateSpec {
            alias: "test-image".into(),
            version: "v1".into(),
            kernel: "kernel".into(),
            rootfs: "rootfs".into(),
            initrd: None,
            boot_args: "console=ttyS0".into(),
        },
        pin: TemplatePin {
            version: "v1".into(),
            kernel_sha256: "b".repeat(64),
            rootfs_sha256: "c".repeat(64),
            initrd_sha256: None,
            boot_args_sha256: "d".repeat(64),
        },
    }
}
fn outcome() -> ImageOutcome {
    let pin = version().pin;
    ImageOutcome {
        alias: "test-image".into(),
        installed: true,
        image: Some(firecrab_api_types::ImageResponse {
            alias: "test-image".into(),
            version: pin.version,
            kernel_version: None,
            kernel_image: "kernel".into(),
            kernel_sha256: pin.kernel_sha256,
            rootfs_sha256: pin.rootfs_sha256,
            initrd_sha256: None,
            min_disk_gb: 1,
            rootfs_size_bytes: 4096,
            installed: true,
            package_url: None,
            package_staged: true,
            package_origin: None,
            description: String::new(),
            has_guest_service: false,
        }),
    }
}
#[test]
fn iac_image_restart_keeps_original_prepared_bytes_and_rejects_stale_callbacks() {
    let (root, store, definition) = fixture();
    let id = Uuid::new_v4();
    let (original, dispatch) = store.begin_image(id, request(), definition).unwrap();
    assert!(dispatch);
    store.prepare_image(&original, &version()).unwrap();
    store.image_effects(&original).unwrap();
    store.interrupt_native_iac_operations().unwrap();
    let reopened = Store::open(&root.path().join("state.db")).unwrap();
    let (resumed, dispatch) = reopened.recover_image_at(id, 0).unwrap();
    assert!(dispatch);
    assert_eq!(resumed.operation.execution_generation, 1);
    assert!(store.check_image(&original).is_err());
    assert_eq!(
        reopened.prepared_image(&resumed).unwrap().unwrap().pin,
        version().pin
    );
    let mut different = version();
    different.pin.rootfs_sha256 = "e".repeat(64);
    assert!(reopened.prepare_image(&resumed, &different).is_err());
    reopened.finish_image(&resumed, &outcome()).unwrap();
    assert_eq!(
        reopened
            .image_receipt(id)
            .unwrap()
            .unwrap()
            .image
            .unwrap()
            .rootfs_sha256,
        version().pin.rootfs_sha256
    );
    assert!(store.finish_image(&original, &outcome()).is_err());
}
#[test]
fn iac_image_prepared_publication_cannot_move_to_a_different_physical_locator() {
    let (_root, store, definition) = fixture();
    let (intent, _) = store
        .begin_image(Uuid::new_v4(), request(), definition)
        .unwrap();
    store.prepare_image(&intent, &version()).unwrap();
    let mut changed = version();
    changed.spec.rootfs = "other-rootfs".into();
    assert!(matches!(
        store.prepare_image(&intent, &changed),
        Err(PersistenceError::IacOperationIdentity)
    ));
    store.prepare_image(&intent, &version()).unwrap();
}
#[test]
fn iac_image_no_effect_failure_releases_its_alias_and_parent_but_never_replays() {
    let (_root, store, definition) = fixture();
    let id = Uuid::new_v4();
    let (intent, _) = store
        .begin_image(id, request(), definition.clone())
        .unwrap();
    store.fail_image(&intent).unwrap();
    assert!(
        store
            .iac_public_write_allowed(&resource("test-image"))
            .unwrap()
    );
    assert!(
        store
            .iac_public_write_allowed(&ManagedResource {
                kind: "host_configuration".into(),
                id: "default".into()
            })
            .unwrap()
    );
    assert_eq!(
        store
            .image_retry(id, &request())
            .unwrap()
            .unwrap()
            .operation
            .state,
        NativeOperationState::Failed
    );
    let mut newer = definition.clone();
    newer.alias = "another-alias".into();
    assert!(!store.begin_image(id, request(), newer).unwrap().1);
    assert!(
        store
            .begin_image(Uuid::new_v4(), request(), definition)
            .unwrap()
            .1
    );
}
#[test]
fn iac_image_uncertain_attempt_retains_exact_scope_and_generation_after_another_recovery_fails() {
    let (_root, store, definition) = fixture();
    let id = Uuid::new_v4();
    let (intent, _) = store.begin_image(id, request(), definition).unwrap();
    store.image_effects(&intent).unwrap();
    store.fail_image(&intent).unwrap();
    assert!(matches!(
        store.iac_public_write_allowed(&resource("test-image")),
        Err(PersistenceError::IacResourceBusy)
    ));
    let (resumed, _) = store.recover_image_at(id, 0).unwrap();
    store.fail_image(&resumed).unwrap();
    assert!(matches!(
        store.recover_image_at(id, 0),
        Err(PersistenceError::StaleIacExecution)
    ));
    assert_eq!(
        store
            .image_intent(id)
            .unwrap()
            .unwrap()
            .operation
            .execution_generation,
        1
    );
    assert!(store.check_image(&intent).is_err());
}
#[test]
fn iac_image_terminal_retry_returns_original_receipt_after_owner_change() {
    let (_root, store, definition) = fixture();
    let id = Uuid::new_v4();
    let (intent, _) = store
        .begin_image(id, request(), definition.clone())
        .unwrap();
    store.prepare_image(&intent, &version()).unwrap();
    store.image_effects(&intent).unwrap();
    store.finish_image(&intent, &outcome()).unwrap();
    store.lock().execute("INSERT INTO iac_resource_owners(kind,resource_id,generation,manager,owner_id,logical_key) VALUES('image','test-image',1,'terraform',?1,'firecrab_image.imported')",[Uuid::new_v4().to_string()]).unwrap();
    let (retry, dispatch) = store
        .begin_image(id, request(), definition.clone())
        .unwrap();
    assert!(!dispatch);
    assert_eq!(
        retry.operation,
        store
            .image_retry(id, &request())
            .unwrap()
            .unwrap()
            .operation
    );
    assert!(retry.operation.ownership.owner.is_none());
    assert!(matches!(
        store.begin_image(Uuid::new_v4(), request(), definition),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    assert!(
        store
            .image_retry(
                id,
                &ImageRequest::Install {
                    alias: "another".into()
                }
            )
            .is_err()
    );
}
#[test]
fn iac_image_receipt_failure_rolls_back_terminal_state_and_keeps_the_prepared_identity() {
    let (_root, store, definition) = fixture();
    let id = Uuid::new_v4();
    let (intent, _) = store.begin_image(id, request(), definition).unwrap();
    store.prepare_image(&intent, &version()).unwrap();
    store.image_effects(&intent).unwrap();
    store.lock().execute_batch("CREATE TRIGGER reject_image_terminal BEFORE UPDATE ON iac_native_operations WHEN NEW.state='succeeded' AND NEW.kind='image' BEGIN SELECT RAISE(ABORT,'injected image acknowledgement failure'); END;").unwrap();
    assert!(store.finish_image(&intent, &outcome()).is_err());
    assert!(store.image_receipt(id).unwrap().is_none());
    assert_eq!(
        store.image_intent(id).unwrap().unwrap().operation.state,
        NativeOperationState::Running
    );
    store.fail_image(&intent).unwrap();
    store
        .lock()
        .execute_batch("DROP TRIGGER reject_image_terminal")
        .unwrap();
    let (resumed, _) = store.recover_image_at(id, 0).unwrap();
    store.finish_image(&resumed, &outcome()).unwrap();
    assert_eq!(
        store.image_intent(id).unwrap().unwrap().operation.state,
        NativeOperationState::Succeeded
    );
}

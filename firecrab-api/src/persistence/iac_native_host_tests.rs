use super::*;
use firecrab_helper_protocol::{host_iac::HostIdentity, native_iac::Platform};
use firecrab_stack::{ManagedResource, ResourceOwner};

fn fixture() -> (tempfile::TempDir, Store, OwnerChange) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(&directory.path().join("state.db")).unwrap();
    let target = Target {
        platform: Platform::Macos,
        installation_id: Uuid::new_v4(),
        machine_sha256: "a".repeat(64),
        configuration_root: "/native/firecrab".into(),
        directory_id: "1:42".into(),
    };
    let intent = OwnerChange {
        configuration_sha256: "b".repeat(64),
        change: ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: store.iac_endpoint_id().unwrap(),
            resource: ManagedResource {
                kind: "management_settings".into(),
                id: target.installation_id.to_string(),
            },
            expected_generation: 0,
            expected_owner: None,
            owner: Some(ResourceOwner {
                manager: Manager::Terraform,
                id: Uuid::new_v4(),
                key: "firecrab_management_settings.host".into(),
            }),
        },
        target,
    };
    (directory, store, intent)
}

fn receipt(intent: &OwnerChange) -> Receipt {
    Receipt {
        operation_id: intent.change.operation_id,
        request_sha256: intent.request_sha256().unwrap(),
        target: intent.target.clone(),
        ownership: ResourceOwnership {
            endpoint_id: intent.change.expected_endpoint_id,
            resource: intent.change.resource.clone(),
            generation: intent.change.expected_generation
                + u64::from(intent.change.expected_owner != intent.change.owner),
            owner: intent.change.owner.clone(),
        },
        confirmed: false,
    }
}

#[test]
fn iac_native_facets_keep_separate_physical_realms_and_shared_address_reservations() {
    let (directory, store, management) = fixture();
    store.prepare_native_host_owner(management.clone()).unwrap();
    let mut configuration = management.clone();
    configuration.target.platform = Platform::Linux;
    configuration.target.installation_id = Uuid::new_v4();
    configuration.target.configuration_root = "/etc/firecrab".into();
    configuration.target.directory_id = "2:99:1234".into();
    configuration.change.operation_id = Uuid::new_v4();
    configuration.change.resource.kind = "host_configuration".into();
    configuration.change.resource.id = configuration.target.installation_id.to_string();
    assert!(matches!(
        store.prepare_native_host_owner(configuration.clone()),
        Err(Error::IacResourceBusy)
    ));
    configuration.change.owner.as_mut().unwrap().key = "firecrab_host_configuration.guest".into();
    store
        .prepare_native_host_owner(configuration.clone())
        .unwrap();
    store
        .check_iac_native_host("management_settings", &management.target)
        .unwrap();
    store
        .check_iac_native_host("host_configuration", &configuration.target)
        .unwrap();
    assert!(
        store
            .check_iac_native_host("management_settings", &configuration.target)
            .is_err()
    );
    assert!(
        store
            .check_iac_native_host("host_configuration", &management.target)
            .is_err()
    );
    drop(store);
    let store = Store::open(&directory.path().join("state.db")).unwrap();
    assert_eq!(
        store
            .native_host_owner_intent(management.change.operation_id)
            .unwrap(),
        Some(management.clone())
    );
    assert_eq!(
        store
            .native_host_owner_intent(configuration.change.operation_id)
            .unwrap(),
        Some(configuration.clone())
    );
    assert_eq!(
        store
            .commit_native_host_owner(&management.change, receipt(&management))
            .unwrap()
            .generation,
        1
    );
    assert_eq!(
        store
            .commit_native_host_owner(&configuration.change, receipt(&configuration))
            .unwrap()
            .generation,
        1
    );
}

#[test]
fn iac_native_legacy_binding_migration_retains_original_target_and_pending_intent() {
    let (directory, store, intent) = fixture();
    store.prepare_native_host_owner(intent.clone()).unwrap();
    {
        let connection = store.lock();
        connection
            .execute("DELETE FROM iac_native_host_bindings", [])
            .unwrap();
        connection.execute_batch("CREATE TABLE iac_native_host_binding(singleton INTEGER PRIMARY KEY,identity TEXT NOT NULL) STRICT;").unwrap();
        connection
            .execute(
                "INSERT INTO iac_native_host_binding VALUES(1,?1)",
                [serde_json::to_string(&intent.target).unwrap()],
            )
            .unwrap();
    }
    drop(store);
    let store = Store::open(&directory.path().join("state.db")).unwrap();
    store
        .check_iac_native_host("management_settings", &intent.target)
        .unwrap();
    let mut replaced = intent.target.clone();
    replaced.directory_id = "99:42".into();
    assert!(
        store
            .check_iac_native_host("management_settings", &replaced)
            .is_err()
    );
    assert_eq!(
        store
            .native_host_owner_intent(intent.change.operation_id)
            .unwrap(),
        Some(intent.clone())
    );
    assert_eq!(
        store.host_owner_status(intent.change.operation_id).unwrap(),
        (false, false)
    );
    assert!(iac_host::ensure_resource_idle(&store.lock(), &intent.change.resource).is_err());
    let leaked_legacy_table:bool=store.lock().query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='iac_native_host_binding')",[],|row|row.get(0)).unwrap();
    assert!(!leaked_legacy_table);
}

#[test]
fn iac_native_host_original_target_and_pending_receipt_survive_database_restart() {
    let (directory, store, intent) = fixture();
    store.prepare_native_host_owner(intent.clone()).unwrap();
    assert!(
        store
            .host_owner_intent(intent.change.operation_id)
            .unwrap()
            .is_none()
    );
    assert!(store.change_iac_resource_owner(&intent.change).is_err());
    assert!(iac_host::ensure_resource_idle(&store.lock(), &intent.change.resource).is_err());
    let mut forged = receipt(&intent);
    forged.target.directory_id = "1:43".into();
    assert!(
        store
            .commit_native_host_owner(&intent.change, forged)
            .is_err()
    );
    drop(store);
    let store = Store::open(&directory.path().join("state.db")).unwrap();
    assert_eq!(
        store
            .native_host_owner_intent(intent.change.operation_id)
            .unwrap(),
        Some(intent.clone())
    );
    let original = receipt(&intent);
    assert_eq!(
        store
            .commit_native_host_owner(&intent.change, original.clone())
            .unwrap(),
        original.ownership
    );
    assert!(iac_host::ensure_resource_idle(&store.lock(), &intent.change.resource).is_err());
    store
        .confirm_host_owner(intent.change.operation_id)
        .unwrap();
    assert!(iac_host::ensure_resource_idle(&store.lock(), &intent.change.resource).is_ok());
    assert_eq!(
        store.change_iac_resource_owner(&intent.change).unwrap(),
        original.ownership
    );
}

#[test]
fn iac_native_host_cannot_retarget_or_reuse_a_guest_host_address_reservation() {
    let (_directory, store, intent) = fixture();
    store.prepare_native_host_owner(intent.clone()).unwrap();
    let guest = firecrab_helper_protocol::host_iac::OwnerChange {
        host: HostIdentity {
            machine_id: "c".repeat(32),
            configuration_root: "/etc/firecrab".into(),
            device: 1,
            inode: 548,
        },
        configuration_sha256: "d".repeat(64),
        change: ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            resource: ManagedResource {
                kind: "ingress".into(),
                id: "listener".into(),
            },
            ..intent.change.clone()
        },
    };
    assert!(matches!(
        store.prepare_host_owner(guest.clone()),
        Err(Error::IacResourceBusy)
    ));
    let mut guest = guest;
    guest.change.owner.as_mut().unwrap().key = "firecrab_ingress.host".into();
    store.prepare_host_owner(guest.clone()).unwrap();
    store.check_iac_host(&guest.host).unwrap();
    store
        .check_iac_native_host("management_settings", &intent.target)
        .unwrap();
    let mut replacement = intent.target.clone();
    replacement.directory_id = "1:43".into();
    assert!(matches!(
        store.check_iac_native_host("management_settings", &replacement),
        Err(Error::IacEndpointConflict)
    ));
    let mut other_request = intent.clone();
    other_request.change.resource.kind = "host_configuration".into();
    assert!(store.prepare_native_host_owner(other_request).is_err());
}

#[test]
fn iac_native_host_releases_only_an_exact_durable_no_effect_result() {
    let (_directory, store, intent) = fixture();
    store.prepare_native_host_owner(intent.clone()).unwrap();
    let mut rejection = Rejection {
        operation_id: intent.change.operation_id,
        request_sha256: intent.request_sha256().unwrap(),
        target: intent.target.clone(),
    };
    rejection.request_sha256 = "f".repeat(64);
    assert!(
        store
            .reject_native_host_owner(&intent.change, rejection.clone())
            .is_err()
    );
    assert_eq!(
        store.host_owner_status(intent.change.operation_id).unwrap(),
        (false, false)
    );
    rejection.request_sha256 = intent.request_sha256().unwrap();
    store
        .reject_native_host_owner(&intent.change, rejection)
        .unwrap();
    assert_eq!(
        store.host_owner_status(intent.change.operation_id).unwrap(),
        (true, true)
    );
    assert!(iac_host::ensure_resource_idle(&store.lock(), &intent.change.resource).is_ok());
    assert!(
        store
            .commit_native_host_owner(&intent.change, receipt(&intent))
            .is_err()
    );
}

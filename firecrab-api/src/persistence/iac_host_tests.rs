use super::*;
use firecrab_stack::{ManagedResource, ResourceOwner};

fn fixture() -> (tempfile::TempDir, Store, OwnerChange) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(&directory.path().join("state.db")).unwrap();
    let intent = OwnerChange {
        host: HostIdentity {
            machine_id: "a".repeat(32),
            configuration_root: "/etc/firecrab".into(),
            device: 1,
            inode: 42,
        },
        configuration_sha256: "b".repeat(64),
        change: ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: store.iac_endpoint_id().unwrap(),
            resource: ManagedResource {
                kind: "ingress".into(),
                id: "listener".into(),
            },
            expected_generation: 0,
            expected_owner: None,
            owner: Some(ResourceOwner {
                manager: Manager::Terraform,
                id: Uuid::new_v4(),
                key: "firecrab_ingress.app".into(),
            }),
        },
    };
    (directory, store, intent)
}
fn receipt(intent: &OwnerChange) -> OwnerReceipt {
    OwnerReceipt {
        operation_id: intent.change.operation_id,
        request_sha256: intent.request_sha256().unwrap(),
        host: intent.host.clone(),
        ownership: ResourceOwnership {
            endpoint_id: intent.change.expected_endpoint_id,
            resource: intent.change.resource.clone(),
            generation: intent.change.expected_generation
                + u64::from(intent.change.owner != intent.change.expected_owner),
            owner: intent.change.owner.clone(),
        },
        confirmed: false,
    }
}
#[test]
fn iac_host_reservation_survives_restart_and_only_the_exact_target_result_can_commit() {
    let (directory, store, intent) = fixture();
    store.prepare_host_owner(intent.clone()).unwrap();
    assert!(!store.iac_host_kind_public_write_allowed("ingress").unwrap());
    assert!(
        store
            .iac_host_kind_public_write_allowed("named_tunnel")
            .unwrap()
    );
    assert!(store.change_iac_resource_owner(&intent.change).is_err());
    assert!(ensure_resource_idle(&store.lock(), &intent.change.resource).is_err());
    let mut forged = receipt(&intent);
    forged.host.inode += 1;
    assert!(store.commit_host_owner(&intent.change, forged).is_err());
    drop(store);
    let store = Store::open(&directory.path().join("state.db")).unwrap();
    assert_eq!(
        store
            .host_owner_intent(intent.change.operation_id)
            .unwrap()
            .unwrap(),
        intent
    );
    assert!(
        store
            .confirm_host_owner(intent.change.operation_id)
            .is_err()
    );
    let expected = receipt(&intent);
    assert_eq!(
        store
            .commit_host_owner(&intent.change, expected.clone())
            .unwrap(),
        expected.ownership
    );
    assert_eq!(
        store.host_owner_status(intent.change.operation_id).unwrap(),
        (false, false)
    );
    store
        .confirm_host_owner(intent.change.operation_id)
        .unwrap();
    assert_eq!(
        store.host_owner_status(intent.change.operation_id).unwrap(),
        (true, false)
    );
    assert!(ensure_resource_idle(&store.lock(), &intent.change.resource).is_ok());
    assert_eq!(
        store
            .commit_host_owner(&intent.change, expected.clone())
            .unwrap(),
        expected.ownership
    );
}
#[test]
fn iac_host_logical_address_and_physical_installation_are_reserved_before_remote_work() {
    let (_directory, store, intent) = fixture();
    store.prepare_host_owner(intent.clone()).unwrap();
    let mut competing = intent.clone();
    competing.change.operation_id = Uuid::new_v4();
    competing.change.resource.id = "route:app.example.com/".into();
    assert!(matches!(
        store.prepare_host_owner(competing.clone()),
        Err(Error::IacResourceBusy)
    ));
    assert!(matches!(
        ensure_available(&store.lock(), &competing.change),
        Err(Error::IacResourceBusy)
    ));
    competing.change.owner.as_mut().unwrap().key = "firecrab_ingress.other".into();
    competing.host.inode += 1;
    assert!(matches!(
        store.prepare_host_owner(competing),
        Err(Error::IacEndpointConflict)
    ));
    let mut changed = intent.clone();
    changed.change.owner = None;
    assert!(matches!(
        store.prepare_host_owner(changed),
        Err(Error::IacOperationIdentity)
    ));
}
#[test]
fn iac_host_only_a_durable_exact_no_effect_rejection_releases_the_api_reservation() {
    let (_directory, store, intent) = fixture();
    store.prepare_host_owner(intent.clone()).unwrap();
    let rejected = OwnerRejection {
        operation_id: intent.change.operation_id,
        request_sha256: intent.request_sha256().unwrap(),
        host: intent.host.clone(),
    };
    let mut wrong = rejected.clone();
    wrong.request_sha256 = "c".repeat(64);
    assert!(store.reject_host_owner(&intent.change, wrong).is_err());
    assert!(ensure_resource_idle(&store.lock(), &intent.change.resource).is_err());
    store.reject_host_owner(&intent.change, rejected).unwrap();
    assert_eq!(
        store.host_owner_status(intent.change.operation_id).unwrap(),
        (true, true)
    );
    assert!(ensure_resource_idle(&store.lock(), &intent.change.resource).is_ok());
    assert_eq!(
        store
            .iac_resource_owner(&intent.change.resource)
            .unwrap()
            .generation,
        0
    );
    let mut retry = intent;
    retry.change.operation_id = Uuid::new_v4();
    assert!(store.prepare_host_owner(retry).is_ok());
}
#[test]
fn iac_host_noop_release_has_an_immutable_zero_generation_receipt() {
    let (_directory, store, mut intent) = fixture();
    intent.change.owner = None;
    store.prepare_host_owner(intent.clone()).unwrap();
    let result = store
        .commit_host_owner(&intent.change, receipt(&intent))
        .unwrap();
    assert_eq!(result.generation, 0);
    assert_eq!(result.owner, None);
    assert_eq!(
        store
            .iac_ownership_operation(intent.change.operation_id)
            .unwrap()
            .unwrap()
            .ownership,
        result
    );
}

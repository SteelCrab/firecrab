use super::*;
use crate::{
    handlers::vms::test_support::{record, seed_vm, test_state},
    persistence::{PersistenceError, Store},
};
use firecrab_api_types::{MicroBlockStorageResponse, VmBlockStorageMount};

fn volume(state: &AppState, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    state
        .store
        .insert_block_storage(
            &MicroBlockStorageResponse {
                id,
                name: name.into(),
                size_gib: 1,
                storage_root: "default".into(),
                state: "creating".into(),
                attached_vm_id: None,
                mount_path: None,
            },
            None,
        )
        .unwrap();
    state.store.finish_block_storage_create(id).unwrap();
    id
}
fn target(volume: Uuid, vm: Uuid) -> ManagedResource {
    ManagedResource {
        kind: "volume_attachment".into(),
        id: format!("{volume}:{vm}"),
    }
}
fn own(state: &AppState, target: &ManagedResource) -> ResourceOwnership {
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(state, target)).unwrap();
    state.store.change_iac_resource_owner(&claim).unwrap()
}
fn release(store: &Store, owned: ResourceOwnership) {
    store
        .change_iac_resource_owner(&ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: owned.endpoint_id,
            resource: owned.resource,
            expected_generation: owned.generation,
            expected_owner: owned.owner,
            owner: None,
        })
        .unwrap();
}

#[tokio::test]
async fn iac_volume_attachment_transfer_preserves_disk_and_blocks_implicit_parent_delete() {
    let root = crate::firecracker::test_support::short_tempdir();
    let state = test_state(root.path()).await;
    let vm = record("attachment-owner", Uuid::new_v4());
    seed_vm(&state, &vm);
    let volume = volume(&state, "attachment-disk");
    state
        .store
        .attach_block_storages(
            vm.id,
            &[VmBlockStorageMount {
                volume_id: volume,
                mount_path: "/data".into(),
            }],
        )
        .unwrap();
    let disk = crate::block_storage::disk_path(&state, &state.store.block_storage(volume).unwrap())
        .unwrap();
    std::fs::create_dir_all(disk.parent().unwrap()).unwrap();
    std::fs::write(&disk, b"borrowed data retained").unwrap();
    let before = state.store.block_storage(volume).unwrap();
    let authority = state.store.attachment_authority(volume, vm.id).unwrap();
    let owned = own(&state, &target(volume, vm.id));
    assert_eq!(state.store.block_storage(volume).unwrap(), before);
    assert!(matches!(
        state.store.attachment_authority(volume, vm.id),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    assert!(matches!(
        state.store.detach_block_storage(volume, vm.id, &authority),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert!(matches!(
        state.store.attach_block_storages(
            vm.id,
            &[VmBlockStorageMount {
                volume_id: volume,
                mount_path: "/data".into()
            }]
        ),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    assert!(matches!(
        state.store.delete(vm.id),
        Err(PersistenceError::IacManagedDependent)
    ));
    assert!(matches!(
        state
            .store
            .begin_native_vm_delete(&vm, Uuid::new_v4(), false),
        Err(PersistenceError::IacManagedDependent)
    ));
    assert!(
        crate::handlers::vms::delete_vm(
            axum::extract::State(state.clone()),
            axum::Extension(crate::server::RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string())
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read(&disk).unwrap(), b"borrowed data retained");
    assert_eq!(state.store.block_storage(volume).unwrap(), before);
    release(&state.store, owned);
    let authority = state.store.attachment_authority(volume, vm.id).unwrap();
    state
        .store
        .detach_block_storage(volume, vm.id, &authority)
        .unwrap();
    assert!(
        state
            .store
            .block_storage(volume)
            .unwrap()
            .attached_vm_id
            .is_none()
    );
    assert_eq!(std::fs::read(&disk).unwrap(), b"borrowed data retained");
}

#[tokio::test]
async fn iac_volume_attachment_cannot_retarget_or_adopt_generated_or_busy_vm() {
    let root = crate::firecracker::test_support::short_tempdir();
    let state = test_state(root.path()).await;
    let mut vm = record("attachment-validation", Uuid::new_v4());
    seed_vm(&state, &vm);
    let volume = volume(&state, "identity-disk");
    state
        .store
        .attach_block_storages(
            vm.id,
            &[VmBlockStorageMount {
                volume_id: volume,
                mount_path: "/data".into(),
            }],
        )
        .unwrap();
    for resource in [
        target(volume, Uuid::new_v4()),
        target(Uuid::new_v4(), vm.id),
    ] {
        let claim: ChangeResourceOwner =
            serde_json::from_value(claim_configuration(&state, &resource)).unwrap();
        assert!(matches!(
            state.store.change_iac_resource_owner(&claim),
            Err(PersistenceError::InvalidStackResource)
        ));
    }
    vm.state = firecrab_api_types::VmState::Starting;
    state.store.update(&vm).unwrap();
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &target(volume, vm.id))).unwrap();
    assert!(matches!(
        state.store.change_iac_resource_owner(&claim),
        Err(PersistenceError::IacResourceBusy)
    ));
    vm.state = firecrab_api_types::VmState::Created;
    vm.purpose = crate::model::VmPurpose::Builder;
    state.store.update(&vm).unwrap();
    assert!(matches!(
        state.store.change_iac_resource_owner(&claim),
        Err(PersistenceError::InvalidStackResource)
    ));
}

#[tokio::test]
async fn iac_volume_attachment_late_attach_and_detach_cannot_cross_release_or_parent_transfer() {
    let root = crate::firecracker::test_support::short_tempdir();
    let state = test_state(root.path()).await;
    let vm = record("attachment-generation", Uuid::new_v4());
    seed_vm(&state, &vm);
    let volume = volume(&state, "generation-disk");
    let authority = state.store.attachment_authority(volume, vm.id).unwrap();
    state
        .store
        .attach_block_storage_checked(
            vm.id,
            VmBlockStorageMount {
                volume_id: volume,
                mount_path: "/data".into(),
            },
            &authority,
        )
        .unwrap();
    let before = state.store.block_storage(volume).unwrap();
    let second = Store::open(&root.path().join("data/firecrab.db")).unwrap();
    release(&second, own(&state, &target(volume, vm.id)));
    assert!(matches!(
        second.detach_block_storage(volume, vm.id, &authority),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert!(matches!(
        second.attach_block_storage_checked(
            vm.id,
            VmBlockStorageMount {
                volume_id: volume,
                mount_path: "/data".into()
            },
            &authority
        ),
        Err(PersistenceError::StaleIacOwner)
    ));
    let current = state.store.attachment_authority(volume, vm.id).unwrap();
    release(
        &second,
        own(
            &state,
            &ManagedResource {
                kind: "vm".into(),
                id: vm.id.to_string(),
            },
        ),
    );
    assert!(matches!(
        second.detach_block_storage(volume, vm.id, &current),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert_eq!(second.block_storage(volume).unwrap(), before);
}

#[tokio::test]
async fn iac_volume_attachment_rejects_adoption_during_durable_parent_cleanup() {
    let root = crate::firecracker::test_support::short_tempdir();
    let state = test_state(root.path()).await;
    let vm = record("attachment-delete-fence", Uuid::new_v4());
    seed_vm(&state, &vm);
    let volume = volume(&state, "delete-fenced-disk");
    state
        .store
        .attach_block_storages(
            vm.id,
            &[VmBlockStorageMount {
                volume_id: volume,
                mount_path: "/data".into(),
            }],
        )
        .unwrap();
    let intent = state
        .store
        .begin_native_vm_delete(&vm, Uuid::new_v4(), false)
        .unwrap();
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &target(volume, vm.id))).unwrap();
    assert!(matches!(
        state.store.change_iac_resource_owner(&claim),
        Err(PersistenceError::IacResourceBusy)
    ));
    let parent_claim: ChangeResourceOwner = serde_json::from_value(claim_configuration(
        &state,
        &ManagedResource {
            kind: "volume".into(),
            id: volume.to_string(),
        },
    ))
    .unwrap();
    assert!(matches!(
        state.store.change_iac_resource_owner(&parent_claim),
        Err(PersistenceError::IacResourceBusy)
    ));
    state.store.fail_native_vm_delete(&intent).unwrap();
    assert!(matches!(
        state.store.change_iac_resource_owner(&claim),
        Err(PersistenceError::IacResourceBusy)
    ));
}

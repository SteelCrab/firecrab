//! Real ext4 files with fake host networking; no KVM or host mounts are changed.

use super::block_storage::*;
use crate::block_storage;
use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::handlers::vms::test_support::{record, seed_vm, test_state};
use crate::server::RequestId;
use crate::state::AppState;
use axum::response::IntoResponse;
use axum::{
    Json,
    extract::{Extension, Path, State},
    http::StatusCode,
};
use firecrab_api_types::{
    AttachMicroBlockStorageRequest, CreateMicroBlockStorageRequest, MicroBlockStorageResponse,
    VmBlockStorageMount,
};
use std::os::fd::AsRawFd;
use uuid::Uuid;

fn request() -> Extension<RequestId> {
    Extension(RequestId(Uuid::new_v4()))
}

#[tokio::test]
async fn block_storage_vm_creation_reserves_disks_and_rolls_back_conflicts() {
    let temp = tempfile::Builder::new()
        .prefix("v247-")
        .tempdir_in("/tmp")
        .unwrap();
    let state = test_state(temp.path()).await;
    let (_, Json(network)) = crate::handlers::micro_networks::create_micro_network(
        State(state.clone()),
        request(),
        ValidatedJson(firecrab_api_types::CreateMicroNetworkRequest {
            name: "volume-network".into(),
            subnet_cidr: "172.30.247.0/24".into(),
            internet_enabled: false,
            uplink: None,
            ipv6_cidr: None,
            ipv6_address_mode: None,
        }),
    )
    .await
    .unwrap();
    let volume = volume(&state, "create-time").await;
    let mut vm_request: firecrab_api_types::CreateVmRequest =
        serde_json::from_value(serde_json::json!({
            "name": "volume-owner", "template": "ubuntu-rootfs-26.04", "cpu": 1, "ram": 128,
            "diskGb": 1, "microNetworkId": network.id,
            "blockStorages": [{ "volumeId": volume.id, "mountPath": "/data" }]
        }))
        .unwrap();
    let (_, Json(created)) = crate::handlers::vms::create_vm_route(
        State(state.clone()),
        request(),
        ValidatedJson(vm_request.clone()),
    )
    .await
    .unwrap();
    assert_eq!(
        state.store.block_storage(volume.id).unwrap().attached_vm_id,
        Some(created.id)
    );
    vm_request.name = "conflicting-owner".into();
    assert_eq!(
        crate::handlers::vms::create_vm_route(
            State(state.clone()),
            request(),
            ValidatedJson(vm_request)
        )
        .await
        .unwrap_err()
        .into_response()
        .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(state.store.load_all().unwrap().len(), 1);
    assert_eq!(state.vms.lock().unwrap().len(), 1);
}

async fn volume(state: &AppState, name: &str) -> MicroBlockStorageResponse {
    let (_, Json(volume)) = create(
        State(state.clone()),
        request(),
        ValidatedJson(CreateMicroBlockStorageRequest {
            name: name.into(),
            size_gib: 1,
            storage_root: None,
        }),
    )
    .await
    .unwrap();
    volume
}

async fn connect(
    state: &AppState,
    volume: Uuid,
    vm: Uuid,
    path: &str,
) -> Result<Json<MicroBlockStorageResponse>, AppError> {
    attach(
        State(state.clone()),
        request(),
        Path(volume.to_string()),
        ValidatedJson(AttachMicroBlockStorageRequest {
            vm_id: vm,
            mount_path: path.into(),
        }),
    )
    .await
}

#[tokio::test]
async fn block_storage_data_survives_vm_delete_reconnect_and_database_reopen() {
    let temp = tempfile::Builder::new()
        .prefix("v247-")
        .tempdir_in("/tmp")
        .unwrap();
    let state = test_state(temp.path()).await;
    let a = record("a", Uuid::new_v4());
    let b = record("b", Uuid::new_v4());
    seed_vm(&state, &a);
    seed_vm(&state, &b);
    let volume = volume(&state, "database").await;
    let disk = block_storage::disk_path(&state, &volume).unwrap();
    crate::rootfs::write_into_image(&disk, "/payload", b"persistent application data\n").unwrap();
    let _ = connect(&state, volume.id, a.id, "/data").await.unwrap();
    crate::handlers::vms::delete_vm(State(state.clone()), request(), Path(a.id.to_string()))
        .await
        .unwrap();
    assert!(disk.is_file());
    assert_eq!(
        state.store.block_storage(volume.id).unwrap().state,
        "available"
    );
    let _ = connect(&state, volume.id, b.id, "/var/lib/postgresql/data")
        .await
        .unwrap();
    let payload = crate::rootfs::run_debugfs(&disk, "cat /payload").unwrap();
    assert_eq!(payload.stdout, b"persistent application data\n");
    let reopened = crate::persistence::Store::open(&temp.path().join("data/firecrab.db")).unwrap();
    assert_eq!(
        reopened.block_storage(volume.id).unwrap().attached_vm_id,
        Some(b.id)
    );
    assert_eq!(reopened.vm_block_storages(b.id).unwrap().len(), 1);
}

#[tokio::test]
async fn block_storage_competing_attachments_and_live_vm_mutations_are_rejected() {
    let temp = tempfile::Builder::new()
        .prefix("v247-")
        .tempdir_in("/tmp")
        .unwrap();
    let state = test_state(temp.path()).await;
    let a = record("a", Uuid::new_v4());
    let b = record("b", Uuid::new_v4());
    seed_vm(&state, &a);
    seed_vm(&state, &b);
    let volume = volume(&state, "exclusive").await;
    let (first, second) = tokio::join!(
        connect(&state, volume.id, a.id, "/data"),
        connect(&state, volume.id, b.id, "/data"),
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    assert_eq!(
        delete(State(state.clone()), request(), Path(volume.id.to_string()))
            .await
            .unwrap_err()
            .into_response()
            .status(),
        StatusCode::CONFLICT
    );
    let owner = state
        .store
        .block_storage(volume.id)
        .unwrap()
        .attached_vm_id
        .unwrap();
    for status in [
        crate::model::VmState::Running,
        crate::model::VmState::Paused,
    ] {
        state.vms.lock().unwrap().get_mut(&owner).unwrap().state = status;
        assert_eq!(
            detach(State(state.clone()), request(), Path(volume.id.to_string()))
                .await
                .unwrap_err()
                .into_response()
                .status(),
            StatusCode::CONFLICT
        );
    }
    state.vms.lock().unwrap().get_mut(&owner).unwrap().state = crate::model::VmState::Stopped;
    let paths = crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for("default"), owner);
    paths.ensure_directories().unwrap();
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(paths.dir.join("vm.lock"))
        .unwrap();
    // SAFETY: the owned file descriptor remains open until the probe finishes.
    assert_eq!(
        unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    assert!(
        detach(State(state.clone()), request(), Path(volume.id.to_string()))
            .await
            .is_err()
    );
    assert!(
        crate::handlers::vms::delete_vm(State(state.clone()), request(), Path(owner.to_string()))
            .await
            .is_err()
    );
    assert!(
        crate::handlers::vms::start_vm_request(
            State(state.clone()),
            request(),
            Path(owner.to_string())
        )
        .await
        .is_err()
    );
    drop(held);
    let _ = detach(State(state.clone()), request(), Path(volume.id.to_string()))
        .await
        .unwrap();
    delete(State(state.clone()), request(), Path(volume.id.to_string()))
        .await
        .unwrap();
    assert!(state.store.block_storages().unwrap().is_empty());
    assert!(!block_storage::disk_path(&state, &volume).unwrap().exists());
}

#[tokio::test]
async fn block_storage_batch_rollback_mount_conflicts_and_interrupted_creation() {
    let temp = tempfile::Builder::new()
        .prefix("v247-")
        .tempdir_in("/tmp")
        .unwrap();
    let state = test_state(temp.path()).await;
    let a = record("a", Uuid::new_v4());
    let b = record("b", Uuid::new_v4());
    seed_vm(&state, &a);
    seed_vm(&state, &b);
    let first = volume(&state, "first").await;
    let second = volume(&state, "second").await;
    let _ = connect(&state, second.id, b.id, "/data").await.unwrap();
    assert!(
        state
            .store
            .attach_block_storages(
                a.id,
                &[
                    VmBlockStorageMount {
                        volume_id: first.id,
                        mount_path: "/one".into()
                    },
                    VmBlockStorageMount {
                        volume_id: second.id,
                        mount_path: "/two".into()
                    },
                ]
            )
            .is_err()
    );
    assert_eq!(
        state.store.block_storage(first.id).unwrap().state,
        "available"
    );
    assert!(
        connect(&state, first.id, b.id, "/data/nested")
            .await
            .is_err()
    );
    let mut pending = first.clone();
    pending.id = Uuid::new_v4();
    pending.name = "interrupted".into();
    state.store.insert_block_storage(&pending, None).unwrap();
    let mut formatted = pending.clone();
    formatted.id = Uuid::new_v4();
    formatted.name = "format-completed-before-crash".into();
    state.store.insert_block_storage(&formatted, None).unwrap();
    block_storage::create_disk(&state, &formatted).unwrap();
    block_storage::recover(&state).unwrap();
    assert!(state.store.block_storage(pending.id).is_err());
    assert_eq!(
        state.store.block_storage(formatted.id).unwrap().state,
        "available"
    );
    assert_eq!(
        state.store.block_storage(second.id).unwrap().attached_vm_id,
        Some(b.id)
    );
}

#[tokio::test]
async fn block_storage_pool_deletion_and_controller_owned_vm_attachment_are_rejected() {
    let temp = tempfile::Builder::new()
        .prefix("v247-")
        .tempdir_in("/tmp")
        .unwrap();
    let state = test_state(temp.path()).await;
    let pool_id = Uuid::new_v4();
    let path = temp.path().join("storage");
    std::fs::create_dir_all(&path).unwrap();
    state
        .store
        .insert_micro_storage(pool_id, "storage", path.to_str().unwrap())
        .unwrap();
    let (_, Json(volume)) = create(
        State(state.clone()),
        request(),
        ValidatedJson(CreateMicroBlockStorageRequest {
            name: "pool-volume".into(),
            size_gib: 1,
            storage_root: Some(pool_id.to_string()),
        }),
    )
    .await
    .unwrap();
    assert!(state.store.delete_micro_storage(pool_id).is_err());
    let mut vm = record("pool-vm", Uuid::new_v4());
    vm.purpose = crate::model::VmPurpose::Pool;
    seed_vm(&state, &vm);
    assert!(connect(&state, volume.id, vm.id, "/data").await.is_err());
    delete(State(state.clone()), request(), Path(volume.id.to_string()))
        .await
        .unwrap();
    state.store.delete_micro_storage(pool_id).unwrap();
}

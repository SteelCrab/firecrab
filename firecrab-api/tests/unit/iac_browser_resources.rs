//! Disposable ext4 and cleanup records for actual API/browser evidence; no guest is booted.
use super::*;
use crate::handlers::vms::test_support::{record, seed_vm};
use axum::{Extension, Json, extract::State};

pub(super) async fn seed_attached_volume(
    state: &AppState,
    root: &std::path::Path,
    owner: Uuid,
) -> (ManagedResource, ResourceOwnership, Value) {
    let (_, Json(volume)) = crate::handlers::block_storage::create(
        State(state.clone()),
        Extension(crate::server::RequestId(Uuid::new_v4())),
        crate::extract::ValidatedJson(firecrab_api_types::CreateMicroBlockStorageRequest {
            name: "application-data".into(),
            size_gib: 1,
            storage_root: None,
        }),
    )
    .await
    .unwrap();
    let vm = record("attachment-host", Uuid::new_v4());
    seed_vm(state, &vm);
    state
        .store
        .attach_block_storages(
            vm.id,
            &[firecrab_api_types::VmBlockStorageMount {
                volume_id: volume.id,
                mount_path: "/data".into(),
            }],
        )
        .unwrap();
    let disk = crate::block_storage::disk_path(state, &volume).unwrap();
    assert!(disk.starts_with(root));
    let marker = root.join("fixture-data.txt");
    std::fs::write(
        &marker,
        b"borrowed volume contents retained through ownership and VM cleanup",
    )
    .unwrap();
    let result = std::process::Command::new("debugfs")
        .args(["-w", "-R"])
        .arg(format!("write {} /evidence.txt", marker.display()))
        .arg(&disk)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let read = std::process::Command::new("debugfs")
        .args(["-R", "cat /evidence.txt"])
        .arg(&disk)
        .output()
        .unwrap();
    assert!(read.status.success());
    assert_eq!(read.stdout, std::fs::read(&marker).unwrap());
    let checksum = digest(&disk);
    let target = ManagedResource {
        kind: "volume_attachment".into(),
        id: format!("{}:{}", volume.id, vm.id),
    };
    let snapshot = state.store.iac_resource_owner(&target).unwrap();
    let owned = state
        .store
        .change_iac_resource_owner(&ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: snapshot.endpoint_id,
            resource: target,
            expected_generation: snapshot.generation,
            expected_owner: snapshot.owner,
            owner: Some(firecrab_stack::ResourceOwner {
                manager: firecrab_stack::Manager::Terraform,
                id: owner,
                key: "firecrab_volume_attachment.application".into(),
            }),
        })
        .unwrap();
    // The disk itself is still unowned here: this rejection isolates the attachment guard.
    let rejected = crate::handlers::vms::delete_vm(
        State(state.clone()),
        Extension(crate::server::RequestId(Uuid::new_v4())),
        axum::extract::Path(vm.id.to_string()),
    )
    .await
    .unwrap_err();
    use axum::response::IntoResponse;
    assert_eq!(rejected.into_response().status(), StatusCode::CONFLICT);
    assert_eq!(digest(&disk), checksum);
    (
        ManagedResource {
            kind: "volume".into(),
            id: volume.id.to_string(),
        },
        owned,
        json!({"vmId":vm.id,"volumeId":volume.id,"diskPath":disk,"sha256":checksum,"mountPath":"/data","attachmentOnlyParentRejection":true}),
    )
}

pub(super) fn seed_vm_cleanup(state: &AppState, root: &std::path::Path) -> Value {
    let vm = record("cleanup-proof", Uuid::new_v4());
    seed_vm(state, &vm);
    let artifacts =
        crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id);
    assert!(artifacts.dir.starts_with(root));
    std::fs::create_dir_all(&artifacts.disks).unwrap();
    std::fs::write(
        artifacts.disks.join("fixture.ext4"),
        b"disposable private VM artifact; no guest boot",
    )
    .unwrap();
    let stack = state.store.register_stack("cleanup-proof").unwrap();
    state
        .store
        .bind_stack_resource(
            &stack.name,
            &Binding {
                kind: ResourceKind::Vm,
                key: "cleanup".into(),
                resource_id: vm.id,
            },
        )
        .unwrap();
    let operation = Uuid::new_v4();
    // Inject only receipt failure. The request still runs the actual filesystem cleanup.
    rusqlite::Connection::open(root.join("state.db"))
        .unwrap()
        .execute_batch(&format!(
            "CREATE TRIGGER reject_qa_vm_receipt BEFORE UPDATE ON iac_native_operations
        WHEN NEW.resource_id='{}' AND NEW.kind='vm' AND NEW.state='succeeded'
        BEGIN SELECT RAISE(ABORT,'QA receipt interruption'); END",
            vm.id
        ))
        .unwrap();
    json!({"vmId":vm.id,"operationId":operation,"stackId":stack.id,"artifacts":artifacts.dir,"databasePath":root.join("state.db")})
}

fn digest(path: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut input = std::fs::File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut chunk = [0; 64 * 1024];
    loop {
        let n = input.read(&mut chunk).unwrap();
        if n == 0 {
            break;
        }
        hash.update(&chunk[..n]);
    }
    format!("{:x}", hash.finalize())
}

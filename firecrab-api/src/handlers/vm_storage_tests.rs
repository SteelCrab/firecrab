use super::*;
use crate::{
    artifacts::VmArtifactPaths,
    handlers::vms::test_support::{record, seed_vm, test_state},
    persistence::Store,
    server::{HttpConfig, build_router},
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use firecrab_stack::{NativeOperation, NativeOperationState};
use serde_json::{Value, json};
use std::{fs, path::Path, time::Duration};
use tower::ServiceExt;

fn router(state: &AppState) -> Router {
    build_router(
        state.clone(),
        &HttpConfig {
            bind_addr: "127.0.0.1:5523".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 32,
            request_timeout: Duration::from_secs(10),
            static_root: None,
        },
    )
    .layer(Extension(crate::ai_sandbox::access::LocalOperator))
}

async fn fixture(
    root: &Path,
    pinned: bool,
    retain: bool,
) -> (
    AppState,
    Router,
    crate::model::VmRecord,
    NativeOperation,
    VmArtifactPaths,
) {
    let state = test_state(root).await;
    let mut vm = record("physical-cleanup", Uuid::new_v4());
    vm.disk_generation = Some(Uuid::new_v4());
    seed_vm(&state, &vm);
    let vm = state.store.load_all().unwrap().remove(&vm.id).unwrap();
    let paths = VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id);
    paths.ensure_directories().unwrap();
    fs::write(paths.rootfs(vm.disk_generation.unwrap()), b"original-data").unwrap();
    let id = Uuid::new_v4();
    let intent = if pinned {
        let proof = VmCleanupStorage::capture(root, vm.id, vm.disk_generation).unwrap();
        state
            .store
            .begin_vm_delete_pinned(&vm, id, retain, None, proof)
            .unwrap()
    } else {
        state.store.begin_native_vm_delete(&vm, id, retain).unwrap()
    };
    state.store.fail_native_vm_delete(&intent).unwrap();
    let operation = state.store.iac_native_operation(id).unwrap().unwrap();
    let app = router(&state);
    (state, app, vm, operation, paths)
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    operation: &NativeOperation,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .header(
                    "x-firecrab-endpoint-id",
                    operation.ownership.endpoint_id.to_string(),
                )
                .header(
                    "x-firecrab-execution-generation",
                    operation.execution_generation.to_string(),
                )
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}

#[tokio::test]
async fn iac_vm_storage_legacy_review_is_explicit_fences_old_attempt_and_recovers_original_operation()
 {
    let root = crate::firecracker::test_support::short_tempdir();
    let (state, app, vm, operation, paths) = fixture(root.path(), false, false).await;
    let path = format!(
        "/api/iac/native-operations/{}/vm-storage",
        operation.operation_id
    );
    let recovery = format!(
        "/api/iac/native-operations/{}/recover",
        operation.operation_id
    );
    let (status, error) = request(&app, "POST", &recovery, json!({}), &operation).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "vm_cleanup_storage_review_required");
    assert_eq!(
        state
            .store
            .iac_native_operation(operation.operation_id)
            .unwrap()
            .unwrap(),
        operation
    );
    let (status, observed) = request(&app, "GET", &path, Value::Null, &operation).await;
    assert_eq!(status, StatusCode::OK, "{observed}");
    assert_eq!(observed["reviewRequired"], true);
    assert!(observed["approved"].is_null());
    let mut body = json!({"expectedEndpointId":operation.ownership.endpoint_id,"expectedExecutionGeneration":0,
        "physicalSha256":observed["candidate"]["physicalSha256"],"confirmOriginalStorage":false});
    assert_eq!(
        request(&app, "POST", &path, body.clone(), &operation)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    body["confirmOriginalStorage"] = json!(true);
    let mut wrong = body.clone();
    wrong["physicalSha256"] = json!("0".repeat(64));
    assert_eq!(
        request(&app, "POST", &path, wrong, &operation).await.0,
        StatusCode::CONFLICT
    );
    let (status, receipt) = request(&app, "POST", &path, body.clone(), &operation).await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(receipt["nextExecutionGeneration"], 1);
    assert!(paths.dir.exists());
    let reviewed = state
        .store
        .iac_native_operation(operation.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(reviewed.state, NativeOperationState::NeedsRecovery);
    assert_eq!(reviewed.execution_generation, 1);
    assert!(
        state
            .store
            .recover_native_vm_delete_at(operation.operation_id, 0)
            .is_err()
    );
    let (status, recovered) = request(&app, "POST", &recovery, json!({}), &reviewed).await;
    assert_eq!(status, StatusCode::OK, "{recovered}");
    assert_eq!(recovered["state"], "succeeded");
    assert_eq!(recovered["operationId"], json!(operation.operation_id));
    assert!(!paths.dir.exists());
    assert!(!state.store.load_all().unwrap().contains_key(&vm.id));
    // An unrelated directory at that retired UUID can never be removed by an old receipt retry.
    fs::create_dir(&paths.dir).unwrap();
    fs::write(paths.dir.join("replacement"), b"retain-new-data").unwrap();
    crate::handlers::vms::resume_cluster_delete(
        &state,
        vm.id,
        &vm.storage_root,
        RequestId(Uuid::new_v4()),
    )
    .await
    .unwrap();
    assert_eq!(
        fs::read(paths.dir.join("replacement")).unwrap(),
        b"retain-new-data"
    );
    let (status, replayed) = request(&app, "POST", &path, body.clone(), &operation).await;
    assert_eq!(status, StatusCode::OK, "{replayed}");
    assert_eq!(replayed, receipt);
    assert_eq!(
        fs::read(paths.dir.join("replacement")).unwrap(),
        b"retain-new-data"
    );
    let reopened = Store::open(&root.path().join("data/firecrab.db")).unwrap();
    assert_eq!(
        reopened
            .vm_delete_storage_review_receipt(
                operation.operation_id,
                &serde_json::from_value(body.clone()).unwrap()
            )
            .unwrap()
            .unwrap(),
        receipt
    );
    let mut changed = body;
    changed["expectedExecutionGeneration"] = json!(1);
    assert_eq!(
        request(&app, "POST", &path, changed, &reviewed).await.0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn iac_vm_storage_remapped_root_and_replaced_directory_never_receive_recovered_cleanup() {
    for mode in ["root", "directory"] {
        let root = crate::firecracker::test_support::short_tempdir();
        let (state, app, vm, operation, paths) = fixture(root.path(), true, false).await;
        let recovery = format!(
            "/api/iac/native-operations/{}/recover",
            operation.operation_id
        );
        let other = root.path().join("other-root");
        fs::create_dir(&other).unwrap();
        let foreign = if mode == "root" {
            VmArtifactPaths::for_vm(&other.join("vms"), vm.id).dir
        } else {
            fs::rename(&paths.dir, root.path().join("original-vm")).unwrap();
            paths.dir.clone()
        };
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("foreign-data"), b"retain").unwrap();
        let remapped = if mode == "root" {
            router(
                &state
                    .clone()
                    .with_test_storage(crate::storage::StorageRegistry::single("default", other)),
            )
        } else {
            app.clone()
        };
        let (status, error) = request(&remapped, "POST", &recovery, json!({}), &operation).await;
        assert_eq!(status, StatusCode::CONFLICT, "{mode}: {error}");
        assert_eq!(error["error"]["code"], "vm_cleanup_storage_changed");
        assert_eq!(fs::read(foreign.join("foreign-data")).unwrap(), b"retain");
        assert_eq!(
            state
                .store
                .iac_native_operation(operation.operation_id)
                .unwrap()
                .unwrap(),
            operation
        );
        if mode == "directory" {
            fs::rename(&foreign, root.path().join("unrelated-retained-data")).unwrap();
            fs::rename(root.path().join("original-vm"), &paths.dir).unwrap();
        }
        let (status, result) = request(&app, "POST", &recovery, json!({}), &operation).await;
        assert_eq!(status, StatusCode::OK, "{mode}: {result}");
        assert_eq!(result["state"], "succeeded");
    }
}

#[tokio::test]
async fn iac_vm_storage_retention_keeps_bytes_and_discoverable_generation_after_vm_removal() {
    let root = crate::firecracker::test_support::short_tempdir();
    let (state, app, vm, operation, paths) = fixture(root.path(), true, true).await;
    let recovery = format!(
        "/api/iac/native-operations/{}/recover",
        operation.operation_id
    );
    let (status, result) = request(&app, "POST", &recovery, json!({}), &operation).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["state"], "succeeded");
    assert_eq!(
        fs::read(paths.rootfs(vm.disk_generation.unwrap())).unwrap(),
        b"original-data"
    );
    assert!(!state.store.load_all().unwrap().contains_key(&vm.id));
    let (status, observed) = request(
        &app,
        "GET",
        &format!(
            "/api/iac/native-operations/{}/vm-storage",
            operation.operation_id
        ),
        Value::Null,
        &operation,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{observed}");
    assert_eq!(
        observed["approved"]["diskGeneration"],
        json!(vm.disk_generation)
    );
    assert_eq!(observed["approved"]["retainDisks"], true);
    assert_eq!(observed["reviewRequired"], false);
    assert!(!observed.to_string().contains(root.path().to_str().unwrap()));
}

#[tokio::test]
async fn iac_vm_storage_disk_generation_change_cannot_retarget_an_original_attempt() {
    let root = crate::firecracker::test_support::short_tempdir();
    let (state, app, vm, operation, paths) = fixture(root.path(), true, false).await;
    let db = rusqlite::Connection::open(root.path().join("data/firecrab.db")).unwrap();
    db.execute(
        "UPDATE vms SET disk_generation=?2 WHERE id=?1",
        rusqlite::params![vm.id.to_string(), Uuid::new_v4().to_string()],
    )
    .unwrap();
    let (status, error) = request(
        &app,
        "POST",
        &format!(
            "/api/iac/native-operations/{}/recover",
            operation.operation_id
        ),
        json!({}),
        &operation,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert!(paths.dir.exists());
    assert_eq!(
        state
            .store
            .iac_native_operation(operation.operation_id)
            .unwrap()
            .unwrap(),
        operation
    );
}

#[tokio::test]
async fn iac_vm_storage_cleanup_holds_registered_root_ownership_until_completion() {
    let root = crate::firecracker::test_support::short_tempdir();
    let state = test_state(root.path()).await;
    let disk = root.path().join("registered-disk");
    fs::create_dir(&disk).unwrap();
    let storage = Uuid::new_v4();
    state
        .store
        .insert_micro_storage(storage, "registered", disk.to_str().unwrap())
        .unwrap();
    let mut vm = record("registered-cleanup", Uuid::new_v4());
    vm.storage_root = storage.to_string();
    seed_vm(&state, &vm);
    let paths = VmArtifactPaths::for_vm(&disk.join("vms"), vm.id);
    paths.ensure_directories().unwrap();
    fs::write(paths.dir.join("retained"), b"keep").unwrap();
    let proof = VmCleanupStorage::capture(&disk, vm.id, None).unwrap();
    let intent = state
        .store
        .begin_vm_delete_pinned(&vm, Uuid::new_v4(), true, None, proof)
        .unwrap();
    state.store.fail_native_vm_delete(&intent).unwrap();
    let change = firecrab_stack::ChangeResourceOwner {
        operation_id: Uuid::new_v4(),
        expected_endpoint_id: intent.operation.ownership.endpoint_id,
        resource: firecrab_stack::ManagedResource {
            kind: "storage_root".into(),
            id: storage.to_string(),
        },
        expected_generation: 0,
        expected_owner: None,
        owner: Some(firecrab_stack::ResourceOwner {
            manager: firecrab_stack::Manager::Terraform,
            id: Uuid::new_v4(),
            key: "firecrab_storage_root.disk".into(),
        }),
    };
    assert!(matches!(
        state.store.change_iac_resource_owner(&change),
        Err(crate::persistence::PersistenceError::IacResourceBusy)
    ));
    let operation = state
        .store
        .iac_native_operation(intent.operation.operation_id)
        .unwrap()
        .unwrap();
    let (status, result) = request(
        &router(&state),
        "POST",
        &format!(
            "/api/iac/native-operations/{}/recover",
            operation.operation_id
        ),
        json!({}),
        &operation,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["state"], "succeeded");
    assert_eq!(
        state
            .store
            .change_iac_resource_owner(&change)
            .unwrap()
            .generation,
        1
    );
    assert_eq!(fs::read(paths.dir.join("retained")).unwrap(), b"keep");
}

#[test]
fn iac_vm_storage_schema_upgrade_preserves_unknown_legacy_input_and_original_receipt() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("legacy.sqlite");
    let store = Store::open(&path).unwrap();
    let vm = record("legacy-schema", Uuid::new_v4());
    store.insert(&vm).unwrap();
    let intent = store
        .begin_native_vm_delete(&vm, Uuid::new_v4(), true)
        .unwrap();
    store.fail_native_vm_delete(&intent).unwrap();
    let original = store
        .iac_native_operation(intent.operation.operation_id)
        .unwrap()
        .unwrap();
    drop(store);
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=OFF;BEGIN IMMEDIATE;
        ALTER TABLE iac_vm_deletes RENAME TO new_deletes;
        CREATE TABLE iac_vm_deletes(operation_id TEXT PRIMARY KEY REFERENCES iac_native_operations(operation_id),storage_root TEXT NOT NULL,retain_disks INTEGER NOT NULL CHECK(retain_disks IN (0,1))) STRICT;
        INSERT INTO iac_vm_deletes SELECT operation_id,storage_root,retain_disks FROM new_deletes;
        DROP TABLE new_deletes;COMMIT;").unwrap();
    drop(conn);
    let reopened = Store::open(&path).unwrap();
    let restored = reopened
        .native_vm_delete(original.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(restored.operation, original);
    assert!(restored.storage_proof().is_none());
    assert!(restored.retain_disks);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let missing:(Option<String>,Option<i64>)=conn.query_row("SELECT storage_pin,storage_review_generation FROM iac_vm_deletes WHERE operation_id=?1",[original.operation_id.to_string()],|row|Ok((row.get(0)?,row.get(1)?))).unwrap();
    assert_eq!(missing, (None, None));
}

//! SQLite and HTTP lifecycle tests use synthetic volume headers, not a live guest mount.
use super::*;
use axum::{
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::io::{Seek, SeekFrom};
use tower::ServiceExt;

pub(super) fn registration() -> Binding {
    Binding {
        project_id: Uuid::new_v4(),
        database_id: Uuid::new_v4(),
        volume_id: Uuid::new_v4(),
        vm_id: Uuid::new_v4(),
        generation: 1,
        provisioning: true,
        verified_runtime_id: None,
    }
}
#[test]
fn private_database_survives_reopen_and_rejects_stale_writes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state").join("projects");
    let mut b = registration();
    let db = db::ProjectDb::open(&root, &b, Uuid::new_v4(), true).unwrap();
    assert_eq!(
        db.conn
            .pragma_query_value::<String, _>(None, "journal_mode", |r| r.get(0))
            .unwrap(),
        "wal"
    );
    assert_eq!(
        db.conn
            .pragma_query_value::<u32, _>(None, "synchronous", |r| r.get(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.conn
            .pragma_query_value::<u32, _>(None, "foreign_keys", |r| r.get(0))
            .unwrap(),
        1
    );
    db.save_document(0, &json!({"name":"retained","layout":["terminal"]}))
        .unwrap();
    assert!(matches!(
        db.save_document(0, &json!({})),
        Err(Error::Conflict(_))
    ));
    assert!(matches!(
        db::ProjectDb::open(&root, &b, Uuid::new_v4(), true),
        Err(Error::Busy)
    ));
    drop(db);
    b.provisioning = false;
    let db = db::ProjectDb::open(&root, &b, Uuid::new_v4(), true).unwrap();
    assert_eq!(db.document().unwrap().0, 1);
    assert_eq!(db.document().unwrap().1["name"], "retained");
    db.conn
        .execute("UPDATE identity SET epoch=epoch+1", [])
        .unwrap();
    assert!(matches!(
        db.save_document(1, &json!({})),
        Err(Error::Conflict(_))
    ));
}
#[test]
fn missing_or_swapped_database_is_never_recreated() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state").join("projects");
    let mut b = registration();
    let db = db::ProjectDb::open(&root, &b, Uuid::new_v4(), true).unwrap();
    let path = db.directory.join("project.db");
    drop(db);
    b.provisioning = false;
    fs::remove_file(&path).unwrap();
    assert!(matches!(
        db::ProjectDb::open(&root, &b, Uuid::new_v4(), true),
        Err(Error::Conflict(_))
    ));
    assert!(!path.exists());
    b.provisioning = true;
    let db = db::ProjectDb::open(&root, &b, Uuid::new_v4(), true).unwrap();
    drop(db);
    let mut swapped = b.clone();
    swapped.database_id = Uuid::new_v4();
    assert!(matches!(
        db::ProjectDb::open(&root, &swapped, Uuid::new_v4(), true),
        Err(Error::Conflict(_))
    ));
    let db = db::ProjectDb::open(&root, &b, Uuid::new_v4(), true).unwrap();
    db.conn
        .pragma_update(None, "user_version", db::SCHEMA_VERSION + 1)
        .unwrap();
    drop(db);
    assert!(matches!(
        db::ProjectDb::open(&root, &b, Uuid::new_v4(), true),
        Err(Error::Conflict(_))
    ));
}
#[test]
fn database_rejects_links_and_oversized_documents() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state").join("projects");
    let b = registration();
    let db = db::ProjectDb::open(&root, &b, Uuid::new_v4(), true).unwrap();
    assert_eq!(
        fs::metadata(db.directory.join("project.db"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(matches!(
        db.save_document(0, &json!("x".repeat(db::DOCUMENT_LIMIT))),
        Err(Error::Invalid(_))
    ));
    let path = db.directory.join("project.db");
    drop(db);
    fs::rename(&path, dir.path().join("other.db")).unwrap();
    symlink(dir.path().join("other.db"), &path).unwrap();
    assert!(db::ProjectDb::open(&root, &b, Uuid::new_v4(), true).is_err());
}

async fn reopen_state(root: &std::path::Path) -> AppState {
    let templates =
        crate::templates::TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
    AppState::with_db_file(templates, root.join("control.db"))
        .await
        .unwrap()
}
pub(super) async fn fixture() -> (tempfile::TempDir, AppState, Binding) {
    let dir = tempfile::tempdir().unwrap();
    let state = reopen_state(dir.path()).await;
    let b = registration();
    let pool = Uuid::new_v4();
    let storage = dir.path().join("storage");
    fs::create_dir(&storage).unwrap();
    state
        .store
        .insert_micro_storage(pool, "phase1", storage.to_str().unwrap())
        .unwrap();
    let mut vm = crate::handlers::vms::test_support::record("phase1", b.vm_id);
    vm.storage_root = pool.to_string();
    crate::handlers::vms::test_support::seed_vm(&state, &vm);
    let spec: SandboxProjectSpec =
        serde_json::from_value(json!({"name":"Phase 1","vmId":b.vm_id})).unwrap();
    state
        .store
        .save_sandbox_project(&SandboxProject {
            id: b.project_id,
            owner: SandboxPrincipal::Local,
            spec,
            created_at: 1,
        })
        .unwrap();
    let volume = MicroBlockStorageResponse {
        id: b.volume_id,
        name: "phase1".into(),
        size_gib: 1,
        storage_root: pool.to_string(),
        state: "creating".into(),
        attached_vm_id: None,
        mount_path: None,
    };
    state
        .store
        .insert_block_storage(&volume, Some(pool))
        .unwrap();
    let disk = block_storage::disk_path(&state, &volume).unwrap();
    fs::create_dir_all(disk.parent().unwrap()).unwrap();
    let mut file = File::create(&disk).unwrap();
    file.set_len(1 << 30).unwrap();
    file.seek(SeekFrom::Start(1024 + 56)).unwrap();
    file.write_all(&[0x53, 0xef]).unwrap();
    file.seek(SeekFrom::Start(1024 + 104)).unwrap();
    file.write_all(volume.id.as_bytes()).unwrap();
    file.sync_all().unwrap();
    state
        .store
        .finish_block_storage_create(b.volume_id)
        .unwrap();
    (dir, state, b)
}
fn router(state: &AppState) -> Router {
    routes()
        .with_state(state.clone())
        .layer(Extension(RequestId(Uuid::new_v4())))
        .layer(Extension(crate::ai_sandbox::access::LocalOperator))
}
pub(super) async fn call(
    state: &AppState,
    method: &str,
    path: &str,
    body: Value,
    host: Option<Uuid>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    if let Some(host_id) = host {
        request
            .extensions_mut()
            .insert(ConnectInfo(crate::tls::AuthenticatedHost { host_id }));
    }
    let output = router(state).oneshot(request).await.unwrap();
    let status = output.status();
    let data = to_bytes(output.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&data).unwrap())
}
pub(super) async fn bind(state: &AppState, b: &Binding) -> Value {
    let (status, value) = call(
        state,
        "PUT",
        &format!("/api/ai-sandbox/projects/{}/workspace", b.project_id),
        json!({"volumeId":b.volume_id}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}
#[tokio::test]
async fn api_metadata_and_volume_binding_survive_controller_reopen() {
    let (dir, state, b) = fixture().await;
    let first = bind(&state, &b).await;
    assert_eq!(first["status"], "stopped");
    let second = bind(&state, &b).await;
    assert_eq!(first["databaseId"], second["databaseId"]);
    assert_eq!(first["generation"], second["generation"]);
    let path = format!("/api/ai-sandbox/projects/{}/workspace", b.project_id);
    let (status, saved) = call(
        &state,
        "PUT",
        &(path.clone() + "/state"),
        json!({"expectedRevision":0,"document":{"draft":"retained"}}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        call(
            &state,
            "PUT",
            &(path.clone() + "/state"),
            json!({"expectedRevision":0,"document":{}}),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    drop(state);
    let reopened = reopen_state(dir.path()).await;
    let (status, value) = call(&reopened, "GET", &path, json!({}), None).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["document"]["draft"], "retained");
    assert_eq!(value["databaseId"], first["databaseId"]);
    assert_eq!(value["volumeId"], first["volumeId"]);
}
#[tokio::test]
async fn project_access_is_checked_before_private_database_access() {
    let (_dir, state, b) = fixture().await;
    bind(&state, &b).await;
    let host = Uuid::new_v4();
    let path = format!("/api/ai-sandbox/projects/{}/workspace", b.project_id);
    assert_eq!(
        call(&state, "GET", &path, json!({}), Some(host)).await.0,
        StatusCode::FORBIDDEN
    );
    state
        .store
        .save_sandbox_access(
            host,
            &SandboxAccessPolicy {
                grants: vec![SandboxResourceGrant {
                    resource: SandboxResource::Project { id: b.project_id },
                    permissions: vec![SandboxPermission::Read],
                }],
            },
        )
        .unwrap();
    assert_eq!(
        call(&state, "GET", &path, json!({}), Some(host)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &state,
            "PUT",
            &(path.clone() + "/state"),
            json!({"expectedRevision":0,"document":{}}),
            Some(host)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &state,
            "PUT",
            &path,
            json!({"volumeId":b.volume_id}),
            Some(host)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let another = format!("/api/ai-sandbox/projects/{}/workspace", Uuid::new_v4());
    assert_eq!(
        call(&state, "GET", &another, json!({}), Some(host)).await.0,
        StatusCode::FORBIDDEN
    );
}
#[tokio::test]
async fn retained_workspace_prevents_project_volume_deletion_and_direct_detach() {
    let (_dir, state, b) = fixture().await;
    bind(&state, &b).await;
    assert!(state.store.delete_sandbox_project(b.project_id).is_err());
    assert!(state.store.begin_block_storage_delete(b.volume_id).is_err());
    assert!(
        state
            .store
            .detach_block_storage(b.volume_id, b.vm_id)
            .is_err()
    );
    assert!(
        state
            .store
            .attach_block_storages(
                b.vm_id,
                &[VmBlockStorageMount {
                    volume_id: b.volume_id,
                    mount_path: "/wrong".into()
                }]
            )
            .is_err()
    );
    let binding = state
        .store
        .project_workspace(b.project_id)
        .unwrap()
        .unwrap();
    let db = open(&state, &binding).unwrap();
    let path = db.directory.join("project.db");
    drop(db);
    fs::remove_file(&path).unwrap();
    let response = call(
        &state,
        "GET",
        &format!("/api/ai-sandbox/projects/{}/workspace", b.project_id),
        json!({}),
        None,
    )
    .await;
    assert_eq!(response.0, StatusCode::CONFLICT);
    assert!(!path.exists());
}
#[tokio::test]
async fn lost_volume_and_new_runtime_are_not_reported_ready() {
    let (_dir, state, b) = fixture().await;
    bind(&state, &b).await;
    let binding = state
        .store
        .project_workspace(b.project_id)
        .unwrap()
        .unwrap();
    let db = open(&state, &binding).unwrap();
    db.mark_initialized().unwrap();
    let runtime = Uuid::new_v4();
    state
        .store
        .verify_project_workspace(&binding, runtime)
        .unwrap();
    {
        let mut vms = state.vms.lock().unwrap();
        let vm = vms.get_mut(&b.vm_id).unwrap();
        vm.state = VmState::Running;
        vm.last_runtime_id = Some(runtime);
    }
    let binding = state
        .store
        .project_workspace(b.project_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        response(&state, &binding, &db).unwrap().status,
        ProjectWorkspaceStatus::Ready
    );
    state
        .vms
        .lock()
        .unwrap()
        .get_mut(&b.vm_id)
        .unwrap()
        .last_runtime_id = Some(Uuid::new_v4());
    assert_eq!(
        response(&state, &binding, &db).unwrap().status,
        ProjectWorkspaceStatus::AwaitingMount
    );
    let volume = state.store.block_storage(b.volume_id).unwrap();
    fs::remove_file(block_storage::disk_path(&state, &volume).unwrap()).unwrap();
    assert_eq!(
        response(&state, &binding, &db).unwrap().status,
        ProjectWorkspaceStatus::Unavailable
    );
}
#[tokio::test]
async fn checkpoint_is_matched_idempotent_and_keeps_partial_failures_incomplete() {
    let (_dir, state, b) = fixture().await;
    bind(&state, &b).await;
    let binding = state
        .store
        .project_workspace(b.project_id)
        .unwrap()
        .unwrap();
    let db = open(&state, &binding).unwrap();
    db.save_document(0, &json!({"checkpoint":"retained"}))
        .unwrap();
    db.mark_initialized().unwrap();
    drop(db);
    let id = Uuid::new_v4();
    let path = format!(
        "/api/ai-sandbox/projects/{}/workspace/checkpoints/{id}",
        b.project_id
    );
    let (status, created) = call(&state, "PUT", &path, json!({}), None).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let manifest: ProjectWorkspaceCheckpoint = serde_json::from_value(created.clone()).unwrap();
    let (status, retried) = call(&state, "PUT", &path, json!({}), None).await;
    assert_eq!(status, StatusCode::OK, "{retried}");
    assert_eq!(retried, created);
    let reader = Uuid::new_v4();
    assert_eq!(
        call(&state, "GET", &path, json!({}), Some(reader)).await.0,
        StatusCode::FORBIDDEN
    );
    state
        .store
        .save_sandbox_access(
            reader,
            &SandboxAccessPolicy {
                grants: vec![SandboxResourceGrant {
                    resource: SandboxResource::Project { id: b.project_id },
                    permissions: vec![SandboxPermission::Read],
                }],
            },
        )
        .unwrap();
    let (status, fetched) = call(&state, "GET", &path, json!({}), Some(reader)).await;
    assert_eq!(status, StatusCode::OK, "{fetched}");
    assert_eq!(fetched, created);
    assert_eq!(
        call(&state, "PUT", &path, json!({}), Some(reader)).await.0,
        StatusCode::FORBIDDEN
    );
    let missing = format!(
        "/api/ai-sandbox/projects/{}/workspace/checkpoints/{}",
        b.project_id,
        Uuid::new_v4()
    );
    assert_eq!(
        call(&state, "GET", &missing, json!({}), None).await.0,
        StatusCode::NOT_FOUND
    );
    let db = open(&state, &binding).unwrap();
    let saved = db.directory.join("checkpoints").join(id.to_string());
    let restored = rusqlite::Connection::open(saved.join("project.db")).unwrap();
    let json: String = restored
        .query_row("SELECT document FROM identity", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&json).unwrap()["checkpoint"],
        "retained"
    );
    assert_eq!(
        checkpoint::digest(&saved.join("data.ext4")).unwrap(),
        manifest.volume_sha256
    );
    let partial = Uuid::new_v4();
    fs::create_dir(db.directory.join("checkpoints").join(partial.to_string())).unwrap();
    assert!(matches!(
        checkpoint::create_checkpoint(&state, &binding, &db, partial),
        Err(Error::Conflict(_))
    ));
    let mut disk = OpenOptions::new()
        .write(true)
        .open(saved.join("data.ext4"))
        .unwrap();
    disk.write_all(b"corrupt").unwrap();
    assert!(matches!(
        checkpoint::create_checkpoint(&state, &binding, &db, id),
        Err(Error::Conflict(_))
    ));
}

#[tokio::test]
async fn runtime_replacement_keeps_database_and_volume_and_rejects_stale_generation() {
    let (_dir, state, b) = fixture().await;
    let original = bind(&state, &b).await;
    let binding = state
        .store
        .project_workspace(b.project_id)
        .unwrap()
        .unwrap();
    let db = open(&state, &binding).unwrap();
    db.save_document(0, &json!({"retained":"across VM replacement"}))
        .unwrap();
    drop(db);
    let old = state.vms.lock().unwrap().get(&b.vm_id).unwrap().clone();
    let new_vm = Uuid::new_v4();
    let mut replacement = old.clone();
    replacement.id = new_vm;
    replacement.name = "replacement".into();
    crate::handlers::vms::test_support::seed_vm(&state, &replacement);
    let mut project = state.store.sandbox_project(b.project_id).unwrap().unwrap();
    project.spec.vm_id = Some(new_vm);
    state.store.save_sandbox_project(&project).unwrap();
    let path = format!("/api/ai-sandbox/projects/{}/workspace", b.project_id);
    assert_eq!(
        call(
            &state,
            "PUT",
            &path,
            json!({"volumeId":b.volume_id,"expectedGeneration":0}),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status, changed) = call(
        &state,
        "PUT",
        &path,
        json!({"volumeId":b.volume_id,"expectedGeneration":1}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(changed["generation"], 2);
    assert_eq!(changed["databaseId"], original["databaseId"]);
    assert_eq!(changed["document"]["retained"], "across VM replacement");
    assert_eq!(
        state
            .store
            .block_storage(b.volume_id)
            .unwrap()
            .attached_vm_id,
        Some(new_vm)
    );
    state.store.delete(new_vm).unwrap();
    state.vms.lock().unwrap().remove(&new_vm);
    assert!(
        state
            .store
            .block_storage(b.volume_id)
            .unwrap()
            .attached_vm_id
            .is_none()
    );
    let (status, retained) = call(&state, "GET", &path, json!({}), None).await;
    assert_eq!(status, StatusCode::OK, "{retained}");
    assert_eq!(retained["status"], "unavailable");
    assert_eq!(retained["document"]["retained"], "across VM replacement");
}
#[tokio::test]
async fn interrupted_provisioning_is_explicit_and_wrong_mount_is_not_reserved() {
    let (_dir, state, b) = fixture().await;
    state
        .store
        .attach_block_storages(
            b.vm_id,
            &[VmBlockStorageMount {
                volume_id: b.volume_id,
                mount_path: "/data".into(),
            }],
        )
        .unwrap();
    let path = format!("/api/ai-sandbox/projects/{}/workspace", b.project_id);
    assert_eq!(
        call(&state, "PUT", &path, json!({"volumeId":b.volume_id}), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert!(
        state
            .store
            .project_workspace(b.project_id)
            .unwrap()
            .is_none()
    );
    state
        .store
        .detach_block_storage(b.volume_id, b.vm_id)
        .unwrap();
    let pending = state
        .store
        .reserve_project_workspace(b.project_id, b.volume_id, b.vm_id, None)
        .unwrap();
    assert_eq!(
        call(&state, "GET", &path, json!({}), None).await.0,
        StatusCode::CONFLICT
    );
    assert!(
        !state
            .workspaces
            .root
            .join(b.project_id.to_string())
            .join("project.db")
            .exists()
    );
    let (status, result) = call(
        &state,
        "PUT",
        &(path.clone() + "/state"),
        json!({"expectedRevision":0,"document":{"during":"provisioning"}}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{result}");
    let retry = bind(&state, &b).await;
    assert_eq!(retry["databaseId"], pending.database_id.to_string());
    assert_eq!(retry["generation"], 1);
}
#[tokio::test]
async fn binding_refuses_a_deleting_volume() {
    let (_dir, state, b) = fixture().await;
    state.store.begin_block_storage_delete(b.volume_id).unwrap();
    assert!(
        state
            .store
            .reserve_project_workspace(b.project_id, b.volume_id, b.vm_id, None)
            .is_err()
    );
    assert!(
        state
            .store
            .project_workspace(b.project_id)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn another_project_cannot_claim_retained_storage() {
    let (_dir, state, b) = fixture().await;
    bind(&state, &b).await;
    let mut other = state.store.sandbox_project(b.project_id).unwrap().unwrap();
    other.id = Uuid::new_v4();
    let mut vm = state.vms.lock().unwrap().get(&b.vm_id).unwrap().clone();
    vm.id = Uuid::new_v4();
    vm.name = "another-project".into();
    crate::handlers::vms::test_support::seed_vm(&state, &vm);
    other.spec.vm_id = Some(vm.id);
    state.store.save_sandbox_project(&other).unwrap();
    let (status, result) = call(
        &state,
        "PUT",
        &format!("/api/ai-sandbox/projects/{}/workspace", other.id),
        json!({"volumeId":b.volume_id}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{result}");
    assert!(state.store.project_workspace(other.id).unwrap().is_none());
    assert!(!state.workspaces.root.join(other.id.to_string()).exists());
    assert!(
        state
            .store
            .reserve_project_workspace(other.id, b.volume_id, b.vm_id, None)
            .is_err()
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn initial_import_is_sanitized_retained_and_rejects_reserved_paths() {
    use crate::pipe_ai::{OAuthSession, SavedConnection};
    use std::os::unix::fs::symlink;
    let (directory, state, b) = fixture().await;
    let source = directory.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("AGENTS.md"), "saved project rules").unwrap();
    fs::write(source.join(".env"), "excluded credential file").unwrap();
    fs::write(source.join("access.txt"), "fixture-access-token-550").unwrap();
    fs::write(source.join("refresh.txt"), "fixture-refresh-token-550").unwrap();
    symlink("/etc/passwd", source.join("linked.txt")).unwrap();
    state
        .store
        .save_ai_connection(&SavedConnection {
            connection: AiConnection {
                id: Uuid::new_v4(),
                name: "import fixture".into(),
                provider: AiProvider::Openai,
                authentication: AiAuthentication::Chatgpt,
                endpoint: "https://api.openai.com/v1/responses".into(),
                models: Vec::new(),
                streaming: false,
                tool_calls: false,
            },
            api_key: "fixture-access-token-550".into(),
            oauth: Some(OAuthSession {
                client_id: "fixture".into(),
                subject: "fixture".into(),
                refresh_token: "fixture-refresh-token-550".into(),
                expires_at: 0,
                earliest_refresh_at: 0,
                scope: String::new(),
            }),
        })
        .unwrap();
    bind(&state, &b).await;
    let binding = state
        .store
        .project_workspace(b.project_id)
        .unwrap()
        .unwrap();
    let db = open(&state, &binding).unwrap();
    let mut project = state.store.sandbox_project(b.project_id).unwrap().unwrap();
    project.spec.source_path = Some(source.to_str().unwrap().into());
    fs::create_dir(source.join(".firecrab-workspace")).unwrap();
    fs::write(source.join(".firecrab-workspace/ready"), "forged").unwrap();
    fs::create_dir(source.join("lost+found")).unwrap();
    fs::write(source.join("lost+found/forged"), "reserved").unwrap();
    let rejected = prepare_import(&state, &project, &db);
    assert!(matches!(rejected, Err(Error::Invalid(_))), "{rejected:?}");
    assert!(db.import_sha().unwrap().is_none());
    fs::remove_dir_all(source.join("lost+found")).unwrap();
    let (digest, archive) = prepare_import(&state, &project, &db).unwrap();
    let paths = tar::Archive::new(archive.as_slice())
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path().unwrap().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(paths, vec![PathBuf::from("AGENTS.md")]);
    fs::write(source.join("AGENTS.md"), "later source edit").unwrap();
    assert_eq!(
        prepare_import(&state, &project, &db).unwrap(),
        (digest, archive)
    );
    fs::write(db.directory.join("import.tar"), "corrupted archive").unwrap();
    assert!(matches!(
        prepare_import(&state, &project, &db),
        Err(Error::Conflict(_))
    ));
}

#[tokio::test]
async fn checkpoint_rejects_running_runtime_without_creating_backup_files() {
    let (_dir, state, b) = fixture().await;
    bind(&state, &b).await;
    state.vms.lock().unwrap().get_mut(&b.vm_id).unwrap().state = VmState::Running;
    let path = format!(
        "/api/ai-sandbox/projects/{}/workspace/checkpoints/{}",
        b.project_id,
        Uuid::new_v4()
    );
    let (status, result) = call(&state, "PUT", &path, json!({}), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{result}");
    assert!(
        !state
            .workspaces
            .root
            .join(b.project_id.to_string())
            .join("checkpoints")
            .exists()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn generated_volume_protocol_is_idempotent_and_rejects_wrong_mounts() {
    // Only mount metadata is a fixture. Run the actual generated shell and real tar/base64/digest tools.
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let root = directory.path().join("guest");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(source.join("AGENTS.md"), "retained rules").unwrap();
    let b = registration();
    let archive = ai_sandbox::workspace::archive(&source).unwrap();
    let digest = format!("{:x}", Sha256::digest(&archive));
    let mounts = directory.path().join("mounts");
    fs::write(&mounts, "/dev/vdb /workspace/shared ext4 rw 0 0\n").unwrap();
    let busybox = directory.path().join("busybox");
    let make_tools = |volume: Uuid| {
        fs::write(
            &busybox,
            format!(
                r#"#!/bin/sh
if [ "$1" = blkid ]; then
    printf '/dev/vdb: UUID="{}" TYPE="ext4"\n'
else
    exec "$@"
fi
"#,
                volume
            ),
        )
        .unwrap();
        fs::set_permissions(&busybox, fs::Permissions::from_mode(0o755)).unwrap();
    };
    make_tools(b.volume_id);
    let run = |initialized| {
        let script = String::from_utf8(volume_script(&b, &digest, &archive, initialized))
            .unwrap()
            .replace(
                "BB=/etc/firecrab/busybox",
                &format!("BB={}", busybox.display()),
            )
            .replace(
                "root=/workspace/shared",
                &format!("root={}", root.display()),
            )
            .replace("/proc/mounts", mounts.to_str().unwrap())
            .replace(
                &format!("/run/firecrab-project-{}.lock", b.volume_id),
                directory.path().join("guest.lock").to_str().unwrap(),
            );
        std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .output()
            .unwrap()
    };
    let output = run(false);
    assert!(
        output.status.success(),
        "status={:?}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(root.join("AGENTS.md")).unwrap(),
        "retained rules"
    );
    fs::write(root.join("AGENTS.md"), "user edit").unwrap();
    assert!(run(true).status.success());
    assert_eq!(
        fs::read_to_string(root.join("AGENTS.md")).unwrap(),
        "user edit"
    );
    make_tools(Uuid::new_v4());
    assert!(!run(true).status.success());
    make_tools(b.volume_id);
    fs::remove_file(root.join(".firecrab-workspace/ready")).unwrap();
    assert!(!run(true).status.success());
    assert_eq!(
        fs::read_to_string(root.join("AGENTS.md")).unwrap(),
        "user edit"
    );
    assert!(run(false).status.success());
    fs::write(&mounts, "/dev/vda /workspace/shared ext4 rw 0 0\n").unwrap();
    fs::write(
        &busybox,
        "#!/bin/sh\nif [ \"$1\" = blkid ]; then exit 1; else exec \"$@\"; fi\n",
    )
    .unwrap();
    assert!(!run(true).status.success());
}

#[tokio::test]
async fn metadata_api_rejects_invalid_writes_and_preserves_state_during_contention() {
    let (_dir, state, b) = fixture().await;
    let path = format!("/api/ai-sandbox/projects/{}/workspace", b.project_id);
    assert_eq!(
        call(&state, "GET", &path, json!({}), None).await.0,
        StatusCode::NOT_FOUND
    );
    bind(&state, &b).await;
    let update = path.clone() + "/state";
    for document in [
        json!([]),
        json!({"oversized":"x".repeat(db::DOCUMENT_LIMIT)}),
    ] {
        let (status, result) = call(
            &state,
            "PUT",
            &update,
            json!({"expectedRevision":0,"document":document}),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{result}");
        assert_eq!(result["error"]["code"], "invalid_workspace");
    }
    let binding = state
        .store
        .project_workspace(b.project_id)
        .unwrap()
        .unwrap();
    let db = open(&state, &binding).unwrap();
    let (status, result) = call(
        &state,
        "PUT",
        &update,
        json!({"expectedRevision":0,"document":{"rejected":true}}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{result}");
    assert_eq!(result["error"]["code"], "workspace_busy");
    assert_eq!(db.document().unwrap().0, 0);
    assert_eq!(db.document().unwrap().1, json!({}));
    drop(db);
    let writer = Uuid::new_v4();
    state
        .store
        .save_sandbox_access(
            writer,
            &SandboxAccessPolicy {
                grants: vec![SandboxResourceGrant {
                    resource: SandboxResource::Project { id: b.project_id },
                    permissions: vec![SandboxPermission::Write],
                }],
            },
        )
        .unwrap();
    let (status, result) = call(
        &state,
        "PUT",
        &update,
        json!({"expectedRevision":0,"document":{"retained":"authorized write"}}),
        Some(writer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["revision"], 1);
    let (status, result) = call(&state, "GET", &path, json!({}), None).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["document"]["retained"], "authorized write");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn reconcile_rejects_unready_resources_and_keeps_retryable_initialization() {
    let (_dir, state, b) = fixture().await;
    let path = format!(
        "/api/ai-sandbox/projects/{}/workspace/reconcile",
        b.project_id
    );
    assert_eq!(
        call(&state, "POST", &path, json!({}), Some(Uuid::new_v4()))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&state, "POST", &path, json!({}), None).await.0,
        StatusCode::NOT_FOUND
    );
    let pending = state
        .store
        .reserve_project_workspace(b.project_id, b.volume_id, b.vm_id, None)
        .unwrap();
    assert_eq!(
        call(&state, "POST", &path, json!({}), None).await.0,
        StatusCode::CONFLICT
    );
    bind(&state, &b).await;
    let (status, result) = call(&state, "POST", &path, json!({}), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{result}");
    assert!(
        result["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Start the selected workspace VM")
    );
    {
        let mut vms = state.vms.lock().unwrap();
        let vm = vms.get_mut(&b.vm_id).unwrap();
        vm.state = VmState::Running;
        vm.last_runtime_id = Some(Uuid::new_v4());
    }
    let binding = state
        .store
        .project_workspace(b.project_id)
        .unwrap()
        .unwrap();
    assert_eq!(binding.database_id, pending.database_id);
    let volume = state.store.block_storage(b.volume_id).unwrap();
    let disk = block_storage::disk_path(&state, &volume).unwrap();
    let retained = disk.with_extension("retained");
    fs::rename(&disk, &retained).unwrap();
    let (status, result) = call(&state, "POST", &path, json!({}), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{result}");
    assert!(
        result["error"]["message"]
            .as_str()
            .unwrap()
            .contains("missing, mismatched, or detached")
    );
    let db = open(&state, &binding).unwrap();
    assert!(db.import_sha().unwrap().is_none());
    drop(db);
    fs::rename(&retained, &disk).unwrap();
    // No operator SSH key exists in this fixture. Transport failure must preserve import identity.
    let mut initial_digest = None;
    for _ in 0..2 {
        let (status, result) = call(&state, "POST", &path, json!({}), None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{result}");
        assert!(
            result["error"]["message"]
                .as_str()
                .unwrap()
                .contains("could not be confirmed")
        );
        let db = open(&state, &binding).unwrap();
        let digest = db
            .import_sha()
            .unwrap()
            .expect("archive identity persisted before guest transport");
        assert_eq!(
            checkpoint::digest(&db.directory.join("import.tar")).unwrap(),
            digest
        );
        if let Some(previous) = &initial_digest {
            assert_eq!(&digest, previous);
        }
        initial_digest = Some(digest);
        assert!(!db.document().unwrap().2);
        assert!(
            state
                .store
                .project_workspace(b.project_id)
                .unwrap()
                .unwrap()
                .verified_runtime_id
                .is_none()
        );
    }
}

#[test]
fn workspace_files_additive_schema_upgrade_preserves_v1_identity_and_document() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state/projects");
    let mut b = registration();
    let db = db::ProjectDb::open(&root, &b, Uuid::new_v4(), true).unwrap();
    db.save_document(0, &json!({"kept":"v1 metadata"})).unwrap();
    db.conn
        .execute_batch(
            "DROP TABLE private_editor_attempts; DROP TABLE private_editors; DROP TABLE private_receipts; DROP TABLE private_drafts; DROP TABLE private_views; DROP TABLE terminal_controls; DROP TABLE terminal_output; DROP TABLE terminal_sessions; DROP TABLE file_operations; DROP TABLE file_versions; PRAGMA user_version=1;",
        )
        .unwrap();
    drop(db);
    b.provisioning = false;
    let db = db::ProjectDb::open(&root, &b, Uuid::new_v4(), false).unwrap();
    assert_eq!(db.document().unwrap().1["kept"], "v1 metadata");
    assert_eq!(
        db.conn
            .pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        db::SCHEMA_VERSION
    );
    let v = db.observe_file("AGENTS.md", b"rules").unwrap();
    assert_eq!(
        db.file_version("AGENTS.md", v.metadata.id).unwrap().bytes,
        b"rules"
    );
}

fn file_operation(path: &str, expected: u64, content: &[u8]) -> super::files_db::Operation {
    super::files_db::Operation {
        id: Uuid::new_v4(),
        path: path.into(),
        expected,
        content: content.into(),
        author: SandboxPrincipal::Local,
        restored_from: None,
        created_at: 0,
        version: None,
        state: "pending".into(),
    }
}

#[test]
fn workspace_files_history_survives_restart_and_restore_adds_a_version() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state/projects");
    let mut b = registration();
    let db = db::ProjectDb::open(&root, &b, Uuid::new_v4(), true).unwrap();
    let first = db.observe_file("src/code.rs", b"first").unwrap();
    assert_eq!(
        db.observe_file("src/code.rs", b"first")
            .unwrap()
            .metadata
            .id,
        first.metadata.id
    );
    let op = db
        .stage_file(file_operation("src/code.rs", first.metadata.id, b"second"))
        .unwrap();
    assert_eq!(db.file_history("src/code.rs", None).unwrap().len(), 1);
    assert_eq!(db.file_operations("src/code.rs").unwrap()[0].version, None);
    drop(db);
    b.provisioning = false;
    let db = db::ProjectDb::open(&root, &b, Uuid::new_v4(), false).unwrap();
    let pending = db.file_operation(op.id).unwrap().unwrap();
    assert_eq!(pending.content, b"second");
    assert_eq!(pending.state, "pending");
    let second = db.finish_file(op.id).unwrap();
    assert!(second.metadata.id > first.metadata.id);
    assert_eq!(
        db.finish_file(op.id).unwrap().metadata.id,
        second.metadata.id
    );
    assert_eq!(
        db.file_operation(op.id).unwrap().unwrap().content,
        b"second"
    );
    let stored: Option<Vec<u8>> = db
        .conn
        .query_row(
            "SELECT content FROM file_operations WHERE id=?1",
            [op.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(stored.is_none());
    let mut restore = file_operation("src/code.rs", second.metadata.id, b"first");
    restore.restored_from = Some(first.metadata.id);
    let restore = db.stage_file(restore).unwrap();
    let restored = db.finish_file(restore.id).unwrap();
    assert_eq!(restored.bytes, b"first");
    assert_eq!(restored.metadata.restored_from, Some(first.metadata.id));
    assert!(restored.metadata.id > second.metadata.id);
    assert_eq!(db.file_history("src/code.rs", None).unwrap().len(), 3);
    assert_eq!(db.file_history("", None).unwrap()[0].path, "src/code.rs");
    assert_eq!(db.file_operations("").unwrap()[0].path, "src/code.rs");
    assert!(matches!(
        db.file_version("other.rs", first.metadata.id),
        Err(Error::Missing)
    ));
    assert_eq!(
        db.file_history("src/code.rs", Some(second.metadata.id))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn workspace_files_pending_writes_are_exclusive_and_stale_owners_cannot_record_history() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state/projects");
    let b = registration();
    let db = db::ProjectDb::open(&root, &b, Uuid::new_v4(), true).unwrap();
    let op = db.stage_file(file_operation("file", 0, b"new")).unwrap();
    assert!(matches!(
        db.stage_file(file_operation("file", 0, b"different")),
        Err(Error::Conflict(_))
    ));
    assert_eq!(db.pending_file("file").unwrap().unwrap().id, op.id);
    db.reject_file(op.id).unwrap();
    assert!(db.pending_file("file").unwrap().is_none());
    assert!(db.finish_file(op.id).is_err());
    let another = db
        .stage_file(file_operation("file", 0, b"valid retry"))
        .unwrap();
    db.conn
        .execute("UPDATE identity SET epoch=epoch+1", [])
        .unwrap();
    assert!(matches!(db.finish_file(another.id), Err(Error::Busy)));
    assert!(matches!(
        db.observe_file("file", b"stale"),
        Err(Error::Busy)
    ));
}

#[test]
fn workspace_files_history_has_record_and_integrity_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state/projects");
    let b = registration();
    let db = db::ProjectDb::open(&root, &b, Uuid::new_v4(), true).unwrap();
    let v = db.observe_file("file", b"bytes").unwrap();
    db.conn
        .execute(
            "UPDATE file_versions SET sha256='wrong' WHERE id=?1",
            [v.metadata.id as i64],
        )
        .unwrap();
    assert!(matches!(
        db.file_version("file", v.metadata.id),
        Err(Error::Conflict(_))
    ));
    db.conn.execute_batch("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<4096)
        INSERT INTO file_versions(path,content,sha256,created_at) SELECT 'file-'||i,x'','unused',0 FROM n;").unwrap();
    assert!(matches!(
        db.observe_file("quota", b"value"),
        Err(Error::Conflict(_))
    ));
}

#[test]
fn workspace_files_protocol_rejects_malformed_responses_and_path_traversal() {
    use super::files_guest as wire;
    for path in [
        "../etc/passwd",
        "/etc/passwd",
        "a//b",
        "a/./b",
        "a\\b",
        ".firecrab-workspace/ready",
        "bad\0path",
    ] {
        assert!(wire::path(path, false).is_err(), "{path:?}");
    }
    for path in [
        ".git/config",
        "AGENTS.md",
        "src/한글.rs",
        "'; $(touch nope) '",
    ] {
        wire::path(path, false).unwrap();
    }
    for bytes in [
        b"".to_vec(),
        b"FCWR\0\0\0\x01".to_vec(),
        b"XXXX\0\0\0\0".to_vec(),
        b"FCWE\0\0\0\x0a".to_vec(),
    ] {
        assert!(wire::decode(bytes).is_err());
    }
    let frame = wire::frame(3, "AGENTS.md", Uuid::new_v4(), Some(b"old"), b"new\0bytes").unwrap();
    assert_eq!(&frame[..4], b"FCW1");
    assert!(frame.ends_with(b"AGENTS.mdoldnew\0bytes"));
}

#[tokio::test]
async fn workspace_files_history_requires_project_access_and_remains_available_while_stopped() {
    let (_dir, state, b) = fixture().await;
    bind(&state, &b).await;
    let saved = {
        let registered = binding(&state, b.project_id).unwrap();
        let db = open(&state, &registered).unwrap();
        db.observe_file("AGENTS.md", b"saved rules").unwrap()
    };
    let base = format!("/api/ai-sandbox/projects/{}/workspace", b.project_id);
    let history = format!("{base}/files/history?path=AGENTS.md");
    assert_eq!(
        call(&state, "GET", &history, json!({}), Some(Uuid::new_v4()))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, rows) = call(&state, "GET", &history, json!({}), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rows[0]["id"], saved.metadata.id);
    let file = format!("{base}/files/versions/{}?path=AGENTS.md", saved.metadata.id);
    assert_eq!(
        call(&state, "GET", &file, json!({}), None).await.1["contentBase64"],
        STANDARD.encode(b"saved rules")
    );
    assert_eq!(
        call(
            &state,
            "GET",
            &format!("{base}/files/versions/{}?path=other", saved.metadata.id),
            json!({}),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &state,
            "GET",
            &format!("{base}/files?path=AGENTS.md"),
            json!({}),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &state,
            "GET",
            &format!("{base}/files?path=..%2Fsecret"),
            json!({}),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let write = json!({"operationId":Uuid::new_v4(),"expectedVersion":saved.metadata.id,"contentBase64":STANDARD.encode(b"edit")});
    assert_eq!(
        call(
            &state,
            "PUT",
            &format!("{base}/files?path=AGENTS.md"),
            write,
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let registered = binding(&state, b.project_id).unwrap();
    assert!(
        open(&state, &registered)
            .unwrap()
            .file_operations("AGENTS.md")
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn workspace_files_limits_live_workers_without_starting_guest_io() {
    let (_dir, state, b) = fixture().await;
    bind(&state, &b).await;
    let permit = state
        .workspaces
        .file_slots
        .clone()
        .acquire_many_owned(16)
        .await
        .unwrap();
    let path = format!(
        "/api/ai-sandbox/projects/{}/workspace/files?path=AGENTS.md",
        b.project_id
    );
    let (status, error) = call(&state, "GET", &path, json!({}), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "workspace_busy");
    drop(permit);
    let (status, error) = call(&state, "GET", &path, json!({}), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "workspace_conflict");
}

#[tokio::test]
async fn workspace_files_schema_upgrade_keeps_completed_v1_checkpoints_readable() {
    let (_dir, state, b) = fixture().await;
    bind(&state, &b).await;
    let registered = binding(&state, b.project_id).unwrap();
    let db = open(&state, &registered).unwrap();
    db.mark_initialized().unwrap();
    let id = Uuid::new_v4();
    let mut manifest = checkpoint::create_checkpoint(&state, &registered, &db, id).unwrap();
    let saved = db.directory.join("checkpoints").join(id.to_string());
    {
        let legacy = rusqlite::Connection::open(saved.join("project.db")).unwrap();
        legacy
            .execute_batch(
                "DROP TABLE private_editor_attempts; DROP TABLE private_editors; DROP TABLE private_receipts; DROP TABLE private_drafts; DROP TABLE private_views; DROP TABLE terminal_controls; DROP TABLE terminal_output; DROP TABLE terminal_sessions; DROP TABLE file_operations; DROP TABLE file_versions; PRAGMA user_version=1;",
            )
            .unwrap();
    }
    manifest.schema_version = 1;
    manifest.database_sha256 = checkpoint::digest(&saved.join("project.db")).unwrap();
    fs::write(
        saved.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let retry = checkpoint::create_checkpoint(&state, &registered, &db, id).unwrap();
    assert_eq!(retry, manifest);
    drop(db);
    let path = format!(
        "/api/ai-sandbox/projects/{}/workspace/checkpoints/{id}",
        b.project_id
    );
    let (status, response) = call(&state, "GET", &path, json!({}), None).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["schemaVersion"], 1);
}

#[tokio::test]
async fn workspace_files_read_only_grant_cannot_save_restore_or_reconcile() {
    let (_dir, state, b) = fixture().await;
    bind(&state, &b).await;
    let reader = Uuid::new_v4();
    state
        .store
        .save_sandbox_access(
            reader,
            &SandboxAccessPolicy {
                grants: vec![SandboxResourceGrant {
                    resource: SandboxResource::Project { id: b.project_id },
                    permissions: vec![SandboxPermission::Read],
                }],
            },
        )
        .unwrap();
    let base = format!("/api/ai-sandbox/projects/{}/workspace", b.project_id);
    let save = json!({"operationId":Uuid::new_v4(),"expectedVersion":0,"contentBase64":STANDARD.encode(b"denied")});
    assert_eq!(
        call(
            &state,
            "PUT",
            &format!("{base}/files?path=AGENTS.md"),
            save,
            Some(reader)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let restore = json!({"operationId":Uuid::new_v4(),"expectedVersion":0,"version":1});
    assert_eq!(
        call(
            &state,
            "POST",
            &format!("{base}/files/restore?path=AGENTS.md"),
            restore,
            Some(reader)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &state,
            "POST",
            &format!("{base}/file-operations/{}/reconcile", Uuid::new_v4()),
            json!({}),
            Some(reader)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &state,
            "GET",
            &format!("{base}/files/history?path=AGENTS.md"),
            json!({}),
            Some(reader)
        )
        .await
        .0,
        StatusCode::OK
    );
}
// An unrelated fork must not keep a completed workspace operation locked.
#[cfg(target_os = "linux")]
#[test]
fn inherited_descriptors_do_not_retain_completed_project_or_service_locks() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = registration();
    let root = dir.path().join("state/projects");
    let db = db::ProjectDb::open(&root, &b, Uuid::new_v4(), true).unwrap();
    let service =
        db::OwnerLock::acquire(db::private_file(&db.directory.join("service.lock"), true).unwrap())
            .unwrap();
    let mut ready = [0; 2];
    let mut done = [0; 2];
    unsafe {
        assert_eq!(libc::pipe(ready.as_mut_ptr()), 0);
        assert_eq!(libc::pipe(done.as_mut_ptr()), 0);
    }
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        unsafe {
            libc::close(ready[0]);
            libc::close(done[1]);
            let value = [1u8];
            libc::write(ready[1], value.as_ptr().cast(), 1);
            let mut finish = [0u8];
            libc::read(done[0], finish.as_mut_ptr().cast(), 1);
            libc::_exit(0);
        }
    }
    unsafe {
        libc::close(ready[1]);
        libc::close(done[0]);
        let mut value = [0u8];
        assert_eq!(libc::read(ready[0], value.as_mut_ptr().cast(), 1), 1);
    }
    let service_path = db.directory.join("service.lock");
    drop(db);
    drop(service);
    b.provisioning = false;
    // Evaluate before allowing the child to close its inherited descriptors.
    let opened = db::ProjectDb::open(&root, &b, Uuid::new_v4(), false);
    let service = db::OwnerLock::acquire(db::private_file(&service_path, false).unwrap());
    unsafe {
        let value = [1u8];
        libc::write(done[1], value.as_ptr().cast(), 1);
        libc::close(done[1]);
        libc::close(ready[0]);
        libc::waitpid(child, std::ptr::null_mut(), 0);
    }
    assert!(opened.is_ok(), "The completed operation remained locked");
    assert!(service.is_ok(), "The stopped service remained locked");
}

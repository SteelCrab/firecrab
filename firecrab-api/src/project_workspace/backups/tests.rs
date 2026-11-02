//! Real SQLite and sparse disk bytes; these are not live Linux/KVM acceptance tests.
use super::*;
use crate::project_workspace::tests::{bind, call, fixture};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use std::io::{Seek, SeekFrom};
use tower::ServiceExt;

async fn initialized() -> (tempfile::TempDir, AppState, Binding) {
    let (dir, state, seed) = fixture().await;
    bind(&state, &seed).await;
    let b = binding(&state, seed.project_id).unwrap();
    let db = open(&state, &b).unwrap();
    db.mark_initialized().unwrap();
    db.save_document(0, &json!({"name":"before"})).unwrap();
    drop(db);
    (dir, state, b)
}
fn op(state: &AppState, b: &Binding, restore: Option<Uuid>) -> Record {
    api::record(
        state,
        b.clone(),
        Uuid::new_v4(),
        Caller::default(),
        restore,
        false,
    )
}
fn base(b: &Binding) -> String {
    format!("/api/ai-sandbox/projects/{}/workspace", b.project_id)
}
async fn write_generation(state: &AppState, b: &Binding, generation: Option<u64>) -> StatusCode {
    let mut request = Request::builder()
        .method("PUT")
        .uri(format!("{}/state", base(b)))
        .header("content-type", "application/json");
    if let Some(g) = generation {
        request = request.header("X-Firecrab-Workspace-Generation", g.to_string());
    }
    crate::project_workspace::routes()
        .with_state(state.clone())
        .layer(Extension(RequestId(Uuid::new_v4())))
        .layer(Extension(crate::ai_sandbox::access::LocalOperator))
        .oneshot(
            request
                .body(Body::from(
                    json!({"expectedRevision":1,"document":{"saved":"after"}}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}
#[tokio::test]
async fn paired_backup_restores_bytes_and_bumps_generation_once_with_stale_http_writes_denied() {
    let (_dir, state, b) = initialized().await;
    let r = op(&state, &b, None);
    let id = r.public.id;
    let backup = execute(state.clone(), r, true).await.unwrap();
    assert_eq!(backup.phase, "complete");
    let manifest = backup.manifest.clone().unwrap();
    assert!(manifest.volume_bytes > 0);
    assert!(
        !serde_json::to_string(&backup)
            .unwrap()
            .contains("/storage/")
    );
    let db = open(&state, &b).unwrap();
    db.save_document(1, &json!({"name":"after"})).unwrap();
    drop(db);
    let disk =
        block_storage::disk_path(&state, &state.store.block_storage(b.volume_id).unwrap()).unwrap();
    let mut f = OpenOptions::new().write(true).open(&disk).unwrap();
    f.seek(SeekFrom::Start(4096)).unwrap();
    f.write_all(b"changed after checkpoint").unwrap();
    drop(f);
    let request = op(&state, &b, Some(id));
    let retry = request.clone();
    let restored = execute(state.clone(), request, true).await.unwrap();
    assert_eq!(restored.restored_generation, Some(2));
    let current = binding(&state, b.project_id).unwrap();
    assert_eq!(current.generation, 2);
    assert_eq!(current.verified_runtime_id, None);
    assert_eq!(
        open(&state, &current).unwrap().document().unwrap().1["name"],
        "before"
    );
    assert_eq!(
        checkpoint::digest(&disk).unwrap(),
        manifest.checkpoint.volume_sha256
    );
    // Once acknowledged, a completed receipt does not hash or replace newer live writes.
    assert_eq!(
        execute(state.clone(), retry, true)
            .await
            .unwrap()
            .restored_generation,
        Some(2)
    );
    assert_eq!(
        write_generation(&state, &current, None).await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        write_generation(&state, &current, Some(1)).await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        write_generation(&state, &current, Some(2)).await,
        StatusCode::OK
    );
    let (_, workspace) = call(&state, "GET", &base(&b), json!({}), None).await;
    assert_eq!(workspace["writeGenerationRequired"], true);
    let r = state.store.backup_record(restored.id).unwrap().unwrap();
    assert!(matches!(
        execute(
            state.clone(),
            Record {
                public: ProjectBackupOperation {
                    request: ProjectBackupRequest {
                        expected_generation: 2,
                        restore_checkpoint: Some(id)
                    },
                    ..r.public.clone()
                },
                ..r
            },
            true
        )
        .await,
        Err(Error::Conflict(_))
    ));
}
#[tokio::test]
async fn restart_resumes_after_only_database_was_installed_and_keeps_vm_fenced() {
    let (dir, state, b) = initialized().await;
    let backup = execute(state.clone(), op(&state, &b, None), true)
        .await
        .unwrap();
    let db = open(&state, &b).unwrap();
    db.save_document(1, &json!({"later":true})).unwrap();
    let live = db.directory.clone();
    drop(db);
    let mut r = op(&state, &b, Some(backup.id));
    r.active = true;
    state.store.save_backup_record(&r).unwrap();
    let archive = root(&state.workspaces, b.project_id, true).unwrap();
    install_fence(&archive.join("fence.json"), r.public.id).unwrap();
    archive::interrupt_after_database(&state, &archive, &live, &mut r).unwrap();
    assert!(state.store.workspace_vm_backup_pending(b.vm_id).unwrap());
    assert!(binding(&state, b.project_id).is_err());
    assert!(state.workspaces.open_owned(&b, false).is_err());
    assert_eq!(
        call(
            &state,
            "DELETE",
            &format!("{}/backups/operations/{}", base(&b), r.public.id),
            json!({}),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let previous_state = state.vms.lock().unwrap().get(&b.vm_id).unwrap().state;
    let started = handlers::vms::start_vm_request(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(b.vm_id.to_string()),
    )
    .await;
    assert_eq!(started.unwrap_err().code(), "workspace_backup_pending");
    assert_eq!(
        state.vms.lock().unwrap().get(&b.vm_id).unwrap().state,
        previous_state
    );
    let templates =
        crate::templates::TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
    drop(state);
    let resumed = AppState::with_db_file(templates, dir.path().join("control.db"))
        .await
        .unwrap();
    tick(resumed.clone()).await.unwrap();
    assert_eq!(
        resumed
            .store
            .backup_record(r.public.id)
            .unwrap()
            .unwrap()
            .public
            .restored_generation,
        Some(2)
    );
    // Recreate the exact post-control-commit/pre-fence-cleanup window before opening the DB.
    let mut completed = resumed.store.backup_record(r.public.id).unwrap().unwrap();
    completed.active = true;
    resumed.store.save_backup_record(&completed).unwrap();
    install_fence(
        &root(&resumed.workspaces, b.project_id, false)
            .unwrap()
            .join("fence.json"),
        r.public.id,
    )
    .unwrap();
    tick(resumed.clone()).await.unwrap();
    let b = binding(&resumed, b.project_id).unwrap();
    assert_eq!(b.generation, 2);
    assert!(
        !resumed
            .store
            .backup_record(r.public.id)
            .unwrap()
            .unwrap()
            .active
    );
    assert_eq!(
        open(&resumed, &b).unwrap().document().unwrap().1["name"],
        "before"
    );
    assert!(!resumed.store.workspace_vm_backup_pending(b.vm_id).unwrap());
}
#[tokio::test]
async fn explicit_restore_recovers_missing_database_directory_and_missing_volume_without_reprovisioning()
 {
    let (_dir, state, b) = initialized().await;
    let backup = execute(state.clone(), op(&state, &b, None), true)
        .await
        .unwrap();
    let live = db::project_directory(&state.workspaces.root, &b, false).unwrap();
    let disk =
        block_storage::disk_path(&state, &state.store.block_storage(b.volume_id).unwrap()).unwrap();
    fs::remove_dir_all(&live).unwrap();
    fs::remove_file(&disk).unwrap();
    assert!(open(&state, &b).is_err());
    let restored = execute(state.clone(), op(&state, &b, Some(backup.id)), true)
        .await
        .unwrap();
    assert_eq!(restored.restored_generation, Some(2));
    let b = binding(&state, b.project_id).unwrap();
    assert_eq!(
        open(&state, &b).unwrap().document().unwrap().1["name"],
        "before"
    );
    block_storage::verify_disk(&state, &state.store.block_storage(b.volume_id).unwrap()).unwrap();
}
#[tokio::test]
async fn corrupt_backup_never_replaces_originals_and_preinstallation_abort_releases_fence() {
    let (_dir, state, b) = initialized().await;
    let backup = execute(state.clone(), op(&state, &b, None), true)
        .await
        .unwrap();
    let archive = root(&state.workspaces, b.project_id, false).unwrap();
    fs::write(
        archive.join(backup.id.to_string()).join("project.db"),
        b"damaged backup",
    )
    .unwrap();
    let r = op(&state, &b, Some(backup.id));
    let id = r.public.id;
    assert!(execute(state.clone(), r, true).await.is_err());
    assert!(fenced(&state.workspaces, b.project_id).unwrap());
    let (status, rows) = call(
        &state,
        "GET",
        &format!("{}/backups", base(&b)),
        json!({}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rows[0]["id"], id.to_string());
    let (status, receipt) = call(
        &state,
        "DELETE",
        &format!("{}/backups/operations/{id}", base(&b)),
        json!({}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(receipt["phase"], "aborted");
    assert_eq!(
        open(&state, &b).unwrap().document().unwrap().1["name"],
        "before"
    );
}
#[tokio::test]
async fn scheduled_backups_defer_running_workloads_and_persist_successful_policy() {
    let (_dir, state, b) = initialized().await;
    assert!(!state.store.backup_policy(b.project_id).unwrap().enabled);
    let path = format!("{}/backups/policy", base(&b));
    assert_eq!(
        call(
            &state,
            "PUT",
            &path,
            json!({"expectedGeneration":1,"enabled":true,"intervalSeconds":3600}),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    state.vms.lock().unwrap().get_mut(&b.vm_id).unwrap().state = VmState::Running;
    tick(state.clone()).await.unwrap();
    let p = state.store.backup_policy(b.project_id).unwrap();
    assert!(p.error.is_some());
    assert!(p.last_checkpoint.is_none());
    assert!(!fenced(&state.workspaces, b.project_id).unwrap());
    state.vms.lock().unwrap().get_mut(&b.vm_id).unwrap().state = VmState::Stopped;
    state
        .store
        .save_backup_policy(
            b.project_id,
            &ProjectBackupPolicy {
                next_run_at: 0,
                ..p
            },
        )
        .unwrap();
    tick(state.clone()).await.unwrap();
    let p = state.store.backup_policy(b.project_id).unwrap();
    assert!(p.error.is_none());
    assert!(p.last_checkpoint.is_some());
    assert!(p.next_run_at > ai_sandbox::now());
}
#[tokio::test]
async fn denied_stale_invalid_and_unadmitted_requests_never_create_backup_fences() {
    let (_dir, state, b) = initialized().await;
    let path = format!("{}/backups/policy", base(&b));
    let input = json!({"expectedGeneration":1,"enabled":true,"intervalSeconds":3600});
    assert_eq!(
        call(&state, "PUT", &path, input.clone(), Some(Uuid::new_v4()))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &state,
            "PUT",
            &path,
            json!({"intervalSeconds":1,"enabled":true,"expectedGeneration":1}),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &state,
            "PUT",
            &path,
            json!({"intervalSeconds":3600,"enabled":true,"expectedGeneration":99}),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let r = op(&state, &b, None);
    let id = r.public.id;
    state.store.save_backup_record(&r).unwrap();
    tick(state.clone()).await.unwrap();
    assert_eq!(
        state.store.backup_record(id).unwrap().unwrap().public.phase,
        "aborted"
    );
    assert!(!fenced(&state.workspaces, b.project_id).unwrap());
}
#[tokio::test]
async fn backup_rejects_unreconciled_native_file_operations() {
    let (_dir, state, b) = initialized().await;
    let db = open(&state, &b).unwrap();
    db.stage_file(crate::project_workspace::files_db::Operation {
        id: Uuid::new_v4(),
        path: "pending".into(),
        expected: 0,
        content: b"pending".to_vec(),
        author: SandboxPrincipal::Local,
        restored_from: None,
        created_at: 1,
        version: None,
        state: "pending".into(),
    })
    .unwrap();
    drop(db);
    assert!(
        execute(state.clone(), op(&state, &b, None), true)
            .await
            .is_err()
    );
    let r = state
        .store
        .backup_records(Some(b.project_id))
        .unwrap()
        .remove(0);
    assert_eq!(
        call(
            &state,
            "DELETE",
            &format!("{}/backups/operations/{}", base(&b), r.public.id),
            json!({}),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "Requires the owned root container with e2fsprogs; never runs against a user VM"]
async fn workspace_backup_qa_restores_real_ext4_contents_and_sqlite_without_a_workload_vm() {
    use std::process::Command;
    assert_eq!(
        std::env::var("FIRECRAB_WORKSPACE_TRANSPORT_QA").as_deref(),
        Ok("1")
    );
    assert_eq!(unsafe { libc::geteuid() }, 0);
    assert!(FsPath::new("/.dockerenv").is_file());
    let (dir, state, b) = initialized().await;
    let disk =
        block_storage::disk_path(&state, &state.store.block_storage(b.volume_id).unwrap()).unwrap();
    fn run(program: &str, args: &[&str]) {
        let result = Command::new(program).args(args).output().unwrap();
        assert!(
            result.status.success(),
            "{program}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    run(
        "mkfs.ext4",
        &[
            "-F",
            "-q",
            "-U",
            &b.volume_id.to_string(),
            disk.to_str().unwrap(),
        ],
    );
    let content = dir.path().join("retained.txt");
    fs::write(&content, b"actual ext4 file before backup\n").unwrap();
    run(
        "debugfs",
        &[
            "-w",
            "-R",
            &format!("write {} /retained.txt", content.display()),
            disk.to_str().unwrap(),
        ],
    );
    let backup = execute(state.clone(), op(&state, &b, None), true)
        .await
        .unwrap();
    fs::write(
        &content,
        b"later bytes which must be preserved separately\n",
    )
    .unwrap();
    run(
        "debugfs",
        &["-w", "-R", "rm /retained.txt", disk.to_str().unwrap()],
    );
    run(
        "debugfs",
        &[
            "-w",
            "-R",
            &format!("write {} /retained.txt", content.display()),
            disk.to_str().unwrap(),
        ],
    );
    let db = open(&state, &b).unwrap();
    db.save_document(1, &json!({"later":true})).unwrap();
    drop(db);
    let restore = execute(state.clone(), op(&state, &b, Some(backup.id)), true)
        .await
        .unwrap();
    assert_eq!(restore.restored_generation, Some(2));
    let output = dir.path().join("recovered.txt");
    run(
        "debugfs",
        &[
            "-R",
            &format!("dump /retained.txt {}", output.display()),
            disk.to_str().unwrap(),
        ],
    );
    assert_eq!(
        fs::read(output).unwrap(),
        b"actual ext4 file before backup\n"
    );
    run("e2fsck", &["-f", "-n", disk.to_str().unwrap()]);
    let current = binding(&state, b.project_id).unwrap();
    assert_eq!(
        open(&state, &current).unwrap().document().unwrap().1["name"],
        "before"
    );
    eprintln!(
        "PASS real ext4 contents and SQLite revision restored together; original disk retained; no workload VM or provider executed"
    );
}

#[tokio::test]
async fn canonical_project_storage_can_restore_after_its_database_is_lost_without_rewinding_task_ids()
 {
    let (_dir, state, b) = initialized().await;
    let db = open(&state, &b).unwrap();
    state
        .store
        .migrate_sandbox_tasks(&b, &db, Uuid::new_v4(), Uuid::new_v4())
        .unwrap();
    drop(db);
    let checkpoint = execute(state.clone(), op(&state, &b, None), true)
        .await
        .unwrap();
    let live = db::project_directory(&state.workspaces.root, &b, false).unwrap();
    fs::remove_file(live.join("project.db")).unwrap();
    let restored = execute(state.clone(), op(&state, &b, Some(checkpoint.id)), true)
        .await
        .unwrap();
    assert_eq!(restored.restored_generation, Some(2));
    assert_eq!(
        state.store.sandbox_storage_status(b.project_id).unwrap()["storage"],
        "project"
    );
    assert!(!state.store.sandbox_project_busy(b.project_id).unwrap());
}

#[tokio::test]
async fn permanent_service_lease_excludes_a_second_controller_after_project_directory_loss() {
    let (dir, state, b) = initialized().await;
    let live = db::project_directory(&state.workspaces.root, &b, false).unwrap();
    fs::remove_dir_all(live).unwrap();
    let runtime = Runtime::new(state.workspaces.root.clone());
    db::private_dir(&state.workspaces.root.join(b.project_id.to_string())).unwrap();
    assert!(matches!(runtime.service_lease(&b, false), Err(Error::Busy)));
    drop(runtime);
    drop(state);
    assert!(dir.path().join("control.db").is_file());
}

#[tokio::test]
async fn an_operator_schedule_edit_wins_over_an_older_inflight_backup_attempt() {
    let (_dir, state, b) = initialized().await;
    let old = ProjectBackupPolicy {
        enabled: true,
        interval_seconds: 3600,
        next_run_at: 0,
        last_checkpoint: None,
        error: None,
    };
    state.store.save_backup_policy(b.project_id, &old).unwrap();
    let disabled = ProjectBackupPolicy {
        enabled: false,
        ..old.clone()
    };
    state
        .store
        .save_backup_policy(b.project_id, &disabled)
        .unwrap();
    let completed = ProjectBackupPolicy {
        last_checkpoint: Some(Uuid::new_v4()),
        next_run_at: 9999999999,
        ..old.clone()
    };
    state
        .store
        .finish_backup_policy_attempt(b.project_id, &old, &completed)
        .unwrap();
    let current = state.store.backup_policy(b.project_id).unwrap();
    assert!(!current.enabled);
    assert_eq!(current.last_checkpoint, None);
}

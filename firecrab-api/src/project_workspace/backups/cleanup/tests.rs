use super::*;
use crate::project_workspace::{
    backups::{api as backup_api, execute as backup_execute, tests::initialized},
    tests::call,
};
use axum::http::StatusCode;
use serde_json::json;

fn base(b: &Binding) -> String {
    format!(
        "/api/ai-sandbox/projects/{}/workspace/backups",
        b.project_id
    )
}
fn input(b: &Binding, ids: &[Uuid]) -> ProjectBackupCleanupRequest {
    let mut target_ids = ids.to_vec();
    target_ids.sort();
    ProjectBackupCleanupRequest {
        expected_generation: b.generation,
        target_ids,
    }
}
async fn backup(
    state: &AppState,
    b: &Binding,
    scheduled: bool,
    days: u64,
) -> ProjectBackupOperation {
    let r = backup_api::record(
        state,
        b.clone(),
        Uuid::new_v4(),
        Caller::default(),
        None,
        scheduled,
    );
    let result = backup_execute(state.clone(), r, true).await.unwrap();
    let mut r = state.store.backup_record(result.id).unwrap().unwrap();
    r.public.created_at = ai_sandbox::now().saturating_sub(days * 86400);
    state.store.save_backup_record(&r).unwrap();
    r.response()
}
async fn clean(
    state: &AppState,
    b: &Binding,
    ids: &[Uuid],
) -> Result<ProjectBackupCleanupOperation, Error> {
    execute(
        state.clone(),
        b.project_id,
        Uuid::new_v4(),
        input(b, ids),
        Caller::default(),
        None,
    )
    .await
}
#[tokio::test]
async fn cleanup_keeps_a_verified_survivor_receipts_and_current_bytes_and_rejects_deleted_restore_before_admission()
 {
    let (_dir, state, b) = initialized().await;
    let first = backup(&state, &b, false, 2).await;
    let second = backup(&state, &b, false, 0).await;
    let live = db::project_directory(&state.workspaces.root, &b, false).unwrap();
    let before = checkpoint::digest(&live.join("project.db")).unwrap();
    let cleanup_id = Uuid::new_v4();
    let (status, value) = call(
        &state,
        "PUT",
        &format!("{}/cleanups/{cleanup_id}", base(&b)),
        json!({"expectedGeneration":1,"targetIds":[first.id]}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let receipt: ProjectBackupCleanupOperation = serde_json::from_value(value).unwrap();
    let (status, history) = call(
        &state,
        "GET",
        &format!("{}/cleanups", base(&b)),
        json!({}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(history.as_array().unwrap().len(), 1);
    let (status, exact) = call(
        &state,
        "GET",
        &format!("{}/cleanups/{cleanup_id}", base(&b)),
        json!({}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(exact["id"], cleanup_id.to_string());
    assert!(!exact.to_string().contains("/project-backups/"));
    assert_ne!(
        call(
            &state,
            "GET",
            &format!("{}/cleanups/{cleanup_id}", base(&b)),
            json!({}),
            Some(Uuid::new_v4())
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, p) = call(
        &state,
        "GET",
        &format!("{}/retention/preview", base(&b)),
        json!({}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(p["automaticTargetIds"].as_array().unwrap().is_empty());
    assert_eq!(
        call(
            &state,
            "GET",
            &format!("{}/retention", base(&b)),
            json!({}),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(!receipt.pending_reconciliation);
    assert!(receipt.logical_bytes > 0);
    let root = root(&state.workspaces, b.project_id, false).unwrap();
    assert!(!root.join(first.id.to_string()).exists());
    archive::completed(&state, &root, &b, second.id).unwrap();
    assert_eq!(
        checkpoint::digest(&live.join("project.db")).unwrap(),
        before
    );
    let retained = state.store.backup_record(first.id).unwrap().unwrap();
    assert_eq!(retained.public.cleanup_id, Some(receipt.id));
    assert!(retained.public.cleaned_at.is_some());
    assert!(retained.public.manifest.is_some());
    assert!(clean(&state, &b, &[second.id]).await.is_err());
    assert!(!fenced(&state.workspaces, b.project_id).unwrap());
    let restore = backup_api::record(
        &state,
        b.clone(),
        Uuid::new_v4(),
        Caller::default(),
        Some(first.id),
        false,
    );
    let id = restore.public.id;
    assert!(matches!(
        backup_execute(state.clone(), restore, true).await,
        Err(Error::Conflict(_))
    ));
    assert!(state.store.backup_record(id).unwrap().is_none());
    assert!(!fenced(&state.workspaces, b.project_id).unwrap());
    let replacement = root.join(first.id.to_string());
    db::private_dir(&replacement).unwrap();
    fs::write(replacement.join("unrelated.txt"), b"new evidence").unwrap();
    let replay = execute(
        state.clone(),
        b.project_id,
        receipt.id,
        receipt.request.clone(),
        Caller::default(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(replay.removed_files, receipt.removed_files);
    assert!(replacement.join("unrelated.txt").exists());
    let mut changed = receipt.request;
    changed.target_ids = vec![second.id];
    assert!(
        execute(
            state,
            b.project_id,
            receipt.id,
            changed,
            Caller::default(),
            None
        )
        .await
        .is_err()
    );
}
#[tokio::test]
async fn protection_and_corrupt_survivors_prevent_any_admission_or_file_loss() {
    let (_dir, state, b) = initialized().await;
    let first = backup(&state, &b, false, 2).await;
    let second = backup(&state, &b, false, 0).await;
    let path = format!("{}/operations/{}/protection", base(&b), first.id);
    assert_eq!(
        call(
            &state,
            "PUT",
            &path,
            json!({"expectedGeneration":1,"protected":true}),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(clean(&state, &b, &[first.id]).await.is_err());
    assert_eq!(
        call(
            &state,
            "PUT",
            &path,
            json!({"expectedGeneration":1,"protected":false}),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let root = root(&state.workspaces, b.project_id, false).unwrap();
    fs::write(
        root.join(second.id.to_string()).join("project.db"),
        b"damaged survivor",
    )
    .unwrap();
    assert!(clean(&state, &b, &[first.id]).await.is_err());
    assert!(
        state
            .store
            .cleanup_records(Some(b.project_id))
            .unwrap()
            .is_empty()
    );
    assert!(!fenced(&state.workspaces, b.project_id).unwrap());
    assert!(
        root.join(first.id.to_string())
            .join("manifest.json")
            .exists()
    );
    clean(&state, &b, &[second.id]).await.unwrap();
    archive::completed(&state, &root, &b, first.id).unwrap();
}
#[tokio::test]
async fn automatic_cleanup_obeys_opt_in_age_count_manual_protection_and_stopped_vm() {
    let (_dir, state, b) = initialized().await;
    let old = backup(&state, &b, true, 20).await;
    let manual = backup(&state, &b, false, 20).await;
    let pinned = backup(&state, &b, true, 20).await;
    let recent = backup(&state, &b, true, 0).await;
    let mut r = state.store.backup_record(pinned.id).unwrap().unwrap();
    r.public.protected = true;
    state.store.save_backup_record(&r).unwrap();
    assert!(!state.store.backup_retention(b.project_id).unwrap().enabled);
    let p = ProjectBackupRetention {
        keep_latest: 1,
        max_age_days: Some(7),
        ..Default::default()
    };
    state
        .store
        .save_backup_retention(b.project_id, 1, &p)
        .unwrap();
    let preview = selection::preview(&state, b.project_id, &p).unwrap();
    assert_eq!(preview.automatic_target_ids, vec![old.id]);
    tick(state.clone()).await.unwrap();
    assert!(
        state
            .store
            .cleanup_records(Some(b.project_id))
            .unwrap()
            .is_empty()
    );
    let mut p = ProjectBackupRetention { enabled: true, ..p };
    state
        .store
        .save_backup_retention(b.project_id, 1, &p)
        .unwrap();
    state.vms.lock().unwrap().get_mut(&b.vm_id).unwrap().state = VmState::Running;
    tick(state.clone()).await.unwrap();
    assert!(
        state
            .store
            .backup_retention(b.project_id)
            .unwrap()
            .error
            .is_some()
    );
    assert!(!fenced(&state.workspaces, b.project_id).unwrap());
    state.vms.lock().unwrap().get_mut(&b.vm_id).unwrap().state = VmState::Stopped;
    p.next_run_at = 0;
    state
        .store
        .save_backup_retention(b.project_id, 1, &p)
        .unwrap();
    tick(state.clone()).await.unwrap();
    assert!(
        state
            .store
            .backup_record(old.id)
            .unwrap()
            .unwrap()
            .public
            .cleaned_at
            .is_some()
    );
    for id in [manual.id, pinned.id, recent.id] {
        assert!(
            state
                .store
                .backup_record(id)
                .unwrap()
                .unwrap()
                .public
                .cleanup_id
                .is_none()
        );
    }
    let p = state.store.backup_retention(b.project_id).unwrap();
    assert!(p.last_cleanup_at.is_some());
    assert!(p.error.is_none());
    let mut with_manual = p.clone();
    with_manual.include_manual = true;
    assert_eq!(
        selection::preview(&state, b.project_id, &with_manual)
            .unwrap()
            .automatic_target_ids,
        vec![manual.id]
    );
    with_manual.max_age_days = Some(30);
    assert!(
        selection::preview(&state, b.project_id, &with_manual)
            .unwrap()
            .automatic_target_ids
            .is_empty()
    );
    let newer = ProjectBackupRetention {
        enabled: false,
        keep_latest: 12,
        ..p.clone()
    };
    state
        .store
        .save_backup_retention(b.project_id, 1, &newer)
        .unwrap();
    state
        .store
        .finish_retention_attempt(b.project_id, &p, &with_manual)
        .unwrap();
    assert_eq!(state.store.backup_retention(b.project_id).unwrap(), newer);
    assert!(matches!(
        execute(
            state.clone(),
            b.project_id,
            Uuid::new_v4(),
            input(&b, &[manual.id]),
            Caller::default(),
            Some(p)
        )
        .await,
        Err(Error::Busy)
    ));
    assert!(
        state
            .store
            .backup_record(manual.id)
            .unwrap()
            .unwrap()
            .public
            .cleanup_id
            .is_none()
    );
}
#[tokio::test]
async fn interrupted_cleanup_restarts_with_the_same_plan_and_never_redeletes_newer_files() {
    let (dir, state, b) = initialized().await;
    let survivor = backup(&state, &b, false, 0).await;
    let obsolete = backup(&state, &b, false, 3).await;
    let restore = backup_execute(
        state.clone(),
        backup_api::record(
            &state,
            b.clone(),
            Uuid::new_v4(),
            Caller::default(),
            Some(survivor.id),
            false,
        ),
        true,
    )
    .await
    .unwrap();
    let b = raw_binding(&state, b.project_id).unwrap();
    let templates =
        crate::templates::TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
    let second = AppState::with_db_file(templates, dir.path().join("control.db"))
        .await
        .unwrap();
    assert!(matches!(
        clean(&second, &b, &[obsolete.id]).await,
        Err(Error::Busy)
    ));
    drop(second);
    let id = Uuid::new_v4();
    let mut r = prepare_record(
        &state,
        b.clone(),
        id,
        input(&b, &[obsolete.id, restore.id]),
        Caller::default(),
        None,
    )
    .unwrap();
    let root = root(&state.workspaces, b.project_id, true).unwrap();
    install_fence(&root.join("fence.json"), id).unwrap();
    files::verify(&state, &r).unwrap();
    assert!(files::remove_one(&state, &mut r).unwrap());
    assert!(state.store.workspace_vm_backup_pending(b.vm_id).unwrap());
    assert!(state.workspaces.open_owned(&b, false).is_err());
    assert!(
        backup_execute(
            state.clone(),
            backup_api::record(
                &state,
                b.clone(),
                Uuid::new_v4(),
                Caller::default(),
                None,
                false
            ),
            true
        )
        .await
        .is_err()
    );
    drop(state);
    let templates =
        crate::templates::TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
    let resumed = AppState::with_db_file(templates, dir.path().join("control.db"))
        .await
        .unwrap();
    #[cfg(target_os = "linux")]
    let _controller_owner = resumed.store.scheduler_lock().unwrap();
    resumed.store.interrupt_sandbox_tasks().unwrap();
    tick(resumed.clone()).await.unwrap();
    let mut finished = resumed.store.cleanup_record(id).unwrap().unwrap();
    assert!(!finished.active);
    assert_eq!(finished.public.phase, "complete");
    assert!(!fenced(&resumed.workspaces, b.project_id).unwrap());
    assert!(
        resumed
            .store
            .backup_record(restore.id)
            .unwrap()
            .unwrap()
            .public
            .cleaned_at
            .is_some()
    );
    assert_eq!(raw_binding(&resumed, b.project_id).unwrap().generation, 2);
    assert_eq!(
        resumed.store.workspace_generation(b.project_id).unwrap(),
        Some((2, true))
    );
    let live = db::project_directory(&resumed.workspaces.root, &b, false).unwrap();
    assert!(
        !live
            .join(format!("project.db.before-{}", restore.id))
            .exists()
    );
    archive::completed(&resumed, &root, &b, survivor.id).unwrap();
    let replacement = root.join(obsolete.id.to_string());
    db::private_dir(&replacement).unwrap();
    fs::write(
        replacement.join("project.db"),
        b"replacement must survive exact replay",
    )
    .unwrap();
    // Crash after the control commit but before maintenance-gate release.
    finished.active = true;
    resumed.store.save_cleanup_record(&finished).unwrap();
    install_fence(&root.join("fence.json"), id).unwrap();
    execute(
        resumed.clone(),
        b.project_id,
        id,
        finished.public.request,
        Caller::default(),
        None,
    )
    .await
    .unwrap();
    assert!(replacement.join("project.db").exists());
}
#[tokio::test]
async fn unplanned_files_aliases_and_changed_admitted_files_fail_closed_until_explicit_retry() {
    let (_dir, state, b) = initialized().await;
    let first = backup(&state, &b, false, 2).await;
    backup(&state, &b, false, 0).await;
    let root = root(&state.workspaces, b.project_id, true).unwrap();
    let archive = root.join(first.id.to_string());
    let unknown = archive.join("notes.txt");
    fs::write(&unknown, b"must be inspected").unwrap();
    assert!(clean(&state, &b, &[first.id]).await.is_err());
    assert!(
        state
            .store
            .cleanup_records(Some(b.project_id))
            .unwrap()
            .is_empty()
    );
    fs::remove_file(unknown).unwrap();
    let database = archive.join("project.db");
    let bytes = fs::read(&database).unwrap();
    fs::remove_file(&database).unwrap();
    std::os::unix::fs::symlink("manifest.json", &database).unwrap();
    assert!(clean(&state, &b, &[first.id]).await.is_err());
    fs::remove_file(&database).unwrap();
    fs::write(&database, &bytes).unwrap();
    let id = Uuid::new_v4();
    let r = prepare_record(
        &state,
        b.clone(),
        id,
        input(&b, &[first.id]),
        Caller::default(),
        None,
    )
    .unwrap();
    install_fence(&root.join("fence.json"), id).unwrap();
    fs::write(&database, b"changed admitted artifact").unwrap();
    tick(state.clone()).await.unwrap();
    let failed = state.store.cleanup_record(id).unwrap().unwrap();
    assert!(failed.active);
    assert!(failed.public.error.is_some());
    assert_eq!(failed.public.removed_files, 0);
    assert!(archive.join("control.db").exists());
    assert!(
        state
            .store
            .backup_record(first.id)
            .unwrap()
            .unwrap()
            .public
            .cleanup_id
            .is_some()
    );
    assert_eq!(
        call(
            &state,
            "DELETE",
            &format!("{}/operations/{}", base(&b), first.id),
            json!({}),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    fs::write(&database, &bytes).unwrap();
    let (status, result) = call(
        &state,
        "POST",
        &format!("{}/cleanups/{id}/retry", base(&b)),
        json!({}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result: ProjectBackupCleanupOperation = serde_json::from_value(result).unwrap();
    assert_eq!(result.request, r.public.request);
    assert!(!result.pending_reconciliation);
    assert!(!archive.exists());
}
#[tokio::test]
async fn unauthorized_invalid_stale_and_pending_operations_cannot_change_cleanup_state() {
    let (_dir, state, b) = initialized().await;
    let path = format!("{}/retention", base(&b));
    let valid = json!({"expectedGeneration":1,"enabled":true,"keepLatest":2,"maxAgeDays":null,"includeManual":false});
    assert_eq!(
        call(&state, "PUT", &path, valid.clone(), Some(Uuid::new_v4()))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    for (key, value) in [
        ("keepLatest", json!(0)),
        ("maxAgeDays", json!(0)),
        ("expectedGeneration", json!(0)),
    ] {
        let mut invalid = valid.clone();
        invalid[key] = value;
        assert_eq!(
            call(&state, "PUT", &path, invalid, None).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let mut stale = valid.clone();
    stale["expectedGeneration"] = json!(99);
    assert_eq!(
        call(&state, "PUT", &path, stale, None).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&state, "PUT", &path, valid, None).await.0,
        StatusCode::OK
    );
    let id = Uuid::new_v4();
    for targets in [vec![], vec![id, id], vec![Uuid::nil()]] {
        let body = json!({"expectedGeneration":1,"targetIds":targets});
        assert_eq!(
            call(
                &state,
                "PUT",
                &format!("{}/cleanups/{id}", base(&b)),
                body,
                None
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let pending = backup_api::record(
        &state,
        b.clone(),
        Uuid::new_v4(),
        Caller::default(),
        None,
        false,
    );
    state.store.save_backup_record(&pending).unwrap();
    assert!(clean(&state, &b, &[pending.public.id]).await.is_err());
    assert!(!fenced(&state.workspaces, b.project_id).unwrap());
}
#[tokio::test]
async fn aborted_partial_cleanup_removes_only_owned_temporary_copies_and_keeps_the_receipt() {
    let (_dir, state, b) = initialized().await;
    let mut r = backup_api::record(
        &state,
        b.clone(),
        Uuid::new_v4(),
        Caller::default(),
        None,
        false,
    );
    r.public.phase = "aborted".into();
    state.store.save_backup_record(&r).unwrap();
    let root = root(&state.workspaces, b.project_id, true).unwrap();
    let path = root.join(r.public.id.to_string());
    db::private_dir(&path).unwrap();
    let mut file = db::private_file(&path.join("data.ext4"), true).unwrap();
    file.write_all(b"unfinished copy").unwrap();
    drop(file);
    let receipt = clean(&state, &b, &[r.public.id]).await.unwrap();
    assert_eq!(receipt.logical_bytes, 15);
    assert!(!path.exists());
    assert!(
        state
            .store
            .backup_record(r.public.id)
            .unwrap()
            .unwrap()
            .public
            .cleaned_at
            .is_some()
    );
    assert!(state.workspaces.open_owned(&b, false).is_ok());
}

#[tokio::test]
async fn aborted_restore_staging_cleanup_never_unlinks_the_live_database_or_volume() {
    let (_dir, state, b) = initialized().await;
    let source = backup(&state, &b, false, 0).await;
    let mut r = backup_api::record(
        &state,
        b.clone(),
        Uuid::new_v4(),
        Caller::default(),
        Some(source.id),
        false,
    );
    r.public.phase = "aborted".into();
    state.store.save_backup_record(&r).unwrap();
    let live = db::project_directory(&state.workspaces.root, &b, false).unwrap();
    let volume =
        block_storage::disk_path(&state, &state.store.block_storage(b.volume_id).unwrap()).unwrap();
    let db_stage = live.join(format!("project.db.restore-{}", r.public.id));
    let volume_stage = volume.with_file_name(format!(
        "{}.restore-{}",
        volume.file_name().unwrap().to_str().unwrap(),
        r.public.id
    ));
    for path in [&db_stage, &volume_stage] {
        let mut file = db::private_file(path, true).unwrap();
        file.write_all(b"uninstalled staging copy").unwrap();
    }
    let before_db = checkpoint::digest(&live.join("project.db")).unwrap();
    let before_volume = checkpoint::digest(&volume).unwrap();
    clean(&state, &b, &[r.public.id]).await.unwrap();
    assert!(!db_stage.exists());
    assert!(!volume_stage.exists());
    assert_eq!(
        checkpoint::digest(&live.join("project.db")).unwrap(),
        before_db
    );
    assert_eq!(checkpoint::digest(&volume).unwrap(), before_volume);
    archive::completed(
        &state,
        &root(&state.workspaces, b.project_id, false).unwrap(),
        &b,
        source.id,
    )
    .unwrap();
}

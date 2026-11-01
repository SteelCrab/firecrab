use super::tests::saved;
use super::*;
use crate::project_workspace::{Binding, db::ProjectDb};
use firecrab_api_types::{
    SandboxEnvironmentRef, SandboxExecutionMode, SandboxRuntimeRef, SandboxTaskKind,
};
fn project(store: &Store) -> (Binding, ProjectDb) {
    let mut b = Binding {
        project_id: Uuid::new_v4(),
        database_id: Uuid::new_v4(),
        volume_id: Uuid::new_v4(),
        vm_id: Uuid::new_v4(),
        generation: 1,
        provisioning: true,
        verified_runtime_id: None,
    };
    let db = store.workspaces.open_owned(&b, true).unwrap();
    db.mark_initialized().unwrap();
    b.provisioning = false;
    store
        .lock()
        .execute(
            "INSERT INTO project_workspaces VALUES(?1,?2,?3,?4,1,0,NULL)",
            params![
                b.project_id.to_string(),
                b.database_id.to_string(),
                b.volume_id.to_string(),
                b.vm_id.to_string()
            ],
        )
        .unwrap();
    (b, db)
}
fn count(c: &rusqlite::Connection, table: &str) -> i64 {
    c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
#[test]
fn migration_keeps_one_canonical_copy_and_routes_writes_history_and_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let (b, db) = project(&store);
    let mut t = saved(b.project_id, 1);
    let runtime = SandboxRuntimeRef {
        vm_id: b.vm_id,
        runtime_id: Uuid::new_v4(),
    };
    t.task.runtime = Some(runtime);
    t.guest = Some(runtime);
    t.project.vm_id = Some(b.vm_id);
    t.task.request.kind = SandboxTaskKind::Code;
    t.execution_mode = Some(SandboxExecutionMode::PersistentWorkspace);
    let environment = Uuid::new_v4();
    t.task.environment = Some(SandboxEnvironmentRef {
        id: environment,
        generation: Uuid::new_v4(),
        role: "backend".into(),
    });
    let other = saved(Uuid::new_v4(), 2);
    store.save_sandbox_task(&t).unwrap();
    store.save_sandbox_task(&other).unwrap();
    let cursor = store.sandbox_cursor(b.project_id, t.task.id).unwrap();
    let original = serde_json::to_value(&t).unwrap();
    let op = Uuid::new_v4();
    let checkpoint = Uuid::new_v4();
    store
        .migrate_sandbox_tasks(&b, &db, op, checkpoint)
        .unwrap();
    assert_eq!(count(&store.lock(), "ai_sandbox_tasks"), 1);
    assert_eq!(count(&db.conn, "ai_sandbox_tasks"), 1);
    assert!(
        db.directory
            .join("sandbox-migrations")
            .join(op.to_string())
            .join("source.db")
            .is_file()
    );
    drop(db);
    assert_eq!(
        serde_json::to_value(store.sandbox_task(t.task.id).unwrap().unwrap()).unwrap(),
        original
    );
    assert_eq!(
        store.sandbox_cursor(b.project_id, t.task.id).unwrap(),
        cursor
    );
    assert_eq!(store.sandbox_history(b.project_id).unwrap().len(), 1);
    t.task.state = SandboxTaskState::InProgress;
    store.save_sandbox_task(&t).unwrap();
    assert!(store.sandbox_project_busy(b.project_id).unwrap());
    assert!(store.sandbox_runtime_busy(b.vm_id, Uuid::new_v4()).unwrap());
    assert!(!store.sandbox_runtime_busy(b.vm_id, t.task.id).unwrap());
    assert_eq!(
        store
            .sandbox_workspace_origin(b.vm_id)
            .unwrap()
            .unwrap()
            .task
            .id,
        t.task.id
    );
    assert_eq!(store.sandbox_retry_tasks(t.task.id).unwrap().len(), 1);
    assert_eq!(
        store.sandbox_environment_task_ids(environment).unwrap(),
        [t.task.id]
    );
    assert_eq!(store.sandbox_pending_count().unwrap(), 1);
    store.interrupt_sandbox_tasks().unwrap();
    assert_eq!(
        store.sandbox_task(t.task.id).unwrap().unwrap().task.state,
        SandboxTaskState::Interrupted
    );
    let mut forged = t.clone();
    forged.task.attempt_id = Uuid::new_v4();
    assert!(store.save_sandbox_task(&forged).is_err());
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.sandbox_task_count(b.project_id).unwrap(), 1);
    assert_eq!(
        store.sandbox_task(t.task.id).unwrap().unwrap().task.state,
        SandboxTaskState::Interrupted
    );
    assert!(store.sandbox_task(other.task.id).unwrap().is_some());
}
#[test]
fn interrupted_target_copy_freezes_old_writers_and_resumes_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let (b, db) = project(&store);
    let t = saved(b.project_id, 1);
    store.save_sandbox_task(&t).unwrap();
    db.conn.execute_batch("CREATE TRIGGER interrupt_copy BEFORE INSERT ON ai_sandbox_tasks BEGIN SELECT RAISE(ABORT,'interrupted copy'); END;").unwrap();
    let op = Uuid::new_v4();
    let cp = Uuid::new_v4();
    assert!(store.migrate_sandbox_tasks(&b, &db, op, cp).is_err());
    assert!(store.sandbox_project_busy(b.project_id).unwrap());
    assert!(store.save_sandbox_task(&t).is_err());
    assert!(
        store
            .lock()
            .execute(
                "UPDATE ai_sandbox_tasks SET spec=spec WHERE id=?1",
                [t.task.id.to_string()]
            )
            .is_err()
    );
    assert_eq!(count(&db.conn, "sandbox_migration"), 0);
    assert!(store.sandbox_task(t.task.id).unwrap().is_some());
    drop(db);
    drop(store);
    let store = Store::open(&path).unwrap();
    let db = store.workspaces.open_owned(&b, false).unwrap();
    db.conn
        .execute_batch("DROP TRIGGER interrupt_copy")
        .unwrap();
    assert!(
        store
            .migrate_sandbox_tasks(&b, &db, Uuid::new_v4(), cp)
            .is_err()
    );
    store.migrate_sandbox_tasks(&b, &db, op, cp).unwrap();
    drop(db);
    assert_eq!(
        store.sandbox_storage_status(b.project_id).unwrap()["storage"],
        "project"
    );
    assert_eq!(store.sandbox_task_count(b.project_id).unwrap(), 1);
}
#[test]
fn interrupted_cutover_revalidates_the_copied_records_before_deleting_source() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state.db")).unwrap();
    let (b, db) = project(&store);
    let t = saved(b.project_id, 1);
    store.save_sandbox_task(&t).unwrap();
    store.lock().execute_batch("CREATE TRIGGER interrupt_cutover BEFORE UPDATE ON sandbox_task_routes WHEN NEW.phase='cutover' BEGIN SELECT RAISE(ABORT,'interrupted cutover'); END;").unwrap();
    let op = Uuid::new_v4();
    let cp = Uuid::new_v4();
    assert!(store.migrate_sandbox_tasks(&b, &db, op, cp).is_err());
    assert_eq!(count(&db.conn, "sandbox_migration"), 1);
    assert_eq!(count(&store.lock(), "ai_sandbox_tasks"), 1);
    db.conn
        .execute(
            "UPDATE ai_sandbox_tasks SET spec=json_set(spec,'$.task.result','tampered')",
            [],
        )
        .unwrap();
    index_task(
        &db.conn,
        store
            .sandbox_cursor(b.project_id, t.task.id)
            .unwrap()
            .unwrap(),
        &t,
    )
    .unwrap();
    store
        .lock()
        .execute_batch("DROP TRIGGER interrupt_cutover")
        .unwrap();
    assert!(store.migrate_sandbox_tasks(&b, &db, op, cp).is_err());
    assert_eq!(count(&store.lock(), "ai_sandbox_tasks"), 1);
    assert_eq!(
        store.sandbox_task(t.task.id).unwrap().unwrap().task.result,
        t.task.result
    );
}
#[test]
fn completed_cutover_retry_is_idempotent_and_never_resurrects_controller_records() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state.db")).unwrap();
    let (b, db) = project(&store);
    let t = saved(b.project_id, 1);
    store.save_sandbox_task(&t).unwrap();
    let op = Uuid::new_v4();
    let cp = Uuid::new_v4();
    store.migrate_sandbox_tasks(&b, &db, op, cp).unwrap();
    store.migrate_sandbox_tasks(&b, &db, op, cp).unwrap();
    assert_eq!(count(&db.conn, "ai_sandbox_tasks"), 1);
    assert_eq!(count(&store.lock(), "ai_sandbox_tasks"), 0);
    assert!(
        store
            .lock()
            .execute(
                "INSERT INTO ai_sandbox_tasks VALUES(?1,?2,?3)",
                params![
                    t.task.id.to_string(),
                    b.project_id.to_string(),
                    encode(&t).unwrap()
                ]
            )
            .is_err()
    );
    let file = db.directory.join("project.db");
    drop(db);
    std::fs::remove_file(&file).unwrap();
    assert!(store.sandbox_task(t.task.id).is_err());
    assert!(!file.exists());
    assert!(store.sandbox_pending_count().is_err());
}
#[test]
fn active_or_unresolved_tasks_cannot_be_migrated() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state.db")).unwrap();
    let (b, db) = project(&store);
    let mut t = saved(b.project_id, 1);
    t.task.state = SandboxTaskState::Accepted;
    store.save_sandbox_task(&t).unwrap();
    assert!(
        store
            .migrate_sandbox_tasks(&b, &db, Uuid::new_v4(), Uuid::new_v4())
            .is_err()
    );
    assert_eq!(
        store.sandbox_storage_status(b.project_id).unwrap()["storage"],
        "controller"
    );
    assert_eq!(count(&db.conn, "ai_sandbox_tasks"), 0);
}
#[test]
fn global_ordinals_and_attempt_identity_remain_unique_across_project_databases() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state.db")).unwrap();
    let (a, da) = project(&store);
    let (b, db) = project(&store);
    let first = saved(a.project_id, 1);
    store.save_sandbox_task(&first).unwrap();
    store
        .migrate_sandbox_tasks(&a, &da, Uuid::new_v4(), Uuid::new_v4())
        .unwrap();
    store
        .migrate_sandbox_tasks(&b, &db, Uuid::new_v4(), Uuid::new_v4())
        .unwrap();
    drop(da);
    drop(db);
    let second = saved(b.project_id, 2);
    store.save_sandbox_task(&second).unwrap();
    let third = saved(a.project_id, 3);
    store.save_sandbox_task(&third).unwrap();
    assert!(
        store.sandbox_cursor(a.project_id, first.task.id).unwrap()
            < store.sandbox_cursor(b.project_id, second.task.id).unwrap()
    );
    assert!(
        store.sandbox_cursor(b.project_id, second.task.id).unwrap()
            < store.sandbox_cursor(a.project_id, third.task.id).unwrap()
    );
    let mut forged = second.clone();
    forged.task.id = Uuid::new_v4();
    forged.task.project_id = a.project_id;
    assert!(store.save_sandbox_task(&forged).is_err());
    assert_eq!(store.sandbox_task_count(a.project_id).unwrap(), 2);
    assert_eq!(store.sandbox_task_count(b.project_id).unwrap(), 1);
}

#[test]
fn mismatched_target_or_unpaired_controller_restore_never_creates_a_second_authority() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state.db")).unwrap();
    let (a, da) = project(&store);
    let (b, db) = project(&store);
    let t = saved(a.project_id, 1);
    store.save_sandbox_task(&t).unwrap();
    assert!(
        store
            .migrate_sandbox_tasks(&a, &db, Uuid::new_v4(), Uuid::new_v4())
            .is_err()
    );
    assert_eq!(count(&db.conn, "sandbox_migration"), 0);
    drop(db);
    store
        .migrate_sandbox_tasks(&a, &da, Uuid::new_v4(), Uuid::new_v4())
        .unwrap();
    drop(da);
    store
        .lock()
        .execute(
            "DELETE FROM sandbox_task_routes WHERE project_id=?1",
            [a.project_id.to_string()],
        )
        .unwrap();
    assert!(store.sandbox_task(t.task.id).is_err());
    assert!(store.save_sandbox_task(&t).is_err());
    assert!(store.sandbox_pending_count().is_err());
    assert_eq!(
        store.sandbox_storage_status(b.project_id).unwrap()["storage"],
        "controller"
    );
}

#[test]
fn an_interrupted_registry_commit_reuses_the_verified_target_copy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let (b, db) = project(&store);
    let task = saved(b.project_id, 1);
    store.save_sandbox_task(&task).unwrap();
    store.lock().execute_batch("CREATE TRIGGER pause_cutover BEFORE UPDATE ON sandbox_task_routes WHEN NEW.phase='cutover' BEGIN SELECT RAISE(ABORT,'pause'); END;").unwrap();
    let op = Uuid::new_v4();
    let cp = Uuid::new_v4();
    assert!(store.migrate_sandbox_tasks(&b, &db, op, cp).is_err());
    assert_eq!(count(&db.conn, "ai_sandbox_tasks"), 1);
    drop(db);
    drop(store);
    let store = Store::open(&path).unwrap();
    store
        .lock()
        .execute_batch("DROP TRIGGER pause_cutover")
        .unwrap();
    let db = store.workspaces.open_owned(&b, false).unwrap();
    store.migrate_sandbox_tasks(&b, &db, op, cp).unwrap();
    assert_eq!(count(&db.conn, "ai_sandbox_tasks"), 1);
    assert_eq!(count(&store.lock(), "ai_sandbox_tasks"), 0);
    drop(db);
    assert_eq!(
        store
            .sandbox_task(task.task.id)
            .unwrap()
            .unwrap()
            .task
            .result,
        task.task.result
    );
}

#[test]
fn damaged_backup_and_wrong_registry_identity_leave_the_source_frozen_and_readable() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state.db")).unwrap();
    let (binding, db) = project(&store);
    let task = saved(binding.project_id, 1);
    store.save_sandbox_task(&task).unwrap();
    db.conn.execute_batch("CREATE TRIGGER pause_copy BEFORE INSERT ON ai_sandbox_tasks BEGIN SELECT RAISE(ABORT,'pause'); END;").unwrap();
    let operation = Uuid::new_v4();
    let checkpoint = Uuid::new_v4();
    assert!(
        store
            .migrate_sandbox_tasks(&binding, &db, operation, checkpoint)
            .is_err()
    );
    db.conn.execute_batch("DROP TRIGGER pause_copy").unwrap();
    let checksum = db
        .directory
        .join("sandbox-migrations")
        .join(operation.to_string())
        .join("source.sha256");
    std::fs::write(checksum, "0".repeat(64)).unwrap();
    assert!(
        store
            .migrate_sandbox_tasks(&binding, &db, operation, checkpoint)
            .is_err()
    );
    assert_eq!(count(&store.lock(), "ai_sandbox_tasks"), 1);
    assert_eq!(count(&db.conn, "ai_sandbox_tasks"), 0);
    drop(db);
    assert!(store.sandbox_task(task.task.id).unwrap().is_some());
    assert!(store.save_sandbox_task(&task).is_err());
    store
        .lock()
        .execute(
            "UPDATE sandbox_task_routes SET document=json_set(document,'$.binding.project_id',?1)",
            [Uuid::new_v4().to_string()],
        )
        .unwrap();
    assert!(store.sandbox_storage_status(binding.project_id).is_err());
    assert!(store.sandbox_pending_count().is_err());
}

#[tokio::test]
async fn agent_checkpoint_waits_for_workspace_owner_and_never_retries_a_missing_target() {
    let dir = tempfile::tempdir().unwrap();
    let registry =
        crate::templates::TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
    let state = crate::state::AppState::with_db_file(registry, dir.path().join("state.db"))
        .await
        .unwrap();
    let (binding, db) = project(&state.store);
    let mut task = saved(binding.project_id, 1);
    state.store.save_sandbox_task(&task).unwrap();
    state
        .store
        .migrate_sandbox_tasks(&binding, &db, Uuid::new_v4(), Uuid::new_v4())
        .unwrap();
    drop(db);
    let owner = state.workspaces.open_owned(&binding, false).unwrap();
    task.task.state = SandboxTaskState::InProgress;
    assert!(matches!(
        state.store.save_sandbox_task(&task),
        Err(PersistenceError::ProjectDatabaseBusy)
    ));
    let local = state.clone();
    let saved = task.clone();
    let pending = tokio::spawn(async move { crate::ai_sandbox::save(&local, &saved).await });
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    assert!(
        !pending.is_finished(),
        "owner contention must not fail the agent checkpoint immediately"
    );
    let path = owner.directory.join("project.db");
    drop(owner);
    tokio::time::timeout(std::time::Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        state
            .store
            .sandbox_task(task.task.id)
            .unwrap()
            .unwrap()
            .task
            .state,
        SandboxTaskState::InProgress
    );
    std::fs::remove_file(&path).unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            crate::ai_sandbox::save(&state, &task)
        )
        .await
        .unwrap()
        .is_err()
    );
    assert!(!path.exists());
}

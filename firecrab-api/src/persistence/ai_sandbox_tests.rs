use super::*;
use firecrab_api_types::*;

fn saved(project: Uuid, index: u64) -> SavedTask {
    SavedTask {
        caller: Default::default(),
        project: SandboxProjectSpec {
            context: Default::default(),
            environment_id: None,
            name: "Project".into(),
            source_path: None,
            vm_id: None,
            selection: None,
            instructions: String::new(),
        },
        workspace: None,
        guest: None,
        guest_pending: false,
        guest_command: None,
        guest_staged: false,
        results_exported: false,
        execution_mode: None,
        task: SandboxTask {
            git_commit: None,
            retry_root: None,
            retry_number: 0,
            dependency_results: vec![],
            tests: vec![],
            artifacts: vec![],
            environment: None,
            id: Uuid::new_v4(),
            project_id: project,
            attempt_id: Uuid::new_v4(),
            runtime: None,
            request: SandboxTaskInput {
                coordination: Default::default(),
                environment_role: None,
                kind: SandboxTaskKind::Chat,
                input: format!("input {index}"),
                selection: None,
                retry_of: None,
            },
            selection: AiModelSelection {
                connection_id: Uuid::new_v4(),
                model: "fixture".into(),
            },
            state: SandboxTaskState::Result,
            created_at: index,
            updated_at: index,
            result: format!("result {index}"),
            error_code: None,
            error: None,
            events: vec![SandboxEvent {
                sequence: 0,
                at: index,
                name: "read_file".into(),
                input: String::new(),
                output: "large-private-tool-output".repeat(1000),
                stderr: String::new(),
                exit_code: None,
            }],
            changes: vec![SandboxFileChange {
                path: "src/file".into(),
                before: Some("large-private-file-before".into()),
                after: Some("large-private-file-after".into()),
                before_mode: Some(0o644),
                after_mode: Some(0o644),
                applied: false,
            }],
        },
    }
}

#[test]
fn attempt_references_are_immutable_and_duplicate_attempts_roll_back() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state.db")).unwrap();
    let original = saved(Uuid::new_v4(), 1);
    store.save_sandbox_task(&original).unwrap();
    let mut variants = Vec::new();
    let mut changed = original.clone();
    changed.task.project_id = Uuid::new_v4();
    variants.push(changed);
    let mut changed = original.clone();
    changed.task.attempt_id = Uuid::new_v4();
    variants.push(changed);
    let mut changed = serde_json::to_value(&original).unwrap();
    changed["caller"]["principal"] = serde_json::json!({"host":Uuid::new_v4()});
    variants.push(serde_json::from_value(changed).unwrap());
    let mut changed = original.clone();
    changed.task.id = Uuid::new_v4();
    variants.push(changed);
    let mut changed = original.clone();
    changed.task.request.coordination.retry_limit = 0;
    variants.push(changed);
    let mut changed = original.clone();
    changed
        .project
        .context
        .references
        .push("other-context.md".into());
    variants.push(changed);
    let mut changed = original.clone();
    changed.task.selection.model = "another-model".into();
    variants.push(changed);
    for changed in variants {
        assert!(store.save_sandbox_task(&changed).is_err());
    }
    let persisted = store.sandbox_task(original.task.id).unwrap().unwrap();
    assert_eq!(persisted.task.project_id, original.task.project_id);
    assert_eq!(persisted.task.attempt_id, original.task.attempt_id);
    assert_eq!(
        store.sandbox_task_count(original.task.project_id).unwrap(),
        1
    );
    let mut code = saved(Uuid::new_v4(), 2);
    let runtime = SandboxRuntimeRef {
        vm_id: Uuid::new_v4(),
        runtime_id: Uuid::new_v4(),
    };
    code.task.request.kind = SandboxTaskKind::Code;
    code.task.runtime = Some(runtime);
    code.project.vm_id = Some(runtime.vm_id);
    code.guest = Some(runtime);
    store.save_sandbox_task(&code).unwrap();
    code.task.runtime.as_mut().unwrap().runtime_id = Uuid::new_v4();
    assert!(store.save_sandbox_task(&code).is_err());
    assert_eq!(
        store
            .sandbox_task(code.task.id)
            .unwrap()
            .unwrap()
            .task
            .runtime,
        Some(runtime)
    );
    code.task.runtime = Some(runtime);
    code.guest.as_mut().unwrap().vm_id = Uuid::new_v4();
    assert!(store.save_sandbox_task(&code).is_err());
}

#[test]
fn vm_claim_is_global_and_uncertain_guest_work_blocks_other_attempts_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fixture.db");
    let vm = Uuid::new_v4();
    let mut task = saved(Uuid::new_v4(), 1);
    let runtime = SandboxRuntimeRef {
        vm_id: vm,
        runtime_id: Uuid::new_v4(),
    };
    task.task.runtime = Some(runtime);
    task.task.request.kind = SandboxTaskKind::Code;
    task.project.vm_id = Some(vm);
    task.guest = Some(runtime);
    task.task.state = SandboxTaskState::InProgress;
    {
        let store = Store::open(&path).unwrap();
        store.save_sandbox_task(&task).unwrap();
        assert!(!store.sandbox_runtime_busy(vm, task.task.id).unwrap());
        assert!(store.sandbox_runtime_busy(vm, Uuid::new_v4()).unwrap());
        task.task.state = SandboxTaskState::Interrupted;
        task.guest_pending = true;
        store.save_sandbox_task(&task).unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert!(store.sandbox_runtime_busy(vm, Uuid::new_v4()).unwrap());
    task.task.state = SandboxTaskState::Cancelled;
    task.guest_pending = false;
    store.save_sandbox_task(&task).unwrap();
    assert!(!store.sandbox_runtime_busy(vm, Uuid::new_v4()).unwrap());
    task.results_exported = true;
    task.task.state = SandboxTaskState::InProgress;
    assert!(store.save_sandbox_task(&task).is_err());
}

#[test]
fn legacy_local_records_backfill_runtime_identity_without_remote_access() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let mut task = saved(Uuid::new_v4(), 1);
    let runtime = SandboxRuntimeRef {
        vm_id: Uuid::new_v4(),
        runtime_id: Uuid::new_v4(),
    };
    task.task.request.kind = SandboxTaskKind::Code;
    task.task.runtime = Some(runtime);
    task.guest = Some(runtime);
    task.project.vm_id = Some(runtime.vm_id);
    let project = SandboxProject {
        id: task.task.project_id,
        owner: SandboxPrincipal::Local,
        spec: task.project.clone(),
        created_at: 0,
    };
    store.save_sandbox_project(&project).unwrap();
    store.save_sandbox_task(&task).unwrap();
    let mut legacy = serde_json::to_value(&task).unwrap();
    legacy.as_object_mut().unwrap().remove("caller");
    legacy["task"].as_object_mut().unwrap().remove("runtime");
    legacy["guest"] = serde_json::json!({"vm_id":runtime.vm_id,"runtime_id":runtime.runtime_id});
    store
        .lock()
        .execute(
            "UPDATE ai_sandbox_tasks SET spec=?1 WHERE id=?2",
            params![legacy.to_string(), task.task.id.to_string()],
        )
        .unwrap();
    store
        .lock()
        .execute("DELETE FROM ai_sandbox_attempts", [])
        .unwrap();
    let mut legacy = serde_json::to_value(&project).unwrap();
    legacy.as_object_mut().unwrap().remove("owner");
    store
        .lock()
        .execute(
            "UPDATE ai_sandbox_projects SET spec=?1 WHERE id=?2",
            params![legacy.to_string(), project.id.to_string()],
        )
        .unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    let loaded = store.sandbox_task(task.task.id).unwrap().unwrap();
    assert_eq!(loaded.task.runtime, Some(runtime));
    assert_eq!(loaded.caller.principal, SandboxPrincipal::Local);
    assert_eq!(
        store.sandbox_project(project.id).unwrap().unwrap().owner,
        SandboxPrincipal::Local
    );
    assert!(
        store
            .sandbox_access(Uuid::new_v4())
            .unwrap()
            .grants
            .is_empty()
    );
    let mut changed = project;
    changed.owner = SandboxPrincipal::Host(Uuid::new_v4());
    assert!(store.save_sandbox_project(&changed).is_err());
    store.delete_sandbox_project(changed.id).unwrap();
    assert!(store.sandbox_task(task.task.id).unwrap().is_none());
    assert_eq!(
        store
            .lock()
            .query_row("SELECT COUNT(*) FROM ai_sandbox_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn summary_index_pages_and_polls_without_returning_retained_tool_or_file_payloads() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state.db")).unwrap();
    let project = Uuid::new_v4();
    let mut ids = Vec::new();
    for index in 0..55 {
        let task = saved(project, index);
        ids.push(task.task.id);
        store.save_sandbox_task(&task).unwrap();
    }
    let recent = store.sandbox_summaries(project, None, None).unwrap();
    assert_eq!(recent.len(), 50);
    assert_eq!(recent[0].request.input, "input 5");
    assert_eq!(recent[49].request.input, "input 54");
    assert_eq!(recent[0].event_count, 1);
    assert_eq!(recent[0].change_count, 1);
    assert_eq!(recent[0].last_tool.as_deref(), Some("read_file"));
    assert!(!encode(&recent).unwrap().contains("large-private"));
    let before = store.sandbox_cursor(project, ids[5]).unwrap().unwrap();
    assert_eq!(
        store
            .sandbox_summaries(project, None, Some(before))
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        store
            .sandbox_summaries(project, Some(53), None)
            .unwrap()
            .len(),
        2
    );
    assert!(
        store
            .sandbox_summaries(project, Some(u64::MAX), None)
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .sandbox_cursor(Uuid::new_v4(), ids[5])
            .unwrap()
            .is_none()
    );
    assert_eq!(store.sandbox_history(project).unwrap().len(), 12);
    assert!(
        store.sandbox_task(ids[0]).unwrap().unwrap().task.events[0]
            .output
            .contains("large-private-tool-output")
    );
    store
        .lock()
        .execute("DELETE FROM ai_sandbox_task_index WHERE ordinal % 2=0", [])
        .unwrap();
    store.rebuild_sandbox_index().unwrap();
    assert_eq!(store.sandbox_task_count(project).unwrap(), 55);
}

#[test]
fn restart_reindexes_older_records_and_retains_unconfirmed_guest_claims() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let project = Uuid::new_v4();
    let mut task = saved(project, 1);
    task.task.state = SandboxTaskState::InProgress;
    task.guest_pending = true;
    store.save_sandbox_task(&task).unwrap();
    store
        .lock()
        .execute("DELETE FROM ai_sandbox_task_index", [])
        .unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.sandbox_pending_count().unwrap(), 1);
    store.interrupt_sandbox_tasks().unwrap();
    assert!(store.sandbox_project_busy(project).unwrap());
    assert_eq!(
        store.sandbox_summaries(project, None, None).unwrap()[0].state,
        SandboxTaskState::Interrupted
    );
    let mut recovered = store.sandbox_task(task.task.id).unwrap().unwrap();
    recovered.guest_pending = false;
    recovered.task.state = SandboxTaskState::Cancelled;
    store.save_sandbox_task(&recovered).unwrap();
    assert!(!store.sandbox_project_busy(project).unwrap());
    assert_eq!(store.sandbox_task_count(project).unwrap(), 1);
    store.delete_sandbox_project(project).unwrap();
    assert_eq!(store.sandbox_task_count(project).unwrap(), 0);
    assert!(store.sandbox_task(task.task.id).unwrap().is_none());
}

#[test]
fn canonical_task_and_projection_roll_back_together_on_failed_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state.db")).unwrap();
    let project = Uuid::new_v4();
    let mut task = saved(project, 1);
    store.save_sandbox_task(&task).unwrap();
    task.task.updated_at = u64::MAX;
    task.task.state = SandboxTaskState::InProgress;
    assert!(store.save_sandbox_task(&task).is_err());
    assert_eq!(
        store
            .sandbox_task(task.task.id)
            .unwrap()
            .unwrap()
            .task
            .state,
        SandboxTaskState::Result
    );
    assert_eq!(
        store.sandbox_summaries(project, None, None).unwrap()[0].updated_at,
        1
    );
    assert_eq!(store.sandbox_pending_count().unwrap(), 0);
}

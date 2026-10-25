use super::*;
use firecrab_api_types::*;

fn saved(project: Uuid, index: u64) -> SavedTask {
    SavedTask {
        project: SandboxProjectSpec {
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
        task: SandboxTask {
            id: Uuid::new_v4(),
            project_id: project,
            attempt_id: Uuid::new_v4(),
            request: SandboxTaskInput {
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

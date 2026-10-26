use super::*;

pub(super) async fn fixture() -> (tempfile::TempDir, AppState, SavedTask) {
    let dir = tempfile::tempdir().unwrap();
    let registry =
        crate::templates::TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
    let state = AppState::with_db_file(registry, dir.path().join("state.db"))
        .await
        .unwrap();
    let runtime = SandboxRuntimeRef {
        vm_id: Uuid::new_v4(),
        runtime_id: Uuid::new_v4(),
    };
    let saved = SavedTask {
        caller: Default::default(),
        project: SandboxProjectSpec {
            name: "Recovery checkpoint".into(),
            source_path: None,
            vm_id: Some(runtime.vm_id),
            selection: None,
            instructions: String::new(),
        },
        workspace: None,
        guest: Some(runtime),
        guest_pending: true,
        guest_command: Some(Uuid::new_v4()),
        guest_staged: true,
        task: SandboxTask {
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            attempt_id: Uuid::new_v4(),
            runtime: Some(runtime),
            request: SandboxTaskInput {
                kind: SandboxTaskKind::Code,
                input: "Run once".into(),
                selection: None,
                retry_of: None,
            },
            selection: AiModelSelection {
                connection_id: Uuid::new_v4(),
                model: "fixture".into(),
            },
            state: SandboxTaskState::InProgress,
            created_at: 0,
            updated_at: 0,
            result: String::new(),
            error: None,
            error_code: None,
            changes: vec![],
            events: vec![SandboxEvent {
                sequence: 0,
                at: 0,
                name: "run_command".into(),
                input: "{}".into(),
                output: String::new(),
                stderr: String::new(),
                exit_code: None,
            }],
        },
    };
    state.store.save_sandbox_task(&saved).unwrap();
    (dir, state, saved)
}

#[tokio::test(start_paused = true)]
async fn task_deadline_stops_a_pending_operation_without_waiting_ten_real_minutes() {
    let (_sender, mut receiver) = watch::channel(false);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
    let error = interruptible(
        std::future::pending::<Result<(), ExecutionError>>(),
        &mut receiver,
        deadline,
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, "sandbox_timeout");
    assert_eq!(tokio::time::Instant::now(), deadline);
}

#[tokio::test]
async fn cancellation_precedes_dispatch_of_an_already_cancelled_operation() {
    let (_sender, mut receiver) = watch::channel(true);
    let polled = std::cell::Cell::new(false);
    let error = interruptible(
        async {
            polled.set(true);
            Ok(())
        },
        &mut receiver,
        tokio::time::Instant::now() + Duration::from_secs(600),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, "sandbox_cancelled");
    assert!(!polled.get());
}

#[tokio::test]
async fn recovered_evidence_is_durable_before_cleanup_without_premature_terminal_state() {
    let (_dir, state, mut saved) = fixture().await;
    saved.task.state = SandboxTaskState::Cancelled;
    saved.task.error_code = Some("sandbox_cancelled".into());
    checkpoint_recovery(
        &state,
        &mut saved,
        Some(guest::RecoveredOutput {
            stdout: b"actual stdout\n".to_vec(),
            stderr: b"actual stderr\n".to_vec(),
            code: Some(137),
        }),
    )
    .await
    .unwrap();
    assert_eq!(saved.task.state, SandboxTaskState::Cancelled);
    let stored = state.store.sandbox_task(saved.task.id).unwrap().unwrap();
    assert_eq!(stored.task.state, SandboxTaskState::InProgress);
    assert!(stored.guest_pending && stored.guest_staged);
    assert_eq!(stored.task.events[0].output, "actual stdout\n");
    assert_eq!(stored.task.events[0].stderr, "actual stderr\n");
    assert_eq!(stored.task.events[0].exit_code, Some(137));
    assert!(stored.task.error_code.is_none());
    state.store.interrupt_sandbox_tasks().unwrap();
    let mut interrupted = state.store.sandbox_task(saved.task.id).unwrap().unwrap();
    checkpoint_recovery(&state, &mut interrupted, None)
        .await
        .unwrap();
    let stored = state.store.sandbox_task(saved.task.id).unwrap().unwrap();
    assert_eq!(stored.task.state, SandboxTaskState::Interrupted);
    assert_eq!(stored.task.events[0].exit_code, Some(137));
    assert_eq!(stored.task.attempt_id, saved.task.attempt_id);
    assert_eq!(stored.task.runtime, saved.task.runtime);
}

#[tokio::test]
async fn unknown_exit_and_failed_checkpoint_preserve_uncertainty_and_previous_evidence() {
    let (dir, state, mut saved) = fixture().await;
    checkpoint_recovery(
        &state,
        &mut saved,
        Some(guest::RecoveredOutput {
            stdout: b"partial output".to_vec(),
            stderr: vec![],
            code: None,
        }),
    )
    .await
    .unwrap();
    let stored = state.store.sandbox_task(saved.task.id).unwrap().unwrap();
    assert!(stored.task.events[0].exit_code.is_none());
    rusqlite::Connection::open(dir.path().join("state.db")).unwrap()
        .execute_batch("CREATE TRIGGER fail_recovery BEFORE UPDATE ON ai_sandbox_tasks BEGIN SELECT RAISE(ABORT,'checkpoint failure'); END;").unwrap();
    assert!(
        checkpoint_recovery(
            &state,
            &mut saved,
            Some(guest::RecoveredOutput {
                stdout: b"must not delete guest on save failure".to_vec(),
                stderr: vec![],
                code: Some(0),
            })
        )
        .await
        .is_err()
    );
    let stored = state.store.sandbox_task(saved.task.id).unwrap().unwrap();
    assert_eq!(stored.task.events[0].output, "partial output");
    assert!(stored.task.events[0].exit_code.is_none());
    assert!(stored.guest_pending && stored.guest_staged);
}

#[tokio::test]
async fn recovered_output_requires_its_command_event_before_cleanup() {
    let (_dir, state, mut saved) = fixture().await;
    saved.task.events.clear();
    assert!(
        checkpoint_recovery(
            &state,
            &mut saved,
            Some(guest::RecoveredOutput {
                stdout: b"retained in guest".to_vec(),
                stderr: vec![],
                code: None,
            })
        )
        .await
        .is_err()
    );
    assert!(
        state
            .store
            .sandbox_task(saved.task.id)
            .unwrap()
            .unwrap()
            .guest_pending
    );
}

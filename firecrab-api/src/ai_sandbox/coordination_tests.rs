use super::*;

fn saved(project: Uuid) -> SavedTask {
    serde_json::from_value(serde_json::json!({
        "task": {"id":Uuid::new_v4(),"projectId":project,"attemptId":Uuid::new_v4(),
            "request":{"kind":"chat","input":"debug","selection":null,"retryOf":null},
            "selection":{"connectionId":Uuid::new_v4(),"model":"fixture"},
            "state":"failed","createdAt":1,"updatedAt":1,"result":"","errorCode":null,"error":null,"events":[],"changes":[]},
        "project":{"name":"p","sourcePath":null,"vmId":null,"selection":null},
        "workspace":null,"guest":null,"guest_pending":false,"guest_staged":false
    })).unwrap()
}

#[test]
fn dependent_follows_latest_retry_success_and_stops_after_exhaustion() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(&directory.path().join("fixture.db")).unwrap();
    let project = Uuid::new_v4();
    let mut debug = saved(project);
    debug.task.retry_root = Some(debug.task.id);
    debug.task.request.coordination.retry_limit = 1;
    store.save_sandbox_task(&debug).unwrap();
    let mut test = saved(project);
    test.task.request.coordination.dependencies = vec![debug.task.id];
    assert!(matches!(
        state(&store, &test).unwrap(),
        DependencyState::Waiting
    ));
    let mut retry = debug.clone();
    retry.task.id = Uuid::new_v4();
    retry.task.attempt_id = Uuid::new_v4();
    retry.task.request.retry_of = Some(debug.task.id);
    retry.task.retry_number = 1;
    retry.task.state = SandboxTaskState::Result;
    retry.task.result = "known fix and actual regression result".into();
    store.save_sandbox_task(&retry).unwrap();
    let DependencyState::Ready(results) = state(&store, &test).unwrap() else {
        panic!("latest retry must unblock Test");
    };
    assert_eq!(results[0].task.id, retry.task.id);
    assert_eq!(results[0].task.attempt_id, retry.task.attempt_id);
    retry.task.state = SandboxTaskState::Failed;
    store.save_sandbox_task(&retry).unwrap();
    assert!(matches!(
        state(&store, &test).unwrap(),
        DependencyState::Failed
    ));
}

#[test]
fn retries_are_bounded_and_uncertain_or_stale_attempts_cannot_replay() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(&directory.path().join("fixture.db")).unwrap();
    let mut original = saved(Uuid::new_v4());
    original.task.retry_root = Some(original.task.id);
    original.task.request.coordination.retry_limit = 1;
    store.save_sandbox_task(&original).unwrap();
    let mut input = original.task.request.clone();
    input.retry_of = Some(original.task.id);
    assert_eq!(
        retry(&store, original.task.project_id, Uuid::new_v4(), &input).unwrap(),
        (original.task.id, 1)
    );
    input.coordination.retry_limit = 3;
    assert!(retry(&store, original.task.project_id, Uuid::new_v4(), &input).is_err());
    input.coordination.retry_limit = 1;
    original.task.state = SandboxTaskState::Interrupted;
    store.save_sandbox_task(&original).unwrap();
    assert!(retry(&store, original.task.project_id, Uuid::new_v4(), &input).is_err());
    original.task.state = SandboxTaskState::Failed;
    original.guest_pending = true;
    store.save_sandbox_task(&original).unwrap();
    assert!(retry(&store, original.task.project_id, Uuid::new_v4(), &input).is_err());
    original.guest_pending = false;
    store.save_sandbox_task(&original).unwrap();
    let mut next = original.clone();
    next.task.id = Uuid::new_v4();
    next.task.attempt_id = Uuid::new_v4();
    next.task.request = input.clone();
    next.task.retry_number = 1;
    store.save_sandbox_task(&next).unwrap();
    assert!(retry(&store, original.task.project_id, Uuid::new_v4(), &input).is_err());
    input.retry_of = Some(next.task.id);
    assert!(retry(&store, original.task.project_id, Uuid::new_v4(), &input).is_err());
}

#[test]
fn restart_interrupts_waiters_without_repeating_successful_dependencies() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fixture.db");
    let mut dependency = saved(Uuid::new_v4());
    dependency.task.state = SandboxTaskState::Result;
    let mut dependent = saved(dependency.task.project_id);
    dependent.task.state = SandboxTaskState::Waiting;
    dependent.task.request.coordination.dependencies = vec![dependency.task.id];
    {
        let store = Store::open(&path).unwrap();
        store.save_sandbox_task(&dependency).unwrap();
        store.save_sandbox_task(&dependent).unwrap();
    }
    let store = Store::open(&path).unwrap();
    store.interrupt_sandbox_tasks().unwrap();
    assert_eq!(
        store
            .sandbox_task(dependency.task.id)
            .unwrap()
            .unwrap()
            .task
            .state,
        SandboxTaskState::Result
    );
    assert_eq!(
        store
            .sandbox_task(dependent.task.id)
            .unwrap()
            .unwrap()
            .task
            .state,
        SandboxTaskState::Interrupted
    );
    assert_eq!(
        store
            .sandbox_task_count(dependency.task.project_id)
            .unwrap(),
        2
    );
}

use super::*;
use serde_json::json;

mod sync;

fn request(generation: u64, paused: bool) -> ConfigureGitSource {
    serde_json::from_value(json!({"expectedGeneration":generation,"source":{"repository":"https://github.com/example/infra","ref":"main","manifestPath":"stack.yaml","paused":paused,"pollIntervalSeconds":60}})).unwrap()
}

fn finish(store: &Store, source: &GitSourceStatus, state: GitCheckState, now: i64) -> GitCheck {
    let mut check = source.latest.clone().unwrap();
    check.state = state;
    check.finished_at = Some(now);
    store
        .finish_stack_check(source.stack_id, &check, now)
        .unwrap();
    check
}

#[test]
fn source_generation_claims_pause_and_recovery_survive_reopen() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("state.db");
    let store = Store::open(&file).unwrap();
    let stack = store.register_stack("demo").unwrap();
    let configured = store
        .configure_stack_source("demo", &request(0, true), 10)
        .unwrap();
    assert_eq!(configured.generation, 1);
    assert!(matches!(
        store.configure_stack_source("demo", &request(0, false), 10),
        Err(PersistenceError::StaleStackSource)
    ));
    assert_eq!(
        store
            .configure_stack_source("demo", &request(1, true), 11)
            .unwrap(),
        configured
    );
    assert!(
        store
            .begin_stack_check("demo", 1, 10, SourceTrigger::Poll)
            .unwrap()
            .is_none()
    );
    let (source, fresh) = store
        .begin_stack_check("demo", 1, 10, SourceTrigger::Check)
        .unwrap()
        .unwrap();
    assert!(fresh);
    let repeated = store
        .begin_stack_check("demo", 1, 10, SourceTrigger::Check)
        .unwrap()
        .unwrap();
    assert!(!repeated.1);
    assert_eq!(source, repeated.0);
    assert!(matches!(
        store.configure_stack_source("demo", &request(1, false), 10),
        Err(PersistenceError::StackSourceBusy)
    ));
    assert!(matches!(
        store.delete_empty_stack("demo", stack.id),
        Err(PersistenceError::StackSourceBusy)
    ));
    drop(store);
    let store = Store::open(&file).unwrap();
    assert_eq!(store.stack_source("demo").unwrap(), source);
    store.interrupt_stack_checks(20).unwrap();
    let interrupted = store.stack_source("demo").unwrap();
    assert_eq!(
        interrupted.latest.as_ref().unwrap().state,
        GitCheckState::Interrupted
    );
    assert_eq!(
        interrupted.latest.as_ref().unwrap().id,
        source.latest.as_ref().unwrap().id
    );
    let mut stale_worker = source.latest.unwrap();
    stale_worker.state = GitCheckState::InSync;
    stale_worker.finished_at = Some(21);
    assert!(matches!(
        store.finish_stack_check(stack.id, &stale_worker, 21),
        Err(PersistenceError::StaleStackSource)
    ));
    let resumed = store
        .configure_stack_source("demo", &request(1, false), 30)
        .unwrap();
    assert_eq!(resumed.generation, 2);
    assert!(resumed.latest.is_none());
    assert!(matches!(
        store.begin_stack_check("demo", 1, 30, SourceTrigger::Check),
        Err(PersistenceError::StaleStackSource)
    ));
    assert_eq!(store.stack_check_history("demo").unwrap().len(), 1);
    assert!(store.stack_check_history("demo").unwrap()[0].source.paused);
    assert_eq!(store.stack_check_history("demo").unwrap()[0].generation, 1);
    store.delete_empty_stack("demo", stack.id).unwrap();
    let newer = store.register_stack("demo").unwrap();
    assert_ne!(stack.id, newer.id);
    assert!(store.stack_check_history("demo").unwrap().is_empty());
    assert!(matches!(
        store.stack_source("demo"),
        Err(PersistenceError::MissingStackSource)
    ));
}

#[test]
fn source_history_is_bounded_and_failures_back_off_without_suppressing_manual_checks() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    store.register_stack("demo").unwrap();
    store
        .configure_stack_source("demo", &request(0, false), 0)
        .unwrap();
    for n in 0..55 {
        let (source, fresh) = store
            .begin_stack_check("demo", 1, n, SourceTrigger::Check)
            .unwrap()
            .unwrap();
        assert!(fresh);
        finish(&store, &source, GitCheckState::Failed, n);
    }
    let source = store.stack_source("demo").unwrap();
    assert_eq!(source.failures, 55);
    assert_eq!(source.next_check_at, 54 + 60 * 64);
    assert!(
        store
            .begin_stack_check("demo", 1, 55, SourceTrigger::Poll)
            .unwrap()
            .is_none()
    );
    let history = store.stack_check_history("demo").unwrap();
    assert_eq!(history.len(), 50);
    assert_eq!(history[0].started_at, 54);
    assert_eq!(history[49].started_at, 5);
    let (source, _) = store
        .begin_stack_check("demo", 1, 60, SourceTrigger::Check)
        .unwrap()
        .unwrap();
    finish(&store, &source, GitCheckState::InSync, 60);
    let source = store.stack_source("demo").unwrap();
    assert_eq!(source.failures, 0);
    assert_eq!(source.next_check_at, 120);
    assert!(
        store
            .begin_stack_check("demo", 1, 120, SourceTrigger::Poll)
            .unwrap()
            .unwrap()
            .1
    );
}

#[test]
fn independent_database_connections_coalesce_concurrent_check_admission() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("state.db");
    let store = Store::open(&file).unwrap();
    store.register_stack("demo").unwrap();
    store
        .configure_stack_source("demo", &request(0, false), 0)
        .unwrap();
    let other = Store::open(&file).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let guard = barrier.clone();
    let thread = std::thread::spawn(move || {
        guard.wait();
        other
            .begin_stack_check("demo", 1, 0, SourceTrigger::Check)
            .unwrap()
            .unwrap()
    });
    barrier.wait();
    let first = store
        .begin_stack_check("demo", 1, 0, SourceTrigger::Check)
        .unwrap()
        .unwrap();
    let second = thread.join().unwrap();
    assert_ne!(first.1, second.1);
    assert_eq!(first.0, second.0);
    assert_eq!(store.stack_check_history("demo").unwrap().len(), 1);
}

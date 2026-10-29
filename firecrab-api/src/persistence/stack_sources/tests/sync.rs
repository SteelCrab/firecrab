use super::*;
use firecrab_stack::{
    Action, ApplyState, ApplyStatus, ApplyStep, GitSyncMode, Inventory, StackRecord, StepState,
};

fn source(store: &Store) -> (StackRecord, GitSourceStatus) {
    let record = store.register_stack("demo").unwrap();
    let mut config = request(0, false);
    config.source.sync_mode = GitSyncMode::Automatic;
    store.configure_stack_source("demo", &config, 0).unwrap();
    let (source, fresh) = store
        .begin_stack_check("demo", 1, 0, SourceTrigger::Poll)
        .unwrap()
        .unwrap();
    assert!(fresh);
    assert!(source.latest.as_ref().unwrap().sync);
    (record, source)
}

fn operation(record: &StackRecord, source: &GitSourceStatus) -> (GitCheck, ApplyStatus, String) {
    let stack = firecrab_stack::parse("apiVersion: firecrab.dev/v1alpha1\nkind: Stack\nname: demo\nmicroNetworks:\n  app:\n    name: demo\n    subnetCidr: 172.31.20.0/24\n").unwrap();
    let plan = firecrab_stack::plan(&stack, record, &Inventory::default()).unwrap();
    let hash = firecrab_stack::desired_hash(&stack);
    let mut check = source.latest.clone().unwrap();
    check.state = GitCheckState::Drifted;
    check.commit = Some("a".repeat(40));
    check.blob = Some("b".repeat(40));
    check.desired_hash = Some(hash.clone());
    check.plan = Some(plan.clone());
    let mut change = plan.changes[0].clone();
    assert_eq!(change.action, Action::Create);
    change.resource_id = Some(Uuid::new_v4());
    let status = ApplyStatus {
        lifecycle: Default::default(),
        inputs: Default::default(),
        git_revision: check.revision(),
        operation_id: check.id,
        stack_id: record.id,
        name: record.name.clone(),
        state: ApplyState::Queued,
        changes: vec![ApplyStep {
            replacement_for: None,
            vm_disk: None,
            change,
            state: StepState::Pending,
        }],
        error: None,
    };
    (check, status, hash)
}

#[test]
fn git_service_writes_require_the_exact_running_operation_and_resource_step() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let (record, source) = source(&store);
    let (check, mut status, hash) = operation(&record, &source);
    status.changes[0].change.kind = firecrab_stack::ResourceKind::Service;
    let service = status.changes[0].change.resource_id.unwrap();
    store
        .begin_stack_apply(&record, &hash, false, &status, Some(&check))
        .unwrap();
    assert!(
        !store
            .stack_service_mutation_allowed(service, Some(status.operation_id))
            .unwrap()
    );
    status.state = ApplyState::Running;
    status.changes[0].state = StepState::Running;
    store.save_stack_apply(&status).unwrap();
    assert!(!store.stack_service_mutation_allowed(service, None).unwrap());
    assert!(
        !store
            .stack_service_mutation_allowed(service, Some(uuid::Uuid::new_v4()))
            .unwrap()
    );
    assert!(
        store
            .stack_service_mutation_allowed(service, Some(status.operation_id))
            .unwrap()
    );
    status.state = ApplyState::Failed;
    status.changes[0].state = StepState::Failed;
    store.save_stack_apply(&status).unwrap();
    assert!(
        !store
            .stack_service_mutation_allowed(service, Some(status.operation_id))
            .unwrap()
    );
}

#[test]
fn git_apply_admission_atomically_guards_source_and_reserves_exactly_one_operation() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let (record, source) = source(&store);
    let (mut check, mut status, hash) = operation(&record, &source);
    check.generation += 1;
    status.git_revision = check.revision();
    assert!(matches!(
        store.begin_stack_apply(&record, &hash, false, &status, Some(&check)),
        Err(PersistenceError::StaleStackSource)
    ));
    assert!(
        store
            .stored_stack_apply(status.operation_id)
            .unwrap()
            .is_none()
    );
    assert!(store.stack("demo").unwrap().unwrap().bindings.is_empty());
    check.generation -= 1;
    status.git_revision = check.revision();
    assert!(matches!(
        store.begin_stack_apply(&record, &hash, true, &status, Some(&check)),
        Err(PersistenceError::StackApplyIdentity)
    ));
    assert!(
        store
            .stored_stack_apply(status.operation_id)
            .unwrap()
            .is_none()
    );
    assert!(store.stack("demo").unwrap().unwrap().bindings.is_empty());
    assert!(
        store
            .begin_stack_apply(&record, &hash, false, &status, Some(&check))
            .unwrap()
            .1
    );
    assert!(
        !store
            .begin_stack_apply(&record, &hash, false, &status, Some(&check))
            .unwrap()
            .1
    );
    let current = store.stack_source("demo").unwrap();
    assert_eq!(
        current.latest.as_ref().unwrap().state,
        GitCheckState::Applying
    );
    assert_eq!(
        current.latest.as_ref().unwrap().operation_id,
        Some(status.operation_id)
    );
    assert_eq!(current.attempted_revision, check.revision());
    assert_eq!(
        store.stack("demo").unwrap().unwrap().bindings[0].resource_id,
        status.changes[0].change.resource_id.unwrap()
    );
    assert!(matches!(
        store.configure_stack_source("demo", &request(1, true), 1),
        Err(PersistenceError::StackSourceBusy)
    ));
    assert!(
        !store
            .begin_stack_check("demo", 1, 1, SourceTrigger::Sync(None))
            .unwrap()
            .unwrap()
            .1
    );
}

#[test]
fn restart_preserves_apply_identity_and_blocks_automatic_replay_until_explicit_recovery() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("state.db");
    let store = Store::open(&file).unwrap();
    let (record, source) = source(&store);
    let (check, mut status, hash) = operation(&record, &source);
    store
        .begin_stack_apply(&record, &hash, false, &status, Some(&check))
        .unwrap();
    drop(store);
    let store = Store::open(&file).unwrap();
    store.interrupt_stack_applies().unwrap();
    store.interrupt_stack_checks(10).unwrap();
    let interrupted = store.stack_source("demo").unwrap();
    assert_eq!(
        interrupted.latest.as_ref().unwrap().state,
        GitCheckState::RecoveryRequired
    );
    assert_eq!(
        interrupted.last_apply.as_ref().unwrap().state,
        ApplyState::Interrupted
    );
    assert_eq!(interrupted.recovery_operation_id, Some(check.id));
    assert_eq!(interrupted.attempted_revision, check.revision());
    assert!(interrupted.successful_revision.is_none());
    assert!(
        store
            .begin_stack_check("demo", 1, 10000, SourceTrigger::Poll)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        store.begin_stack_check("demo", 1, 10000, SourceTrigger::Sync(None)),
        Err(PersistenceError::StackSyncRecoveryRequired)
    ));
    status.state = ApplyState::Succeeded;
    assert!(!store.save_stack_apply(&status).unwrap());
    assert_eq!(store.stack("demo").unwrap().unwrap().bindings.len(), 1);
    let (recovering, fresh) = store
        .begin_stack_check("demo", 1, 10000, SourceTrigger::Sync(Some(check.id)))
        .unwrap()
        .unwrap();
    assert!(fresh);
    assert_ne!(recovering.latest.unwrap().id, check.id);
    assert_eq!(recovering.recovery_operation_id, Some(check.id));
}

#[test]
fn completed_apply_survives_lost_acknowledgement_and_observation_eviction() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("state.db");
    let store = Store::open(&file).unwrap();
    let (record, source) = source(&store);
    let (check, mut status, hash) = operation(&record, &source);
    store
        .begin_stack_apply(&record, &hash, false, &status, Some(&check))
        .unwrap();
    status.state = ApplyState::Succeeded;
    status.changes[0].state = StepState::Succeeded;
    store.save_stack_apply(&status).unwrap();
    drop(store);
    let store = Store::open(&file).unwrap();
    store.interrupt_stack_applies().unwrap();
    store.interrupt_stack_checks(10).unwrap();
    assert_eq!(
        store.stack_source("demo").unwrap().latest.unwrap().state,
        GitCheckState::Applied
    );
    for n in 0..55 {
        let (source, _) = store
            .begin_stack_check("demo", 1, 20 + n, SourceTrigger::Check)
            .unwrap()
            .unwrap();
        finish(&store, &source, GitCheckState::Failed, 20 + n);
    }
    let source = store.stack_source("demo").unwrap();
    assert_eq!(source.successful_revision, check.revision());
    assert_eq!(source.last_apply, Some(status));
    assert!(source.recovery_operation_id.is_none());
    assert_eq!(store.stack_check_history("demo").unwrap().len(), 50);
    assert_eq!(
        store
            .stored_stack_apply(check.id)
            .unwrap()
            .unwrap()
            .status
            .git_revision,
        check.revision()
    );
}

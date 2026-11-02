use super::*;
use crate::{
    persistence::PersistenceError,
    pipe_databases::{
        DatabaseRecord,
        maintenance::{Job, Kind, ParentJob},
    },
};

fn connection(state: &AppState, root: &std::path::Path, network: Uuid) -> DatabaseRecord {
    let r = configuration_resource(state, root, network, "database_connection");
    state
        .store
        .pipe_database(Uuid::parse_str(&r.id).unwrap())
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn iac_database_target_admission_serializes_saved_aliases_across_connections() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let record = connection(&state, root.path(), network);
    let mut alias = record.clone();
    alias.id = Uuid::new_v4();
    alias.input.name = "alias".into();
    state
        .store
        .save_pipe_database(alias.id, &alias.input)
        .unwrap();
    let first = Job::new(&record, Uuid::new_v4(), Kind::Migration).unwrap();
    let other = Job::new(&alias, Uuid::new_v4(), Kind::Backup).unwrap();
    state.store.save_database_job(&first).unwrap();
    let second = crate::persistence::Store::open(&root.path().join("state.db")).unwrap();
    assert!(matches!(
        second.save_database_job(&other),
        Err(PersistenceError::IacResourceBusy)
    ));
    assert!(second.database_job(other.id).unwrap().is_none());
    let mut changed = first.clone();
    changed.sql = Some("CREATE TABLE unintended(id int)".into());
    assert!(matches!(
        second.save_database_job(&changed),
        Err(PersistenceError::IacOperationIdentity)
    ));
}

#[tokio::test]
async fn iac_database_job_claim_is_single_use_and_interruption_fences_old_progress() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let record = connection(&state, root.path(), network);
    let queued = Job::new(&record, Uuid::new_v4(), Kind::Migration).unwrap();
    state.store.save_database_job(&queued).unwrap();
    let second = crate::persistence::Store::open(&root.path().join("state.db")).unwrap();
    let mut claimed = state
        .store
        .claim_database_job(queued.id, 0)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.execution_generation, 1);
    assert!(second.claim_database_job(queued.id, 0).unwrap().is_none());
    assert!(matches!(
        second.save_database_job(&queued),
        Err(PersistenceError::StaleIacOwner)
    ));
    claimed.maintenance_started = true;
    state.store.save_database_job(&claimed).unwrap();
    assert!(
        second
            .interrupt_database_job(claimed.id, claimed.execution_generation)
            .unwrap()
    );
    claimed.status = "succeeded".into();
    assert!(matches!(
        state.store.save_database_job(&claimed),
        Err(PersistenceError::StaleIacOwner)
    ));
    let current = second.database_job(queued.id).unwrap().unwrap();
    assert_eq!(current.status, "needs_recovery");
    assert_eq!(current.execution_generation, 2);
    assert!(!second.interrupt_database_job(queued.id, 1).unwrap());
}

#[tokio::test]
async fn iac_database_safety_child_uses_only_its_original_running_parent_attempt() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let record = connection(&state, root.path(), network);
    let queued = Job::new(&record, Uuid::new_v4(), Kind::Migration).unwrap();
    state.store.save_database_job(&queued).unwrap();
    let mut parent = state
        .store
        .claim_database_job(queued.id, 0)
        .unwrap()
        .unwrap();
    let mut child = Job::new(&record, Uuid::new_v4(), Kind::Backup).unwrap();
    child.status = "running".into();
    child.parent = Some(ParentJob {
        id: parent.id,
        execution_generation: parent.execution_generation,
    });
    parent.safety_backup_id = Some(child.id);
    state.store.save_database_job(&parent).unwrap();
    state.store.save_database_job(&child).unwrap();
    let unrelated = Job::new(&record, Uuid::new_v4(), Kind::Backup).unwrap();
    assert!(matches!(
        state.store.save_database_job(&unrelated),
        Err(PersistenceError::IacResourceBusy)
    ));
    assert!(
        state
            .store
            .interrupt_database_job(parent.id, parent.execution_generation)
            .unwrap()
    );
    child.status = "succeeded".into();
    assert!(matches!(
        state.store.save_database_job(&child),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert_eq!(
        state.store.database_job(child.id).unwrap().unwrap().status,
        "running"
    );
    assert!(state.store.interrupt_database_job(child.id, 0).unwrap());
}

#[tokio::test]
async fn iac_database_claim_race_runs_one_executor_across_two_sqlite_connections() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let record = connection(&state, root.path(), network);
    let queued = Job::new(&record, Uuid::new_v4(), Kind::Backup).unwrap();
    state.store.save_database_job(&queued).unwrap();
    let second = crate::persistence::Store::open(&root.path().join("state.db")).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let workers = [state.store.clone(), second]
        .into_iter()
        .map(|store| {
            let barrier = barrier.clone();
            let id = queued.id;
            std::thread::spawn(move || {
                barrier.wait();
                store.claim_database_job(id, 0).unwrap().is_some()
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    assert_eq!(
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|claimed| *claimed)
            .count(),
        1
    );
    assert_eq!(
        state
            .store
            .database_job(queued.id)
            .unwrap()
            .unwrap()
            .execution_generation,
        1
    );
}

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

fn migration(record: &DatabaseRecord) -> Job {
    let mut job = Job::new(record, Uuid::new_v4(), Kind::Migration).unwrap();
    job.version = Some("v1".into());
    job.sql = Some("CREATE TABLE native_migration_fixture(id int)".into());
    job
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
    let first = migration(&record);
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
    let queued = migration(&record);
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
    let queued = migration(&record);
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

#[tokio::test]
async fn iac_database_migration_original_receipts_survive_parent_removal_and_transfer() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let record = connection(&state, root.path(), network);
    let mut job = migration(&record);
    job.status = "succeeded".into();
    state.store.save_database_job(&job).unwrap();
    let target = ManagedResource {
        kind: "database_migration".into(),
        id: format!("{}:{}", record.id, job.id),
    };
    let request: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &target)).unwrap();
    let owned = state.store.change_iac_resource_owner(&request).unwrap();
    assert_eq!(owned.resource, target);
    assert!(state.store.delete_pipe_database(record.id).unwrap());
    let body = json!({"requestId":job.id,"version":job.version,"sql":job.sql,"confirmDatabase":"","externalWritersStopped":false});
    let (code, receipt) = call(
        &app,
        "POST",
        &format!("/api/pipe/databases/{}/migrations", record.id),
        body.clone(),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{receipt}");
    assert_eq!(receipt["status"], "succeeded");
    assert_eq!(receipt["versionChecksum"].as_str().unwrap().len(), 64);
    assert!(!receipt.to_string().contains("CREATE TABLE"));
    let (code, observed) = call(
        &app,
        "GET",
        &format!("/api/pipe/database-jobs/{}", job.id),
        Value::Null,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(observed, receipt);
    let mut changed = body;
    changed["sql"] = json!("CREATE TABLE replacement(id int)");
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/api/pipe/databases/{}/migrations", record.id),
            changed
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        state.store.database_job(job.id).unwrap().unwrap().sql,
        job.sql
    );
}

#[tokio::test]
async fn iac_database_migration_retry_uses_original_identity_and_observed_attempt() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let record = connection(&state, root.path(), network);
    let mut job = migration(&record);
    state.store.save_database_job(&job).unwrap();
    let target = ManagedResource {
        kind: "database_migration".into(),
        id: format!("{}:{}", record.id, job.id),
    };
    let mut change: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &target)).unwrap();
    assert!(matches!(
        state.store.change_iac_resource_owner(&change),
        Err(PersistenceError::IacResourceBusy)
    ));
    job.status = "failed".into();
    state.store.save_database_job(&job).unwrap();
    let owned = state.store.change_iac_resource_owner(&change).unwrap();
    let retry = format!("/api/pipe/database-jobs/{}/retry", job.id);
    assert_eq!(
        call(&app, "POST", &retry, json!({"expectedGeneration":0}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    change.operation_id = Uuid::new_v4();
    change.expected_generation = owned.generation;
    change.expected_owner = owned.owner;
    change.owner = None;
    state.store.change_iac_resource_owner(&change).unwrap();
    let (code, receipt) = call(&app, "POST", &retry, json!({"expectedGeneration":0})).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{receipt}");
    assert_eq!(receipt["id"], job.id.to_string());
    assert_eq!(receipt["executionGeneration"], 1);
    assert_eq!(receipt["status"], "queued");
    assert!(state.store.claim_database_job(job.id, 0).unwrap().is_none());
    assert!(state.store.save_database_job(&job).is_err());
    assert!(matches!(
        state.store.retry_database_migration(job.id, 0),
        Err(PersistenceError::StaleIacOwner)
    ));
    let claimed = state.store.claim_database_job(job.id, 1).unwrap().unwrap();
    state.store.check_database_job(&claimed).unwrap();
    assert_eq!(claimed.sql, job.sql);
    state
        .store
        .interrupt_database_job(claimed.id, claimed.execution_generation)
        .unwrap();
    assert!(state.store.check_database_job(&claimed).is_err());
}

#[tokio::test]
async fn iac_database_migration_versions_cannot_be_recreated_through_another_alias() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let record = connection(&state, root.path(), network);
    let mut alias = record.clone();
    alias.id = Uuid::new_v4();
    state
        .store
        .save_pipe_database(alias.id, &alias.input)
        .unwrap();
    let mut original = migration(&record);
    original.status = "succeeded".into();
    state.store.save_database_job(&original).unwrap();
    let other = crate::persistence::Store::open(&root.path().join("state.db")).unwrap();
    let mut duplicate = migration(&alias);
    assert!(matches!(
        other.save_database_job(&duplicate),
        Err(PersistenceError::IacOperationIdentity)
    ));
    duplicate.sql = Some("CREATE TABLE different_definition(id int)".into());
    assert!(matches!(
        other.save_database_job(&duplicate),
        Err(PersistenceError::IacOperationIdentity)
    ));
    assert!(other.database_job(duplicate.id).unwrap().is_none());
    duplicate.version = Some("v2".into());
    other.save_database_job(&duplicate).unwrap();
    let wrong = ManagedResource {
        kind: "database_migration".into(),
        id: format!("{}:{}", alias.id, original.id),
    };
    let change: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &wrong)).unwrap();
    assert!(matches!(
        state.store.change_iac_resource_owner(&change),
        Err(PersistenceError::InvalidStackResource)
    ));
}

#[tokio::test]
async fn iac_database_migration_unknown_schema_changes_cannot_be_replayed_by_retry() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let record = connection(&state, root.path(), network);
    let job = migration(&record);
    state.store.save_database_job(&job).unwrap();
    let mut running = state.store.claim_database_job(job.id, 0).unwrap().unwrap();
    running.maintenance_started = true;
    state.store.save_database_job(&running).unwrap();
    state
        .store
        .interrupt_database_job(running.id, running.execution_generation)
        .unwrap();
    let (code, receipt) = call(
        &app,
        "POST",
        &format!("/api/pipe/database-jobs/{}/retry", job.id),
        json!({"expectedGeneration":2}),
    )
    .await;
    assert_eq!(code, StatusCode::CONFLICT, "{receipt}");
    let current = state.store.database_job(job.id).unwrap().unwrap();
    assert_eq!(current.status, "needs_recovery");
    assert_eq!(current.execution_generation, 2);
    assert!(!current.summary()["retryable"].as_bool().unwrap());
}

use super::*;
use crate::service_runtime::tests::{deployment, state};
use axum::response::IntoResponse;
use firecrab_api_types::MicroServiceResponse;
use std::future::Future;

fn insert_configuration(state: &AppState, record: &MicroServiceDeployment) {
    let mut spec = record.spec.clone();
    spec.name = record.service_id.simple().to_string();
    state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id: record.service_id,
            spec,
        })
        .unwrap();
}

fn insert(state: &AppState, record: &MicroServiceDeployment) {
    insert_configuration(state, record);
    if let Some(update) = &record.update {
        state
            .store
            .save_service_deployment(&update.previous)
            .unwrap();
    }
    state.store.save_service_deployment(record).unwrap();
}

fn update() -> MicroServiceDeployment {
    let mut record = crate::service_runtime::deploy::tests::journal();
    record.instances.clear();
    record.domain_route_owned = false;
    record.internal_route_owned = false;
    record
}

async fn cancel_attempt(
    state: &AppState,
    record: &MicroServiceDeployment,
) -> Result<Json<MicroServiceDeployment>, AppError> {
    cancel(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path((record.service_id.to_string(), record.id.to_string())),
    )
    .await
}

#[tokio::test]
async fn rollback_precedes_waiting_normal_work_and_normal_work_is_fifo() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let held = state
        .service_deploy_permits
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    let mut first = deployment(Uuid::new_v4());
    first.id = Uuid::from_u128(300);
    let mut second = deployment(Uuid::new_v4());
    second.id = Uuid::from_u128(200);
    let mut rollback = update();
    rollback.update.as_mut().unwrap().rollback_of = Some(Uuid::new_v4());
    for record in [&first, &second, &rollback] {
        insert(&state, record);
    }
    let (sender, mut admitted) = tokio::sync::mpsc::channel(3);
    let mut releases = std::collections::HashMap::new();
    let mut tasks = Vec::new();
    for record in [&first, &second, &rollback] {
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        releases.insert(record.id, release);
        let state = state.clone();
        let record = record.clone();
        let sender = sender.clone();
        tasks.push(tokio::spawn(async move {
            let (_permit, current) = admit(&state, &record).await.unwrap();
            sender.send((current.id, current.phase)).await.unwrap();
            wait.await.unwrap();
        }));
    }
    drop(held);
    for expected in [rollback.id, first.id, second.id] {
        let (id, phase) = tokio::time::timeout(Duration::from_secs(3), admitted.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(id, expected);
        assert_eq!(phase, ServicePhase::Importing);
        releases.remove(&id).unwrap().send(()).unwrap();
    }
    for task in tasks {
        task.await.unwrap();
    }
}

#[tokio::test]
async fn cancelled_initial_worker_cannot_capture_secrets_or_overwrite_history() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut record = deployment(Uuid::new_v4());
    record
        .spec
        .secrets
        .insert("PASSWORD".into(), Uuid::new_v4());
    insert(&state, &record);
    let cancelled = cancel_attempt(&state, &record).await.unwrap().0;
    assert_eq!(cancelled.phase, ServicePhase::Cancelled);
    assert!(cancelled.finished_at.is_some());
    run_deploy(state.clone(), record.clone()).await;
    assert_eq!(
        state
            .store
            .service_deployment(record.service_id)
            .unwrap()
            .unwrap(),
        cancelled
    );
    assert!(!state.store.service_deploy_turn(record.id).unwrap().0);
    assert!(state.vms.lock().unwrap().is_empty());
    assert!(
        state
            .secrets
            .read(|vault| Ok(!vault.service_environments.contains_key(&record.id)))
            .unwrap()
    );
}

#[tokio::test]
async fn cancelled_update_atomically_retains_the_running_release_and_private_inputs() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut record = update();
    record
        .spec
        .secrets
        .insert("PASSWORD".into(), Uuid::new_v4());
    let previous = (*record.update.as_ref().unwrap().previous).clone();
    state
        .secrets
        .write(|vault| {
            vault.service_environments.insert(
                previous.id,
                BTreeMap::from([("PASSWORD".into(), "old-private".into())]),
            );
            vault.service_environments.insert(
                record.id,
                BTreeMap::from([("PASSWORD".into(), "unused-private".into())]),
            );
            Ok(())
        })
        .unwrap();
    insert(&state, &record);
    let releases = state.store.service_releases(record.service_id).unwrap();
    let cancelled = cancel_attempt(&state, &record).await.unwrap().0;
    assert_eq!(cancelled.phase, ServicePhase::Cancelled);
    assert!(cancelled.update.as_ref().unwrap().completed);
    assert!(!cancelled.update.as_ref().unwrap().cancelled);
    assert_eq!(
        state
            .store
            .service_deployment(record.service_id)
            .unwrap()
            .unwrap(),
        previous
    );
    assert_eq!(
        state.store.service_attempt(record.id).unwrap().unwrap(),
        cancelled
    );
    assert_eq!(
        state.store.service_releases(record.service_id).unwrap(),
        releases
    );
    assert!(
        state
            .store
            .vm_has_service_owner(previous.instances[0].vm_id)
            .unwrap()
    );
    state
        .secrets
        .read(|vault| {
            assert!(vault.service_environments.contains_key(&previous.id));
            assert!(!vault.service_environments.contains_key(&record.id));
            Ok(())
        })
        .unwrap();
    deploy::resume_worker(state.clone(), record);
    tokio::task::yield_now().await;
    assert_eq!(
        state
            .store
            .service_deployment(previous.service_id)
            .unwrap()
            .unwrap(),
        previous
    );
}

#[tokio::test]
async fn cancellation_replay_does_not_change_a_newer_queued_attempt() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let old = update();
    insert(&state, &old);
    let cancelled = cancel_attempt(&state, &old).await.unwrap().0;
    let mut newer = old.clone();
    newer.id = Uuid::new_v4();
    state.store.save_service_deployment(&newer).unwrap();
    assert_eq!(cancel_attempt(&state, &old).await.unwrap().0, cancelled);
    assert_eq!(
        state
            .store
            .service_deployment(old.service_id)
            .unwrap()
            .unwrap(),
        newer
    );
}

#[tokio::test]
async fn admitted_operation_rejects_cancellation_and_duplicate_workers() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let record = deployment(Uuid::new_v4());
    insert(&state, &record);
    let (_permit, current) = admit(&state, &record).await.unwrap();
    assert_eq!(current.phase, ServicePhase::Importing);
    assert_eq!(
        cancel_attempt(&state, &record).await.unwrap_err().code(),
        "deploy_not_cancellable"
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(1), admit(&state, &record))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        state
            .store
            .service_deployment(record.service_id)
            .unwrap()
            .unwrap(),
        current
    );
}

#[tokio::test]
async fn cancellation_is_scoped_to_the_exact_service_and_unstarted_operation() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut record = update();
    insert(&state, &record);
    let mut wrong = record.clone();
    wrong.service_id = Uuid::new_v4();
    assert_eq!(
        cancel_attempt(&state, &wrong).await.unwrap_err().code(),
        "not_found"
    );
    wrong = record.clone();
    wrong.id = Uuid::new_v4();
    assert_eq!(
        cancel_attempt(&state, &wrong).await.unwrap_err().code(),
        "not_found"
    );
    // Retry can be Queued while a previous transition or VM still needs cleanup.
    record.update.as_mut().unwrap().restoring = true;
    state.store.save_service_deployment(&record).unwrap();
    assert_eq!(
        cancel_attempt(&state, &record).await.unwrap_err().code(),
        "deploy_not_cancellable"
    );
    assert_eq!(
        state
            .store
            .service_deployment(record.service_id)
            .unwrap()
            .unwrap(),
        record
    );
}

#[tokio::test]
async fn queue_claim_and_cancellation_have_exactly_one_winner() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    for _ in 0..12 {
        let record = deployment(Uuid::new_v4());
        insert(&state, &record);
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));
        let task_state = state.clone();
        let task_record = record.clone();
        let start = barrier.clone();
        let worker = tokio::spawn(async move {
            start.wait().await;
            admit(&task_state, &task_record).await
        });
        let task_state = state.clone();
        let task_record = record.clone();
        let start = barrier.clone();
        let cancel = tokio::spawn(async move {
            start.wait().await;
            cancel_attempt(&task_state, &task_record).await
        });
        barrier.wait().await;
        let (admission, cancellation) = tokio::time::timeout(Duration::from_secs(3), async {
            (worker.await.unwrap(), cancel.await.unwrap())
        })
        .await
        .unwrap();
        match (admission, cancellation) {
            (None, Ok(Json(current))) => assert_eq!(current.phase, ServicePhase::Cancelled),
            (Some((_permit, current)), Err(error)) => {
                assert_eq!(current.phase, ServicePhase::Importing);
                assert_eq!(error.code(), "deploy_not_cancellable");
                let mut finished = current;
                finished.phase = ServicePhase::Failed;
                state.store.save_service_deployment(&finished).unwrap();
            }
            _ => panic!("admission and cancellation must be mutually exclusive"),
        }
    }
}

#[tokio::test]
async fn restart_preserves_fifo_and_finishes_an_interrupted_claim_before_rollback() {
    let root = tempfile::tempdir().unwrap();
    let original = state(root.path()).await;
    let active = update();
    insert(&original, &active);
    let (permit, mut active) = admit(&original, &active).await.unwrap();
    active.phase = ServicePhase::Draining;
    original.store.save_service_deployment(&active).unwrap();
    let first = deployment(Uuid::new_v4());
    let second = deployment(Uuid::new_v4());
    let mut rollback = update();
    rollback.update.as_mut().unwrap().rollback_of = Some(Uuid::new_v4());
    for record in [&first, &second, &rollback] {
        insert(&original, record);
    }
    drop(permit);
    drop(original);
    let restored = state(root.path()).await;
    // Restore all persisted claims before starting any worker, as startup does.
    for record in [&second, &rollback, &first, &active] {
        restored
            .store
            .enqueue_service_deployment(record, true)
            .unwrap();
    }
    for record in [&active, &rollback, &first, &second] {
        assert_eq!(
            restored.store.service_deploy_turn(record.id).unwrap(),
            (true, true)
        );
        let (permit, admitted) = admit(&restored, record).await.unwrap();
        if record.id == active.id {
            assert_eq!(admitted.phase, ServicePhase::Draining);
        }
        drop(permit);
    }
}

#[tokio::test]
async fn cancelled_initial_http_waiter_cannot_commit_a_job_without_dispatching_it() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let held = state
        .service_deploy_permits
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    let record = deployment(Uuid::new_v4());
    state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id: record.service_id,
            spec: record.spec.clone(),
        })
        .unwrap();
    let claim = state.service_mutations.clone().lock_owned().await;
    let mut request = Box::pin(submit_initial(
        state.clone(),
        record.clone(),
        claim,
        Uuid::new_v4(),
    ));
    assert!(
        std::future::poll_fn(|context| std::task::Poll::Ready(request.as_mut().poll(context)))
            .await
            .is_pending()
    );
    drop(request);
    tokio::time::timeout(Duration::from_secs(3), async {
        while state
            .store
            .service_deployment(record.service_id)
            .unwrap()
            .is_none()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        state
            .store
            .service_deployment(record.service_id)
            .unwrap()
            .unwrap()
            .phase,
        ServicePhase::Queued
    );
    state
        .store
        .delete_pipe_auth(crate::pipe_auth::Provider::Github)
        .unwrap();
    drop(held);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let current = state
                .store
                .service_deployment(record.service_id)
                .unwrap()
                .unwrap();
            if current.phase == ServicePhase::Failed {
                assert!(current.error.unwrap().contains("no longer connected"));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn direct_cancellation_respects_git_terraform_and_controller_ownership() {
    for owner in ["git", "terraform", "controller"] {
        let root = tempfile::tempdir().unwrap();
        let state = state(root.path()).await;
        let record = deployment(Uuid::new_v4());
        if owner == "controller" {
            state
                .store
                .cluster_service_worker_admit(
                    record.service_id,
                    Uuid::new_v4(),
                    firecrab_api_types::ServiceWorkerIntent {
                        generation: 1,
                        release_generation: 1,
                        desired: firecrab_api_types::ClusterServiceDesired::Running,
                        deployment_id: record.id,
                        spec: record.spec.clone(),
                        request: record.request.clone(),
                        state: Default::default(),
                        credentials: Default::default(),
                    },
                )
                .unwrap();
        }
        insert_configuration(&state, &record);
        if owner == "git" {
            state.store.register_stack("queue-ownership").unwrap();
            state
                .store
                .bind_stack_resource(
                    "queue-ownership",
                    &firecrab_stack::Binding {
                        kind: firecrab_stack::ResourceKind::Service,
                        key: "app".into(),
                        resource_id: record.service_id,
                    },
                )
                .unwrap();
            let source = serde_json::from_value(serde_json::json!({
                "expectedGeneration": 0,
                "source": {"repository":"https://github.com/example/infra","ref":"main","manifestPath":"stack.yaml","paused":true,"pollIntervalSeconds":60}
            })).unwrap();
            state
                .store
                .configure_stack_source("queue-ownership", &source, 0)
                .unwrap();
        } else if owner == "terraform" {
            state
                .store
                .change_iac_resource_owner(&firecrab_stack::ChangeResourceOwner {
                    operation_id: Uuid::new_v4(),
                    expected_endpoint_id: state.store.iac_endpoint_id().unwrap(),
                    resource: firecrab_stack::ManagedResource {
                        kind: "service".into(),
                        id: record.service_id.to_string(),
                    },
                    expected_generation: 0,
                    expected_owner: None,
                    owner: Some(firecrab_stack::ResourceOwner {
                        manager: firecrab_stack::Manager::Terraform,
                        id: Uuid::new_v4(),
                        key: "firecrab_service.app".into(),
                    }),
                })
                .unwrap();
        }
        // Establish ownership before starting work. Active deployments cannot
        // change writers, even when the new writer will only test cancellation.
        state.store.save_service_deployment(&record).unwrap();
        let error = cancel_attempt(&state, &record).await.unwrap_err();
        assert_eq!(
            error.code(),
            match owner {
                "controller" => "controller_managed",
                "terraform" => "iac_resource_managed",
                _ => "stack_source_managed",
            }
        );
        assert_eq!(
            state
                .store
                .service_deployment(record.service_id)
                .unwrap()
                .unwrap(),
            record
        );
        assert_eq!(
            state.store.service_attempt(record.id).unwrap().unwrap(),
            record
        );
    }
}

#[tokio::test]
async fn health_repair_uses_its_own_operation_without_creating_a_new_release() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut record = deployment(Uuid::new_v4());
    let repair = Uuid::new_v4();
    record.phase = ServicePhase::Starting;
    record.recovery = Some(Box::new(firecrab_api_types::ServiceRecovery {
        replicas: Default::default(),
        repair: Some(firecrab_api_types::ReplicaRepair {
            id: repair,
            vm_id: Uuid::new_v4(),
            replacement_id: None,
            stage: firecrab_api_types::ReplicaRepairStage::Withdraw,
            transition: None,
        }),
    }));
    insert(&state, &record);
    let (permit, admitted) = admit(&state, &record).await.unwrap();
    assert_eq!(admitted, record);
    assert!(state.store.service_attempt(repair).unwrap().is_none());
    assert!(state.store.service_deploy_turn(repair).is_ok());
    let mut completed = record.clone();
    completed.phase = ServicePhase::Running;
    completed.recovery.as_mut().unwrap().repair = None;
    state.store.save_service_deployment(&completed).unwrap();
    drop(permit);
    assert!(admit(&state, &record).await.is_none());
    assert!(!state.store.service_deploy_waiting().unwrap());
}

#[tokio::test]
async fn a_manual_operation_cannot_reuse_another_services_live_repair_identity() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut repair = deployment(Uuid::new_v4());
    let operation = Uuid::new_v4();
    repair.phase = ServicePhase::Starting;
    repair.recovery = Some(Box::new(firecrab_api_types::ServiceRecovery {
        replicas: Default::default(),
        repair: Some(firecrab_api_types::ReplicaRepair {
            id: operation,
            vm_id: Uuid::new_v4(),
            replacement_id: None,
            stage: firecrab_api_types::ReplicaRepairStage::Withdraw,
            transition: None,
        }),
    }));
    insert(&state, &repair);
    state
        .store
        .enqueue_service_deployment(&repair, false)
        .unwrap();
    let mut manual = update();
    manual.id = operation;
    let previous = (*manual.update.as_ref().unwrap().previous).clone();
    insert(&state, &previous);
    let claim = state.service_mutations.clone().lock_owned().await;
    let error = deploy::submit(state.clone(), manual.clone(), claim, Uuid::new_v4(), None)
        .await
        .unwrap_err();
    assert_eq!(error.code(), "operation_conflict");
    assert_eq!(
        state
            .store
            .service_deployment(previous.service_id)
            .unwrap()
            .unwrap(),
        previous
    );
    assert!(state.store.service_attempt(operation).unwrap().is_none());
    assert!(matches!(
        state
            .store
            .claim_service_deployment(manual.service_id, manual.id, operation)
            .unwrap(),
        Claim::Obsolete
    ));
    assert_eq!(
        state.store.service_deploy_turn(operation).unwrap(),
        (true, true)
    );
    let (_permit, admitted) = admit(&state, &repair).await.unwrap();
    assert_eq!(admitted, repair);
}

#[tokio::test]
async fn restarting_an_interrupted_stop_wakes_the_next_queued_deploy() {
    let root = crate::firecracker::test_support::short_tempdir();
    let mut state = state(root.path()).await;
    let mut stopping = crate::service_runtime::deploy::tests::journal();
    stopping.phase = ServicePhase::Stopping;
    stopping.update.as_mut().unwrap().cancelled = true;
    let helper = crate::service_runtime::deploy::tests::helper(root.path(), &stopping).await;
    helper
        .auto_drain
        .store(true, std::sync::atomic::Ordering::SeqCst);
    state.network = helper.client.clone();
    insert(&state, &stopping);
    let queued = deployment(Uuid::new_v4());
    insert(&state, &queued);
    state
        .store
        .delete_pipe_auth(crate::pipe_auth::Provider::Github)
        .unwrap();
    recover(&state).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let record = state
                .store
                .service_deployment(queued.service_id)
                .unwrap()
                .unwrap();
            if record.phase == ServicePhase::Failed {
                assert!(record.error.unwrap().contains("no longer connected"));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        state
            .store
            .service_deployment(stopping.service_id)
            .unwrap()
            .unwrap()
            .phase,
        ServicePhase::Stopped
    );
    assert!(!state.store.service_deploy_waiting().unwrap());
}

#[tokio::test]
async fn an_active_deploy_allows_queueing_during_drain_but_worker_rechecks_before_capture() {
    let root = crate::firecracker::test_support::short_tempdir();
    let mut state = state(root.path()).await;
    let mut record = update();
    let previous = (*record.update.as_ref().unwrap().previous).clone();
    let helper = crate::service_runtime::deploy::tests::helper(root.path(), &record).await;
    state.network = helper.client.clone();
    helper.workers.lock().unwrap()[0].draining = true;
    let secret = Uuid::new_v4();
    record.spec.secrets.insert("PASSWORD".into(), secret);
    state
        .secrets
        .write(|vault| {
            vault.secrets.push(crate::secrets::Secret {
                summary: crate::secrets::SecretSummary {
                    id: secret,
                    name: "queue-worker-readiness".into(),
                    description: String::new(),
                    version: 1,
                    created_at: 0,
                    updated_at: 0,
                    source: None,
                },
                value: "capture-only-after-ingress-check".into(),
            });
            Ok(())
        })
        .unwrap();
    insert(&state, &previous);
    let request = || firecrab_api_types::UpdateServiceRequest {
        expected_release_id: Some(previous.id),
        operation_id: record.id,
        spec: record.spec.clone(),
        strategy: Default::default(),
    };
    let error = deploy::update(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(record.service_id.to_string()),
        ValidatedJson(request()),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.into_response().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        state
            .store
            .service_deployment(previous.service_id)
            .unwrap()
            .unwrap(),
        previous
    );
    let held = state
        .service_deploy_permits
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    let (_, Json(queued)) = deploy::update(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(record.service_id.to_string()),
        ValidatedJson(request()),
    )
    .await
    .unwrap();
    assert_eq!(queued.phase, ServicePhase::Queued);
    drop(held);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let current = state
                .store
                .service_deployment(record.service_id)
                .unwrap()
                .unwrap();
            if current.phase == ServicePhase::Failed {
                assert!(current.error.unwrap().contains("reload is still draining"));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(helper.changes.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(state.vms.lock().unwrap().is_empty());
    assert!(
        state
            .secrets
            .read(|vault| Ok(!vault.service_environments.contains_key(&record.id)))
            .unwrap()
    );
}

#[tokio::test]
async fn clean_queued_initial_deploy_survives_database_reopen_and_startup_recovery() {
    let root = tempfile::tempdir().unwrap();
    let original = state(root.path()).await;
    let record = deployment(Uuid::new_v4());
    insert(&original, &record);
    drop(original);
    let restarted = state(root.path()).await;
    let held = restarted
        .service_deploy_permits
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    recover(&restarted).await;
    assert_eq!(
        restarted
            .store
            .service_deployment(record.service_id)
            .unwrap()
            .unwrap(),
        record
    );
    assert_eq!(
        restarted.store.service_deploy_turn(record.id).unwrap(),
        (true, true)
    );
    restarted
        .store
        .delete_pipe_auth(crate::pipe_auth::Provider::Github)
        .unwrap();
    drop(held);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let current = restarted
                .store
                .service_deployment(record.service_id)
                .unwrap()
                .unwrap();
            if current.phase == ServicePhase::Failed {
                assert_eq!(current.id, record.id);
                assert!(current.error.unwrap().contains("no longer connected"));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

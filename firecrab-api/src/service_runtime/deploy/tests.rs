use super::*;
use crate::service_runtime::tests::{deployment, state};
use axum::response::IntoResponse;
use firecrab_api_types::MicroServiceResponse;
use firecrab_helper_protocol::{
    PROTOCOL_VERSION,
    framing::{read_frame, write_frame},
    network::{HelperFailure, NetworkRequest, NetworkRequestEnvelope, NetworkResponseEnvelope},
    pipe::{Config, Route},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

pub(super) fn journal() -> MicroServiceDeployment {
    let service = Uuid::new_v4();
    let mut previous = deployment(service);
    previous.phase = ServicePhase::Running;
    previous.spec.domain = Some("web.example.com".into());
    previous.spec.drain_timeout = 1;
    previous.digest = Some(format!("sha256:{}", "a".repeat(64)));
    previous.domain_route_owned = true;
    previous.instances = vec![ServiceInstance {
        vm_id: Uuid::new_v4(),
        ipv4: "172.30.0.2".into(),
        host_port: 0,
        state: VmState::Running,
        healthy: true,
    }];
    let mut current = previous.clone();
    current.id = Uuid::new_v4();
    current.phase = ServicePhase::Queued;
    current.spec.image = "ghcr.io/team/app:v2".into();
    current.digest = Some(format!("sha256:{}", "b".repeat(64)));
    current.instances = vec![ServiceInstance {
        vm_id: Uuid::new_v4(),
        ipv4: "172.30.0.3".into(),
        host_port: 0,
        state: VmState::Running,
        healthy: true,
    }];
    current.update = Some(Box::new(ServiceUpdate {
        failure: None,
        cancelled: false,
        requested_spec: current.spec.clone(),
        previous: Box::new(previous),
        strategy: DeployStrategy::Rolling,
        rollback_of: None,
        restoring: false,
        routed_new: false,
        transition: None,
        retiring: vec![],
        completed: false,
    }));
    current
}

pub(super) struct Helper {
    pub(super) client: crate::network::NetworkClient,
    config: Arc<Mutex<Config>>,
    workers: Arc<Mutex<Vec<IngressWorker>>>,
    changes: Arc<AtomicUsize>,
    auto_drain: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Helper {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(super) async fn helper(root: &std::path::Path, deployment: &MicroServiceDeployment) -> Helper {
    let path = root.join("deploy.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let previous = &deployment.update.as_ref().unwrap().previous;
    let mut initial: Config =
        serde_json::from_value(serde_json::json!({"routes":[], "tunnel":null})).unwrap();
    initial.routes.push(Route {
        host: previous.spec.domain.clone().unwrap(),
        path_prefix: "/".into(),
        upstreams: endpoints(previous).unwrap(),
        owner: Some(ServiceRouteOwner {
            service_id: previous.service_id,
            deployment_id: previous.id,
        }),
    });
    let config = Arc::new(Mutex::new(initial));
    let workers = Arc::new(Mutex::new(vec![IngressWorker {
        draining: false,
        pid: 101,
        start_time: 10,
        boot_id: "boot".into(),
    }]));
    let changes = Arc::new(AtomicUsize::new(0));
    let auto_drain = Arc::new(AtomicBool::new(false));
    let drain = auto_drain.clone();
    let (saved, live, calls) = (config.clone(), workers.clone(), changes.clone());
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let envelope: NetworkRequestEnvelope = read_frame(&mut stream).await.unwrap();
            let mut result = Ok(());
            if let NetworkRequest::Pipe {
                request: Operation::ManageServiceRoute { request },
            } = envelope.request
            {
                calls.fetch_add(1, Ordering::SeqCst);
                let mut config = saved.lock().unwrap();
                match request.updated_config(&config) {
                    Ok(next) => {
                        *config = next;
                        if drain.load(Ordering::SeqCst) {
                            for worker in live.lock().unwrap().iter_mut() {
                                worker.start_time += 1;
                            }
                        }
                    }
                    Err(detail) => result = Err(HelperFailure::Internal { detail }),
                }
            }
            let snapshot = Snapshot {
                ingress_graceful_drain: true,
                config: Some(saved.lock().unwrap().clone()),
                ingress_workers: Some(live.lock().unwrap().clone()),
                units: vec![firecrab_helper_protocol::pipe::UnitStatus {
                    unit: "firecrab-ingress.service".into(),
                    load_state: "loaded".into(),
                    active_state: "active".into(),
                    sub_state: "running".into(),
                    main_pid: 100,
                }],
                nginx_available: true,
                cloudflared_available: false,
                quick_tunnel: None,
                quick_tunnels: vec![],
                firecrab_tunnels: vec![],
            };
            write_frame(
                &mut stream,
                &NetworkResponseEnvelope {
                    version: PROTOCOL_VERSION,
                    request_id: envelope.request_id,
                    result,
                    pipe: Some(snapshot),
                },
            )
            .await
            .unwrap();
        }
    });
    Helper {
        client: crate::network::NetworkClient::with_socket_path(path),
        config,
        workers,
        changes,
        auto_drain,
        task,
    }
}

#[tokio::test]
async fn restart_finishes_the_interrupted_surge_before_alphabetically_earlier_queued_work() {
    let root = crate::firecracker::test_support::short_tempdir();
    let mut state = state(root.path()).await;
    let mut active = journal();
    active.spec.name = "z-started".into();
    active.spec.drain_timeout = 10;
    let helper = helper(root.path(), &active).await;
    state.network = helper.client.clone();
    state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id: active.service_id,
            spec: active.spec.clone(),
        })
        .unwrap();
    let next = endpoints(&active).unwrap();
    begin_transition(&state, &mut active, next, vec![], false)
        .await
        .unwrap();
    let mut queued = journal();
    queued.spec.name = "a-queued".into();
    queued.instances.clear();
    state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id: queued.service_id,
            spec: queued.spec.clone(),
        })
        .unwrap();
    state.store.save_service_deployment(&queued).unwrap();
    // A wrongly scheduled queued worker must fail locally, without registry I/O.
    state
        .store
        .delete_pipe_auth(crate::pipe_auth::Provider::Github)
        .unwrap();
    recover(&state).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        state
            .store
            .service_deployment(active.service_id)
            .unwrap()
            .unwrap()
            .phase,
        ServicePhase::Draining
    );
    assert_eq!(
        state
            .store
            .service_deployment(queued.service_id)
            .unwrap()
            .unwrap()
            .phase,
        ServicePhase::Queued,
        "queued work must not consume capacity reserved by the interrupted surge"
    );
    assert_eq!(helper.changes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn drain_timeout_retains_ownership_and_retry_does_not_switch_twice() {
    let root = crate::firecracker::test_support::short_tempdir();
    let mut state = state(root.path()).await;
    let mut record = journal();
    let helper = helper(root.path(), &record).await;
    state.network = helper.client.clone();
    let old_vm = record.update.as_ref().unwrap().previous.instances[0].vm_id;
    state.store.save_service_deployment(&record).unwrap();
    let next = endpoints(&record).unwrap();
    begin_transition(&state, &mut record, next, vec![old_vm], false)
        .await
        .unwrap();
    let error = finish_transition(&state, &mut record).await.unwrap_err();
    assert!(error.contains("drainTimeout"));
    assert!(state.store.vm_has_service_owner(old_vm).unwrap());
    assert_eq!(record.update.as_ref().unwrap().previous.instances.len(), 1);
    assert_eq!(
        helper.config.lock().unwrap().routes[0]
            .owner
            .as_ref()
            .unwrap()
            .deployment_id,
        record.id
    );
    // PID reuse does not keep a finished generation alive.
    *helper.workers.lock().unwrap() = vec![IngressWorker {
        draining: false,
        pid: 101,
        start_time: 11,
        boot_id: "boot".into(),
    }];
    finish_transition(&state, &mut record).await.unwrap();
    assert!(!state.store.vm_has_service_owner(old_vm).unwrap());
    assert!(
        record
            .update
            .as_ref()
            .unwrap()
            .previous
            .instances
            .is_empty()
    );
    assert_eq!(helper.changes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn lost_reply_can_reconcile_a_switch_without_stale_owner_cleanup() {
    let root = crate::firecracker::test_support::short_tempdir();
    let mut state = state(root.path()).await;
    let mut record = journal();
    let helper = helper(root.path(), &record).await;
    state.network = helper.client.clone();
    state.store.save_service_deployment(&record).unwrap();
    let old_vm = record.update.as_ref().unwrap().previous.instances[0].vm_id;
    let next = endpoints(&record).unwrap();
    begin_transition(&state, &mut record, next.clone(), vec![old_vm], false)
        .await
        .unwrap();
    let previous = record.update.as_ref().unwrap().previous.clone();
    let request = ServiceRouteRequest::Replace {
        expected_owner: ServiceRouteOwner {
            service_id: record.service_id,
            deployment_id: previous.id,
        },
        expected_upstreams: endpoints(&previous).unwrap(),
        owner: ServiceRouteOwner {
            service_id: record.service_id,
            deployment_id: record.id,
        },
        host: record.spec.domain.clone().unwrap(),
        upstreams: next,
    };
    // Simulate Nginx accepting the exact change before the API receives its reply.
    let applied = request
        .updated_config(&helper.config.lock().unwrap())
        .unwrap();
    *helper.config.lock().unwrap() = applied;
    helper.workers.lock().unwrap().clear();
    let persisted = state
        .store
        .service_deployment(record.service_id)
        .unwrap()
        .unwrap();
    assert!(
        !persisted
            .update
            .as_ref()
            .unwrap()
            .transition
            .as_ref()
            .unwrap()
            .confirmed
    );
    record = persisted;
    finish_transition(&state, &mut record).await.unwrap();
    assert!(
        record
            .update
            .as_ref()
            .unwrap()
            .previous
            .instances
            .is_empty()
    );
    assert_eq!(
        helper.config.lock().unwrap().routes[0]
            .owner
            .as_ref()
            .unwrap()
            .deployment_id,
        record.id
    );
}

#[tokio::test]
async fn operation_identity_rejects_conflicting_retries_and_other_services() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut record = journal();
    record.update.as_mut().unwrap().completed = true;
    record.phase = ServicePhase::Running;
    state
        .store
        .insert_micro_service(&firecrab_api_types::MicroServiceResponse {
            id: record.service_id,
            spec: record.spec.clone(),
        })
        .unwrap();
    state.store.save_service_deployment(&record).unwrap();
    let body = UpdateServiceRequest {
        expected_release_id: None,
        operation_id: record.id,
        spec: record.spec.clone(),
        strategy: DeployStrategy::Rolling,
    };
    let (status, Json(repeated)) = accept(
        state.clone(),
        RequestId(Uuid::new_v4()),
        record.service_id,
        body.clone(),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(repeated.id, record.id);
    let mut changed = body.clone();
    changed.spec.cpu += 1;
    let error = accept(
        state.clone(),
        RequestId(Uuid::new_v4()),
        record.service_id,
        changed,
        None,
        None,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    let error = accept(
        state.clone(),
        RequestId(Uuid::new_v4()),
        Uuid::new_v4(),
        body,
        None,
        None,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    assert!(state.store.load_all().unwrap().is_empty());
}

#[tokio::test]
async fn releases_stay_immutable_after_stop_and_ownership_covers_both_generations() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut record = journal();
    let old = (*record.update.as_ref().unwrap().previous).clone();
    state
        .store
        .insert_micro_service(&firecrab_api_types::MicroServiceResponse {
            id: record.service_id,
            spec: old.spec.clone(),
        })
        .unwrap();
    state.store.save_service_deployment(&old).unwrap();
    state.store.save_service_deployment(&record).unwrap();
    let old_vm = old.instances[0].vm_id;
    let new_vm = record.instances[0].vm_id;
    assert_eq!(
        state
            .store
            .service_deployment_for_vm(old_vm)
            .unwrap()
            .unwrap()
            .id,
        old.id
    );
    assert_eq!(
        state
            .store
            .service_deployment_for_vm(new_vm)
            .unwrap()
            .unwrap()
            .id,
        record.id
    );
    assert!(
        ensure_vm_policy_editable(&state, old_vm, Uuid::new_v4())
            .await
            .is_err()
    );
    record.phase = ServicePhase::Stopped;
    record.spec.image = "ghcr.io/team/app:unhealthy".into();
    state.store.save_service_deployment(&record).unwrap();
    let releases = state.store.service_releases(record.service_id).unwrap();
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].digest, old.digest);
    assert_eq!(releases[0].spec, old.spec);
    assert!(releases[0].instances.is_empty());
    assert!(!releases[0].domain_route_owned);
    assert!(!format!("{releases:?}").contains("test-only-secret"));
}

#[tokio::test]
async fn missing_queue_binding_is_rejected_before_mutating_the_active_release() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let record = journal();
    let previous = &record.update.as_ref().unwrap().previous;
    state.store.save_service_deployment(previous).unwrap();
    let mut spec = record.spec.clone();
    spec.queues.insert(
        "JOBS".into(),
        firecrab_api_types::QueueBinding {
            queue_id: Uuid::new_v4(),
            access: firecrab_api_types::QueueAccess::Consume,
        },
    );
    let response = update(
        State(state.clone()),
        Extension(RequestId(record.id)),
        Path(record.service_id.to_string()),
        ValidatedJson(UpdateServiceRequest {
            expected_release_id: Some(previous.id),
            operation_id: record.id,
            spec,
            strategy: DeployStrategy::Rolling,
        }),
    )
    .await
    .unwrap_err()
    .into_response();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        state
            .store
            .service_deployment(record.service_id)
            .unwrap()
            .unwrap()
            .id,
        previous.id
    );
    assert!(state.store.service_attempt(record.id).unwrap().is_none());
}

#[tokio::test]
async fn direct_port_updates_fail_before_mutating_a_live_release() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut record = journal();
    record.update = None;
    record.phase = ServicePhase::Running;
    record.spec.domain = None;
    state.store.save_service_deployment(&record).unwrap();
    let body = UpdateServiceRequest {
        expected_release_id: None,
        operation_id: Uuid::new_v4(),
        spec: record.spec.clone(),
        strategy: DeployStrategy::Rolling,
    };
    let error = accept(
        state.clone(),
        RequestId(Uuid::new_v4()),
        record.service_id,
        body,
        None,
        None,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
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
async fn failed_candidate_restores_healthy_previous_without_registry_or_image_cache() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = crate::firecracker::test_support::short_tempdir();
    let mut state = state(root.path()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut record = journal();
    let old = record.update.as_mut().unwrap();
    old.restoring = true;
    old.failure = Some("candidate health failed".into());
    old.previous.spec.port = listener.local_addr().unwrap().port();
    old.previous.instances[0].ipv4 = "127.0.0.1".into();
    let mut vm = vms::test_support::record("previous", old.previous.instances[0].vm_id);
    vm.state = VmState::Running;
    vm.template_version = old.previous.digest.clone().unwrap();
    vm.env = old.previous.spec.env.clone();
    state.vms.lock().unwrap().insert(vm.id, vm);
    let old = (*record.update.as_ref().unwrap().previous).clone();
    let operation = record.id;
    state.store.save_service_deployment(&old).unwrap();
    state.store.save_service_deployment(&record).unwrap();
    let helper = helper(root.path(), &record).await;
    helper.auto_drain.store(true, Ordering::SeqCst);
    state.network = helper.client.clone();
    let app = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 2048];
        assert!(stream.read(&mut request).await.unwrap() > 0);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
            .await
            .unwrap();
    });
    restore(&state, &mut record).await.unwrap();
    app.await.unwrap();
    assert_eq!(record.id, old.id);
    assert_eq!(record.phase, ServicePhase::Running);
    assert!(
        record
            .error
            .as_ref()
            .unwrap()
            .contains("candidate health failed")
    );
    let failed = state.store.service_attempt(operation).unwrap().unwrap();
    assert_eq!(failed.phase, ServicePhase::Failed);
    assert!(failed.update.as_ref().unwrap().completed);
    assert!(failed.finished_at.is_some());
    assert!(failed.instances.is_empty());
    assert_eq!(
        state
            .store
            .service_deployment(old.service_id)
            .unwrap()
            .unwrap()
            .id,
        old.id
    );
    assert_eq!(
        helper.config.lock().unwrap().routes[0]
            .owner
            .as_ref()
            .unwrap()
            .deployment_id,
        old.id
    );
}

#[tokio::test]
async fn stale_release_and_stale_retry_do_not_mutate_the_active_operation() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let record = journal();
    state.store.save_service_deployment(&record).unwrap();
    let error = accept(
        state.clone(),
        RequestId(Uuid::new_v4()),
        record.service_id,
        UpdateServiceRequest {
            expected_release_id: Some(Uuid::new_v4()),
            operation_id: Uuid::new_v4(),
            spec: record.spec.clone(),
            strategy: DeployStrategy::Rolling,
        },
        None,
        None,
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "release_changed");
    let error = retry(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(record.service_id.to_string()),
        ValidatedJson(RetryServiceRequest {
            operation_id: Uuid::new_v4(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "deploy_not_retryable");
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
async fn cancelled_http_waiter_does_not_cancel_committed_stop_or_lose_ownership() {
    let root = crate::firecracker::test_support::short_tempdir();
    let mut state = state(root.path()).await;
    let mut record = journal();
    record.update.as_mut().unwrap().cancelled = true;
    record.phase = ServicePhase::Stopping;
    let helper = helper(root.path(), &record).await;
    helper.auto_drain.store(true, Ordering::SeqCst);
    state.network = helper.client.clone();
    let service = record.service_id;
    let claim = state.service_mutations.clone().lock_owned().await;
    {
        let mut future = std::pin::pin!(submit(state.clone(), record, claim, Uuid::new_v4()));
        // One poll starts the independently owned commit/dispatch task.
        assert!(
            std::future::poll_fn(|context| std::task::Poll::Ready(future.as_mut().poll(context)))
                .await
                .is_pending()
        );
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if state
                .store
                .service_deployment(service)
                .unwrap()
                .is_some_and(|current| current.phase == ServicePhase::Stopped)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        state
            .store
            .service_deployment(service)
            .unwrap()
            .unwrap()
            .instances
            .is_empty()
    );
    assert!(helper.config.lock().unwrap().routes.is_empty());
}

#[test]
fn surge_reserves_the_larger_forward_or_rollback_peak() {
    assert_eq!(peak_resources(4, 3, 1, 3, DeployStrategy::Rolling), 13);
    assert_eq!(peak_resources(1, 3, 4, 3, DeployStrategy::Rolling), 13);
    assert_eq!(peak_resources(4, 3, 1, 3, DeployStrategy::BlueGreen), 15);
}

#[tokio::test]
async fn release_history_keeps_twenty_and_migrates_legacy_records_without_new_fields() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut record = journal();
    record.update = None;
    record.phase = ServicePhase::Running;
    let mut ids = vec![];
    for _ in 0..23 {
        record.id = Uuid::new_v4();
        ids.push(record.id);
        state.store.save_service_deployment(&record).unwrap();
    }
    let releases = state.store.service_releases(record.service_id).unwrap();
    assert_eq!(releases.len(), 20);
    assert_eq!(releases[0].id, ids[22]);
    assert_eq!(releases[19].id, ids[3]);
    let mut legacy = serde_json::to_value(&record).unwrap();
    for field in ["startedAt", "finishedAt", "update"] {
        legacy.as_object_mut().unwrap().remove(field);
    }
    drop(state);
    let db_path = root.path().join("state.db");
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute("DELETE FROM micro_service_attempts", [])
        .unwrap();
    connection
        .execute("DELETE FROM micro_service_releases", [])
        .unwrap();
    connection
        .execute(
            "UPDATE micro_service_deployments SET deployment = ?1",
            [legacy.to_string()],
        )
        .unwrap();
    drop(connection);
    let store = crate::persistence::Store::open(&db_path).unwrap();
    let releases = store.service_releases(record.service_id).unwrap();
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].id, record.id);
    assert!(releases[0].instances.is_empty());
    assert_eq!(releases[0].started_at, 0);
    assert!(store.service_attempt(record.id).unwrap().is_some());
}

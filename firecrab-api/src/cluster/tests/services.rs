//! Cross-host lifecycle acceptance uses real HTTPS/mTLS and fixture VM observations.
use super::super::services::reconcile_one as reconcile_service;
use super::*;

fn service_spec(hosts: &[HostReport]) -> ClusterServiceSpec {
    ClusterServiceSpec {
        service: serde_json::from_value(serde_json::json!({"name":"web","image":format!("ghcr.io/team/web@sha256:{}","a".repeat(64)),"port":8080,"cpu":1,"ram":512,"replicas":hosts.len(),"drainTimeout":0})).unwrap(),
        mode: ClusterServiceMode::Replicated,
        targets: hosts.iter().map(|h| ClusterServiceTarget { host_id:h.id,replicas:1,request:DeployMicroServiceRequest { micro_network_id:Some(h.networks[0].id),host_port:30100 } }).collect(),
    }
}
fn create(store: &Store, spec: ClusterServiceSpec) -> ClusterService {
    store
        .cluster_service_admit(
            "create",
            None,
            ClusterServiceAction::Create,
            None,
            Some(spec),
            1001,
        )
        .unwrap()
}
fn stopped(id: Uuid, intent: &ServiceWorkerIntent) -> MicroServiceDeployment {
    MicroServiceDeployment {
        id: intent.deployment_id,
        service_id: id,
        started_at: 0,
        finished_at: None,
        update: None,
        phase: ServicePhase::Stopped,
        spec: intent.spec.clone(),
        request: intent.request.clone(),
        digest: Some("sha256:".to_owned() + &"a".repeat(64)),
        instances: vec![],
        log: vec![],
        domain_route_owned: false,
        error: None,
    }
}

#[test]
fn service_identity_release_and_retry_intents_survive_restart() {
    let (root, store, _) = world(2);
    let spec = service_spec(&[report(1), report(2)]);
    let s = create(&store, spec.clone());
    let reopened = Store::open(&root.path().join("state.db")).unwrap();
    assert_eq!(reopened.cluster_service(s.id).unwrap(), s);
    assert_eq!(create(&reopened, spec), s);
    assert_eq!(reopened.cluster_services().unwrap().len(), 1);
    assert_eq!(s.generation, 1);
    assert_eq!(s.release_generation, 1);
    assert_eq!(s.hosts.len(), 2);
    assert_ne!(
        s.hosts[0].intent.deployment_id,
        s.hosts[1].intent.deployment_id
    );
    for host in &s.hosts {
        assert_eq!(host.intent.spec.image, s.spec.service.image);
        assert_eq!(host.intent.release_generation, 1);
        assert!(host.role.is_none());
    }
    assert_eq!(store.cluster_hosts(1002).unwrap()[0].reserved.cpu, 1);
    let ops = reopened.cluster_service_operations(s.id).unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].service, s);
}

#[test]
fn concurrent_admission_reserves_capacity_and_deduplicates_retry_keys() {
    let (root, store, _) = world(2);
    let spec = service_spec(&[report(1), report(2)]);
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let db = root.path().join("state.db");
            let spec = spec.clone();
            std::thread::spawn(move || create(&Store::open(&db).unwrap(), spec).id)
        })
        .collect();
    let ids: BTreeSet<_> = tasks.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(ids.len(), 1);
    let mut second = spec.clone();
    second.service.name = "second".into();
    store
        .cluster_service_admit(
            "second",
            None,
            ClusterServiceAction::Create,
            None,
            Some(second),
            1002,
        )
        .unwrap();
    let mut third = spec;
    third.service.name = "third".into();
    assert!(matches!(
        store.cluster_service_admit(
            "third",
            None,
            ClusterServiceAction::Create,
            None,
            Some(third),
            1002
        ),
        Err(ClusterError::Capacity)
    ));
    assert!(matches!(
        store.cluster_admit("vm", super::spec(&[report(1), report(2)]), 1002),
        Err(ClusterError::Capacity)
    ));
}

#[test]
fn late_create_responses_cannot_overwrite_new_release_or_stop_intents() {
    let (_root, store, _) = world(2);
    let s = create(&store, service_spec(&[report(1), report(2)]));
    let (token, mut stale) = store
        .cluster_service_claim(s.id, 1002, true)
        .unwrap()
        .unwrap();
    let mut spec = s.spec.clone();
    spec.service.image = format!("ghcr.io/team/web@sha256:{}", "b".repeat(64));
    let updated = store
        .cluster_service_admit(
            "update",
            Some(s.id),
            ClusterServiceAction::Update,
            Some(1),
            Some(spec.clone()),
            1003,
        )
        .unwrap();
    assert_eq!(updated.release_generation, 2);
    stale.pending = false;
    store.cluster_service_finish(&stale, token, 1004).unwrap();
    assert_eq!(store.cluster_service(s.id).unwrap(), updated);
    assert!(matches!(
        store.cluster_service_admit(
            "stale-stop",
            Some(s.id),
            ClusterServiceAction::Stop,
            Some(1),
            None,
            1005
        ),
        Err(ClusterError::Conflict(_))
    ));
    let stop = store
        .cluster_service_admit(
            "stop",
            Some(s.id),
            ClusterServiceAction::Stop,
            Some(2),
            None,
            1005,
        )
        .unwrap();
    assert_eq!(stop.release_generation, 2);
    assert_eq!(stop.generation, 3);
    assert_eq!(stop.desired, ClusterServiceDesired::Stopped);
    assert_eq!(
        store.cluster_hosts(1006).unwrap()[0].reserved.cpu,
        1,
        "unobserved ownership stays reserved during Stop"
    );
    assert_eq!(
        store
            .cluster_service_admit(
                "update",
                Some(s.id),
                ClusterServiceAction::Update,
                Some(1),
                Some(spec),
                1007
            )
            .unwrap(),
        stop
    );
    assert_eq!(store.cluster_service_operations(s.id).unwrap().len(), 3);
}

#[test]
fn invalid_images_foreign_networks_and_host_changes_do_not_persist_intent() {
    let (_root, store, _) = world(2);
    let mut tagged = service_spec(&[report(1), report(2)]);
    tagged.service.image = "ghcr.io/team/web:latest".into();
    assert!(create_error(&store, tagged).is_err());
    let mut spec = service_spec(&[report(1), report(2)]);
    spec.targets[0].request.micro_network_id = spec.targets[1].request.micro_network_id;
    assert!(create_error(&store, spec).is_err());
    let mut spec = service_spec(&[report(1), report(2)]);
    spec.service
        .secrets
        .insert("PASSWORD".into(), Uuid::new_v4());
    assert!(create_error(&store, spec).is_err());
    assert!(store.cluster_services().unwrap().is_empty());
    let s = create(&store, service_spec(&[report(1), report(2)]));
    let mut spec = s.spec.clone();
    spec.targets[0].request.host_port += 1;
    assert!(matches!(
        store.cluster_service_admit(
            "move",
            Some(s.id),
            ClusterServiceAction::Update,
            Some(1),
            Some(spec),
            1002
        ),
        Err(ClusterError::Conflict(_))
    ));
}
fn create_error(store: &Store, spec: ClusterServiceSpec) -> Result<ClusterService, ClusterError> {
    store.cluster_service_admit(
        "bad",
        None,
        ClusterServiceAction::Create,
        None,
        Some(spec),
        1001,
    )
}

#[test]
fn worker_generations_and_foreign_deployments_are_fenced_before_local_mutation() {
    let (_root, store, _) = world(1);
    let s = create(&store, service_spec(&[report(1)]));
    let owner = Uuid::from_u128(500);
    let intent = s.hosts[0].intent.clone();
    let r = store
        .cluster_service_worker_admit(s.id, owner, intent.clone())
        .unwrap();
    assert_eq!(
        store
            .cluster_service_worker_admit(s.id, owner, intent.clone())
            .unwrap(),
        r
    );
    assert!(matches!(
        store.cluster_service_worker_admit(Uuid::new_v4(), owner, intent.clone()),
        Err(ClusterError::Conflict(_))
    ));
    assert!(matches!(
        store.cluster_service_worker_admit(s.id, Uuid::new_v4(), intent.clone()),
        Err(ClusterError::Conflict(_))
    ));
    let mut next = intent.clone();
    next.generation = 2;
    next.desired = ClusterServiceDesired::Stopped;
    store
        .cluster_service_worker_admit(s.id, owner, next)
        .unwrap();
    assert!(
        store
            .cluster_service_worker_admit(s.id, owner, intent)
            .is_err()
    );
    assert!(store.cluster_service_worker_finish(&r).is_err());
    let foreign = Uuid::new_v4();
    store
        .insert_micro_service(&MicroServiceResponse {
            id: foreign,
            spec: r.intent.spec.clone(),
        })
        .unwrap();
    let mut intent = r.intent;
    intent.deployment_id = Uuid::new_v4();
    assert!(
        store
            .cluster_service_worker_admit(foreign, owner, intent)
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_tls_hosts_share_identity_and_release_and_restart_reconciles_without_duplicates() {
    let world = World::new(2).await;
    let reports: Vec<_> = world.hosts.iter().map(|h| h.report.clone()).collect();
    let mut spec = service_spec(&reports);
    // Live application health is a real HTTP request; VM ownership uses fixture records.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    spec.service.port = listener.local_addr().unwrap().port();
    let health = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().route("/health", axum::routing::get(|| async { "ok" })),
        )
        .await
        .unwrap();
    });
    let s = world
        .controller
        .store
        .cluster_service_admit(
            "tls",
            None,
            ClusterServiceAction::Create,
            None,
            Some(spec),
            now_ms(),
        )
        .unwrap();
    reconcile_service(world.controller.clone(), s.id, true)
        .await
        .unwrap();
    for (host, target) in world.hosts.iter().zip(&s.hosts) {
        let _lock = host.state.cluster.lock_worker(s.id).await;
        let receipt = host
            .state
            .store
            .cluster_service_worker(s.id)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.intent, target.intent);
        assert_eq!(receipt.controller_id, Uuid::from_u128(500));
        assert_eq!(host.state.store.cluster_service_workers().unwrap().len(), 1);
        // The real local API failed before touching resources because Auth was not connected.
        assert!(receipt.error.is_some());
        host.state
            .store
            .insert_micro_service(&MicroServiceResponse {
                id: s.id,
                spec: target.intent.spec.clone(),
            })
            .unwrap();
        let mut d = stopped(s.id, &target.intent);
        d.phase = ServicePhase::Running;
        let mut vm = crate::handlers::vms::test_support::record("replica", Uuid::new_v4());
        vm.state = VmState::Running;
        host.state.store.insert(&vm).unwrap();
        host.state.vms.lock().unwrap().insert(vm.id, vm.clone());
        d.instances.push(ServiceInstance {
            vm_id: vm.id,
            ipv4: "127.0.0.1".into(),
            host_port: target.intent.request.host_port,
            state: VmState::Running,
            healthy: true,
        });
        host.state.store.save_service_deployment(&d).unwrap();
    }
    let mut restarted = state(&world.root.path().join("controller")).await;
    restarted.cluster = world.controller.cluster.clone();
    reconcile_service(restarted.clone(), s.id, true)
        .await
        .unwrap();
    let running = restarted.store.cluster_service(s.id).unwrap();
    assert_eq!(running.observed, ClusterServiceObserved::Running);
    assert!(!running.pending && !running.degraded);
    for (host, target) in world.hosts.iter().zip(&s.hosts) {
        let d = host.state.store.service_deployment(s.id).unwrap().unwrap();
        assert_eq!(d.id, target.intent.deployment_id);
        assert_eq!(d.service_id, s.id);
        assert_eq!(d.digest, Some("sha256:".to_owned() + &"a".repeat(64)));
        assert_eq!(host.state.store.load_all().unwrap().len(), 1);
        let response = world
            .controller
            .cluster
            .client(host.report.id)
            .unwrap()
            .post(format!(
                "{}/api/micro-services/{}/stop",
                host.report.endpoint, s.id
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 409);
    }
    health.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_retries_unreachable_host_after_restart_and_retains_unresolved_reservations() {
    let mut world = World::new(2).await;
    let reports: Vec<_> = world.hosts.iter().map(|h| h.report.clone()).collect();
    let s = world
        .controller
        .store
        .cluster_service_admit(
            "partial",
            None,
            ClusterServiceAction::Create,
            None,
            Some(service_spec(&reports)),
            now_ms(),
        )
        .unwrap();
    reconcile_service(world.controller.clone(), s.id, true)
        .await
        .unwrap();
    world.hosts[1].task.abort();
    let unavailable = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    world.hosts[1].report.endpoint = format!("https://{}", unavailable.local_addr().unwrap());
    drop(unavailable);
    world
        .controller
        .store
        .cluster_register(&world.hosts[1].report, now_ms())
        .unwrap();
    let stopping = world
        .controller
        .store
        .cluster_service_admit(
            "stop",
            Some(s.id),
            ClusterServiceAction::Stop,
            Some(1),
            None,
            now_ms(),
        )
        .unwrap();
    reconcile_service(world.controller.clone(), s.id, true)
        .await
        .unwrap();
    let partial = world.controller.store.cluster_service(s.id).unwrap();
    assert!(partial.pending && partial.degraded);
    assert_eq!(partial.hosts[0].reservation.cpu, 0);
    assert_eq!(partial.hosts[1].reservation.cpu, 1);
    assert_eq!(
        partial.hosts[1].intent.deployment_id,
        stopping.hosts[1].intent.deployment_id
    );
    let mut restarted = state(&world.root.path().join("controller")).await;
    restarted.cluster = world.controller.cluster.clone();
    let (endpoint, task) = serve(
        world.hosts[1].state.clone(),
        world.hosts[1].state.cluster.paths.clone().unwrap(),
    )
    .await;
    world.hosts[1].task = task;
    world.hosts[1].report.endpoint = endpoint;
    restarted
        .store
        .cluster_register(&world.hosts[1].report, now_ms())
        .unwrap();
    reconcile_service(restarted.clone(), s.id, true)
        .await
        .unwrap();
    let stopped = restarted.store.cluster_service(s.id).unwrap();
    assert_eq!(stopped.observed, ClusterServiceObserved::Stopped);
    assert!(!stopped.pending && !stopped.degraded);
    assert!(stopped.hosts.iter().all(|h| h.reservation.cpu == 0));
    assert_eq!(restarted.store.cluster_services().unwrap().len(), 1);
    for host in &world.hosts {
        assert_eq!(host.state.store.cluster_service_workers().unwrap().len(), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_service_routes_validate_and_expose_operations_with_authentication() {
    let world = World::new(2).await;
    let client = world.client();
    let spec = service_spec(
        &world
            .hosts
            .iter()
            .map(|h| h.report.clone())
            .collect::<Vec<_>>(),
    );
    let url = format!("{}/api/cluster/services", world.url);
    assert_eq!(
        client.post(&url).json(&spec).send().await.unwrap().status(),
        400
    );
    let response = client
        .post(&url)
        .header("Idempotency-Key", "public")
        .json(&spec)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let service: ClusterService = response.json().await.unwrap();
    let again: ClusterService = client
        .post(&url)
        .header("Idempotency-Key", "public")
        .json(&spec)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(again.id, service.id);
    let ops: Vec<ClusterServiceOperation> = client
        .get(format!("{url}/{}/operations", service.id))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].id, service.operation_id);
    let worker_url = format!(
        "{}/api/cluster/worker/services/{}",
        world.hosts[0].report.endpoint, service.id
    );
    let foreign = world.hosts[1]
        .state
        .cluster
        .client(world.hosts[0].report.id)
        .unwrap();
    assert_eq!(
        foreign
            .put(&worker_url)
            .json(&service.hosts[0].intent)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let stop = client
        .post(format!("{url}/{}/stop", service.id))
        .header("Idempotency-Key", "stale")
        .json(&StopClusterServiceRequest {
            expected_generation: 0,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(stop.status(), 409);
    let detail = format!("{url}/{}", service.id);
    let listed: Vec<ClusterService> = client.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(listed.len(), 1);
    let fetched: ClusterService = client
        .get(&detail)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fetched.id, service.id);
    let mut next = spec;
    next.service.image = format!("ghcr.io/team/web@sha256:{}", "b".repeat(64));
    let body = UpdateClusterServiceRequest {
        expected_generation: 1,
        spec: next,
    };
    let response = client
        .put(&detail)
        .header("Idempotency-Key", "public-update")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let updated: ClusterService = response.json().await.unwrap();
    assert_eq!(updated.release_generation, 2);
    let retry: ClusterService = client
        .post(format!("{detail}/update"))
        .header("Idempotency-Key", "public-update")
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retry.generation, 2);
    let response = client
        .post(format!("{detail}/stop"))
        .header("Idempotency-Key", "public-stop")
        .json(&StopClusterServiceRequest {
            expected_generation: 2,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let stopped: ClusterService = response.json().await.unwrap();
    assert_eq!(stopped.desired, ClusterServiceDesired::Stopped);
    assert_eq!(stopped.generation, 3);
    assert_eq!(
        client
            .post(format!("{detail}/reconcile"))
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
    let history: Vec<ClusterServiceOperation> = client
        .get(format!("{detail}/operations"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(history.len(), 3);
    assert_eq!(
        client
            .get(format!("{url}/{}", Uuid::new_v4()))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_service_apis_use_reserved_ids_and_recover_interrupted_create_and_update() {
    let world = World::new(2).await;
    for host in &world.hosts {
        host.state
            .store
            .put_pipe_auth(&crate::pipe_auth::SavedAuth::Github {
                username: "team".into(),
                token: "fixture-token".into(),
            })
            .unwrap();
        // Pause image/guest execution while exercising real admission and recovery APIs.
        host.state
            .service_deploy_permits
            .clone()
            .acquire_owned()
            .await
            .unwrap()
            .forget();
    }
    let reports: Vec<_> = world.hosts.iter().map(|h| h.report.clone()).collect();
    let mut configuration = service_spec(&reports);
    for target in &mut configuration.targets {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        target.request.host_port = listener.local_addr().unwrap().port();
    }
    let initial = world
        .controller
        .store
        .cluster_service_admit(
            "local-create",
            None,
            ClusterServiceAction::Create,
            None,
            Some(configuration),
            now_ms(),
        )
        .unwrap();
    reconcile_service(world.controller.clone(), initial.id, true)
        .await
        .unwrap();
    for (host, target) in world.hosts.iter().zip(&initial.hosts) {
        let d = host
            .state
            .store
            .service_deployment(initial.id)
            .unwrap()
            .unwrap_or_else(|| {
                panic!(
                    "deployment was not queued: {:?}",
                    host.state.store.cluster_service_worker(initial.id).unwrap()
                )
            });
        assert_eq!(d.id, target.intent.deployment_id);
        assert_eq!(d.service_id, initial.id);
        assert_eq!(d.phase, ServicePhase::Queued);
        assert_eq!(host.state.store.list_micro_services().unwrap().len(), 1);
        crate::service_runtime::recover(&host.state).await;
    }
    let mut next = initial.spec.clone();
    next.service.image = format!("ghcr.io/team/web@sha256:{}", "b".repeat(64));
    next.service.env.insert("RELEASE".into(), "two".into());
    let updated = world
        .controller
        .store
        .cluster_service_admit(
            "local-update",
            Some(initial.id),
            ClusterServiceAction::Update,
            Some(1),
            Some(next),
            now_ms(),
        )
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        reconcile_service(world.controller.clone(), initial.id, true)
            .await
            .unwrap();
        let all = world
            .hosts
            .iter()
            .zip(&updated.hosts)
            .all(|(host, target)| {
                host.state
                    .store
                    .service_deployment(initial.id)
                    .unwrap()
                    .is_some_and(|d| d.id == target.intent.deployment_id)
            });
        if all {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "replacement did not resume after restart"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    for (host, target) in world.hosts.iter().zip(&updated.hosts) {
        let d = host
            .state
            .store
            .service_deployment(initial.id)
            .unwrap()
            .unwrap();
        assert_eq!(d.spec.image, updated.spec.service.image);
        assert_eq!(d.spec.env["RELEASE"], "two");
        assert_eq!(d.id, target.intent.deployment_id);
        assert_eq!(host.state.store.list_micro_services().unwrap().len(), 1);
        let old = &initial
            .hosts
            .iter()
            .find(|h| h.host_id == host.report.id)
            .unwrap()
            .intent;
        let response = world
            .controller
            .cluster
            .client(host.report.id)
            .unwrap()
            .put(format!(
                "{}/api/cluster/worker/services/{}",
                host.report.endpoint, initial.id
            ))
            .json(old)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 409);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_local_cleanup_retains_vm_ownership_and_stop_retries_without_touching_foreign_vm() {
    let world = World::new(1).await;
    let host = &world.hosts[0];
    let s = world
        .controller
        .store
        .cluster_service_admit(
            "cleanup",
            None,
            ClusterServiceAction::Create,
            None,
            Some(service_spec(std::slice::from_ref(&host.report))),
            now_ms(),
        )
        .unwrap();
    reconcile_service(world.controller.clone(), s.id, true)
        .await
        .unwrap();
    let mut d = stopped(s.id, &s.hosts[0].intent);
    d.phase = ServicePhase::Failed;
    let mut vm = crate::handlers::vms::test_support::record("owned", Uuid::new_v4());
    vm.state = VmState::Starting;
    host.state.store.insert(&vm).unwrap();
    host.state.vms.lock().unwrap().insert(vm.id, vm.clone());
    let foreign = crate::handlers::vms::test_support::record("foreign", Uuid::new_v4());
    host.state.store.insert(&foreign).unwrap();
    host.state
        .vms
        .lock()
        .unwrap()
        .insert(foreign.id, foreign.clone());
    d.instances.push(ServiceInstance {
        vm_id: vm.id,
        ipv4: "127.0.0.1".into(),
        host_port: 30100,
        state: VmState::Starting,
        healthy: false,
    });
    host.state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id: s.id,
            spec: d.spec.clone(),
        })
        .unwrap();
    host.state.store.save_service_deployment(&d).unwrap();
    world
        .controller
        .store
        .cluster_service_admit(
            "cleanup-stop",
            Some(s.id),
            ClusterServiceAction::Stop,
            Some(1),
            None,
            now_ms(),
        )
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        reconcile_service(world.controller.clone(), s.id, true)
            .await
            .unwrap();
        if host
            .state
            .store
            .service_deployment(s.id)
            .unwrap()
            .unwrap()
            .phase
            == ServicePhase::Failed
        {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let failed = host.state.store.service_deployment(s.id).unwrap().unwrap();
    assert_eq!(failed.instances[0].vm_id, vm.id);
    assert_eq!(
        world.controller.store.cluster_service(s.id).unwrap().hosts[0]
            .reservation
            .cpu,
        1
    );
    vm.state = VmState::Stopped;
    host.state.store.update(&vm).unwrap();
    host.state.vms.lock().unwrap().insert(vm.id, vm.clone());
    loop {
        reconcile_service(world.controller.clone(), s.id, true)
            .await
            .unwrap();
        if world
            .controller
            .store
            .cluster_service(s.id)
            .unwrap()
            .observed
            == ClusterServiceObserved::Stopped
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Stop did not retry cleanup"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let remaining = host.state.store.load_all().unwrap();
    assert!(!remaining.contains_key(&vm.id));
    assert!(remaining.contains_key(&foreign.id));
    assert_eq!(
        world.controller.store.cluster_service(s.id).unwrap().hosts[0]
            .reservation
            .cpu,
        0
    );
}

#[test]
fn stopped_service_cannot_reuse_released_capacity_after_another_service_reserves_it() {
    let (_root, store, _) = world(1);
    let s = create(&store, service_spec(&[report(1)]));
    let stop = store
        .cluster_service_admit(
            "stop",
            Some(s.id),
            ClusterServiceAction::Stop,
            Some(1),
            None,
            1002,
        )
        .unwrap();
    let (token, mut done) = store
        .cluster_service_claim(s.id, 1003, true)
        .unwrap()
        .unwrap();
    done.hosts[0].reservation = HostCapacity::default();
    done.hosts[0].observed_generation = Some(stop.generation);
    done.pending = false;
    done.observed = ClusterServiceObserved::Stopped;
    store.cluster_service_finish(&done, token, 1004).unwrap();
    let mut other = service_spec(&[report(1)]);
    other.service.name = "other".into();
    other.service.cpu = 2;
    store
        .cluster_service_admit(
            "other",
            None,
            ClusterServiceAction::Create,
            None,
            Some(other),
            1005,
        )
        .unwrap();
    assert!(matches!(
        store.cluster_service_admit(
            "restart",
            Some(s.id),
            ClusterServiceAction::Update,
            Some(2),
            Some(s.spec),
            1006
        ),
        Err(ClusterError::Capacity)
    ));
    assert_eq!(
        store.cluster_service(s.id).unwrap().observed,
        ClusterServiceObserved::Stopped
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_reservations_are_not_double_counted_but_local_admission_counts_existing_replicas()
{
    let world = World::new(1).await;
    let host = &world.hosts[0];
    let s = world
        .controller
        .store
        .cluster_service_admit(
            "capacity",
            None,
            ClusterServiceAction::Create,
            None,
            Some(service_spec(std::slice::from_ref(&host.report))),
            now_ms(),
        )
        .unwrap();
    reconcile_service(world.controller.clone(), s.id, true)
        .await
        .unwrap();
    host.state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id: s.id,
            spec: s.hosts[0].intent.spec.clone(),
        })
        .unwrap();
    let mut d = stopped(s.id, &s.hosts[0].intent);
    let mut vm = crate::handlers::vms::test_support::record("managed", Uuid::new_v4());
    vm.cpu = 1;
    vm.ram = 512;
    vm.disk_gb = 1;
    host.state.store.insert(&vm).unwrap();
    host.state.vms.lock().unwrap().insert(vm.id, vm.clone());
    d.phase = ServicePhase::Starting;
    d.instances.push(ServiceInstance {
        vm_id: vm.id,
        ipv4: String::new(),
        host_port: 30100,
        state: VmState::Created,
        healthy: false,
    });
    host.state.store.save_service_deployment(&d).unwrap();
    let inventory = inventory(&host.state).await.unwrap();
    assert_eq!(inventory.unmanaged.cpu, 0);
    assert_eq!(inventory.unmanaged.ram_mib, 0);
    assert_eq!(inventory.storages[0].unmanaged_gib, 1);
    world
        .controller
        .store
        .cluster_register(&inventory, now_ms())
        .unwrap();
    assert_eq!(
        world.controller.store.cluster_hosts(now_ms()).unwrap()[0]
            .reserved
            .cpu,
        1
    );
    assert!(
        super::super::worker::check_capacity(&host.state, Uuid::new_v4(), 1, 512, 1, "default")
            .await
            .is_ok()
    );
    assert!(matches!(
        super::super::worker::check_capacity(&host.state, Uuid::new_v4(), 2, 512, 1, "default")
            .await,
        Err(ClusterError::Capacity)
    ));
    let response = world
        .controller
        .cluster
        .client(host.report.id)
        .unwrap()
        .delete(format!("{}/api/vms/{}", host.report.endpoint, vm.id))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    assert!(host.state.store.load_all().unwrap().contains_key(&vm.id));
}

#[test]
fn restart_expires_dispatch_claim_and_fences_the_lost_response() {
    let (root, store, _) = world(1);
    let service = create(&store, service_spec(&[report(1)]));
    let (old, stale) = store
        .cluster_service_claim(service.id, 1002, true)
        .unwrap()
        .unwrap();
    let reopened = Store::open(&root.path().join("state.db")).unwrap();
    assert!(
        reopened
            .cluster_service_claim(service.id, 1003, true)
            .unwrap()
            .is_none()
    );
    assert!(reopened.cluster_service_due(31001, 8).unwrap().is_empty());
    assert_eq!(
        reopened.cluster_service_due(31002, 8).unwrap(),
        vec![service.id]
    );
    let (current, mut recovered) = reopened
        .cluster_service_claim(service.id, 31002, false)
        .unwrap()
        .unwrap();
    assert_eq!(recovered, service);
    recovered.degraded = true;
    reopened
        .cluster_service_finish(&recovered, current, 31003)
        .unwrap();
    store.cluster_service_finish(&stale, old, 31004).unwrap();
    assert_eq!(store.cluster_service(service.id).unwrap(), recovered);
    assert_eq!(store.cluster_hosts(31004).unwrap()[0].reserved.cpu, 1);
}

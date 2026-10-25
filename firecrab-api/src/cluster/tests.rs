use super::*;
use crate::persistence::Store;

fn report(n: u128) -> HostReport {
    HostReport {
        id: Uuid::from_u128(n),
        name: format!("host-{n}"),
        endpoint: format!("https://127.0.0.1:{}", 10000 + n),
        architecture: "aarch64".into(),
        capacity: HostCapacity {
            cpu: 2,
            ram_mib: 1024,
        },
        unmanaged: HostCapacity::default(),
        images: vec![HostImage {
            alias: "fixture".into(),
            version: "v1".into(),
            kernel_sha256: "a".repeat(64),
            initrd_sha256: None,
            rootfs_sha256: "b".repeat(64),
            boot_args_sha256: "c".repeat(64),
        }],
        networks: vec![HostNetwork {
            id: Uuid::from_u128(100 + n),
            name: "private".into(),
        }],
        storages: vec![HostStorage {
            id: "default".into(),
            capacity_gib: 4,
            unmanaged_gib: 0,
        }],
        labels: BTreeMap::from([("zone".into(), "test".into())]),
    }
}
fn target(host: &HostReport) -> PlacementTarget {
    PlacementTarget {
        network: HostResourceRef {
            host_id: host.id,
            resource_id: host.networks[0].id.to_string(),
        },
        storage: HostResourceRef {
            host_id: host.id,
            resource_id: host.storages[0].id.clone(),
        },
    }
}
fn spec(hosts: &[HostReport]) -> PlacementSpec {
    PlacementSpec {
        name: "placed-vm".into(),
        image: hosts[0].images[0].clone(),
        cpu: 1,
        ram: 512,
        disk_gb: 2,
        egress_policy: EgressPolicy::Isolated,
        env: BTreeMap::new(),
        required_labels: BTreeMap::new(),
        targets: hosts.iter().map(target).collect(),
    }
}
fn world(count: u128) -> (tempfile::TempDir, Store, PlacementSpec) {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("state.db")).unwrap();
    let hosts: Vec<_> = (1..=count).map(report).collect();
    for host in &hosts {
        store.cluster_register(host, 1000).unwrap();
    }
    (root, store, spec(&hosts))
}
fn deleted(store: &Store, id: Uuid) {
    store.cluster_action(id, PlacementAction::Delete).unwrap();
    let (claim, mut p) = store.cluster_claim(id, 1100, true).unwrap().unwrap();
    p.phase = PlacementPhase::Deleted;
    store.cluster_finish(&p, claim, 1101).unwrap();
}

#[tokio::test]
async fn local_storage_reassignment_cannot_consume_controller_disk_reservations() {
    let root = tempfile::tempdir().unwrap();
    let mut state = state(root.path()).await;
    let mut report = report(1);
    report.storages[0].capacity_gib = 2;
    report.storages.push(HostStorage {
        id: "other".into(),
        capacity_gib: 2,
        unmanaged_gib: 0,
    });
    let other = root.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    state.storage = Arc::new(
        crate::storage::StorageRegistry::parse(&format!(
            "default={}:other={}",
            root.path().join("storage").display(),
            other.display(),
        ))
        .unwrap(),
    );
    state.cluster = Arc::new(Runtime {
        identity: Some(report.id),
        worker: Some(WorkerConfig {
            controller_id: Uuid::new_v4(),
            controller_url: "https://controller.test".into(),
            endpoint: report.endpoint.clone(),
            name: report.name.clone(),
            labels: BTreeMap::new(),
            disk_limits: BTreeMap::new(),
        }),
        report_override: Some(report.clone()),
        ..Runtime::default()
    });
    let spec = spec(&[report]);
    state
        .store
        .cluster_worker_admit(
            Uuid::new_v4(),
            Uuid::new_v4(),
            WorkerIntent {
                generation: 0,
                action: PlacementAction::Create,
                target: spec.targets[0].clone(),
                spec,
            },
        )
        .unwrap();
    let id = Uuid::new_v4();
    let mut vm = crate::handlers::vms::test_support::record("unmanaged", id);
    vm.disk_gb = 2;
    vm.storage_root = "other".into();
    state.store.insert(&vm).unwrap();
    state.vms.lock().unwrap().insert(id, vm);
    let error = crate::handlers::vms::assign_vm_storage(
        axum::extract::State(state.clone()),
        axum::Extension(crate::server::RequestId(Uuid::new_v4())),
        axum::extract::Path(id.to_string()),
        crate::extract::ValidatedJson(AssignVmStorageRequest {
            storage_root: "default".into(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "insufficient_capacity");
    assert_eq!(state.vms.lock().unwrap()[&id].storage_root, "other");
}

#[test]
fn placement_scores_three_hosts_and_retries_keep_the_exact_owner() {
    let (_root, store, spec) = world(3);
    let placements: Vec<_> = (0..3)
        .map(|i| {
            store
                .cluster_admit(&format!("key-{i}"), spec.clone(), 1001)
                .unwrap()
        })
        .collect();
    assert_eq!(
        placements.iter().map(|p| p.vm.host_id).collect::<Vec<_>>(),
        (1..=3).map(Uuid::from_u128).collect::<Vec<_>>()
    );
    let mut reordered = spec.clone();
    reordered.targets.reverse();
    assert_eq!(
        store.cluster_admit("key-0", reordered, 100_000).unwrap(),
        placements[0]
    );
    let mut changed = spec;
    changed.ram = 1024;
    assert!(matches!(
        store.cluster_admit("key-0", changed, 1001),
        Err(ClusterError::Conflict(_))
    ));
    assert_eq!(store.cluster_placements().unwrap().len(), 3);
}

#[test]
fn independent_sqlite_connections_reserve_capacity_atomically() {
    let (root, store, spec) = world(3);
    let barrier = Arc::new(std::sync::Barrier::new(12));
    let connections: Vec<_> = (0..12)
        .map(|_| Store::open(&root.path().join("state.db")).unwrap())
        .collect();
    let threads: Vec<_> = connections
        .into_iter()
        .enumerate()
        .map(|(i, store)| {
            let barrier = barrier.clone();
            let spec = spec.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.cluster_admit(&format!("concurrent-{i}"), spec, 1001)
            })
        })
        .collect();
    let accepted: Vec<_> = threads
        .into_iter()
        .map(|t| t.join().unwrap())
        .filter_map(Result::ok)
        .collect();
    assert_eq!(accepted.len(), 6);
    for host in store.cluster_hosts(1002).unwrap() {
        assert_eq!(
            host.reserved,
            HostCapacity {
                cpu: 2,
                ram_mib: 1024
            }
        );
        assert_eq!(host.reserved_disk_gib["default"], 4);
    }
    deleted(&store, accepted[0].id);
    assert!(store.cluster_admit("after-cleanup", spec, 1102).is_ok());
}

#[test]
fn concurrent_identical_retries_share_one_journal_and_reservation() {
    let (root, store, spec) = world(2);
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let connections: Vec<_> = (0..8)
        .map(|_| Store::open(&root.path().join("state.db")).unwrap())
        .collect();
    let threads: Vec<_> = connections
        .into_iter()
        .map(|store| {
            let spec = spec.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.cluster_admit("same", spec, 1001).unwrap().id
            })
        })
        .collect();
    let ids: BTreeSet<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(ids.len(), 1);
    assert_eq!(store.cluster_placements().unwrap().len(), 1);
    assert_eq!(
        store
            .cluster_hosts(1002)
            .unwrap()
            .iter()
            .map(|h| h.reserved.cpu)
            .sum::<u32>(),
        1
    );
}

#[test]
fn drain_is_sticky_and_missing_heartbeats_never_release_uncertain_ownership() {
    let (root, store, spec) = world(2);
    let p = store.cluster_admit("first", spec.clone(), 1001).unwrap();
    store.cluster_claim(p.id, 1002, false).unwrap().unwrap();
    store.cluster_drain(p.vm.host_id, true).unwrap();
    store.cluster_register(&report(1), 1010).unwrap();
    let next = store.cluster_admit("second", spec.clone(), 1011).unwrap();
    assert_ne!(p.vm.host_id, next.vm.host_id);
    drop(store);
    let reopened = Store::open(&root.path().join("state.db")).unwrap();
    let saved = reopened.cluster_placement(p.id).unwrap();
    assert_eq!(saved.phase, PlacementPhase::Uncertain);
    assert_eq!(saved.vm, p.vm);
    let hosts = reopened.cluster_hosts(40_000).unwrap();
    assert!(hosts.iter().all(|h| !h.online));
    assert!(hosts[0].draining);
    assert_eq!(hosts[0].reserved.cpu, 1);
    assert!(matches!(
        reopened.cluster_admit("offline", spec, 40_000),
        Err(ClusterError::Capacity)
    ));
}

#[test]
fn foreign_resources_labels_and_image_pins_are_enforced_before_dispatch() {
    let (_root, store, spec) = world(2);
    let mut bad = spec.clone();
    bad.targets[0].storage.host_id = Uuid::from_u128(2);
    assert!(matches!(
        store.cluster_admit("foreign-store", bad, 1001),
        Err(ClusterError::Invalid(_))
    ));
    let mut bad = spec.clone();
    bad.targets[0].network.resource_id = report(2).networks[0].id.to_string();
    assert!(matches!(
        store.cluster_admit("foreign-net", bad, 1001),
        Err(ClusterError::Invalid(_))
    ));
    let mut bad = spec.clone();
    bad.required_labels
        .insert("zone".into(), "elsewhere".into());
    assert!(matches!(
        store.cluster_admit("labels", bad, 1001),
        Err(ClusterError::Capacity)
    ));
    let mut bad = spec;
    bad.image.rootfs_sha256 = "d".repeat(64);
    assert!(matches!(
        store.cluster_admit("pin", bad, 1001),
        Err(ClusterError::Capacity)
    ));
    assert!(store.cluster_placements().unwrap().is_empty());
}

#[test]
fn stale_controller_responses_cannot_overwrite_a_new_delete_generation() {
    let (_root, store, spec) = world(2);
    let p = store.cluster_admit("first", spec, 1001).unwrap();
    let (old_claim, mut old) = store.cluster_claim(p.id, 1002, false).unwrap().unwrap();
    deleted(&store, p.id);
    old.phase = PlacementPhase::Confirmed;
    store.cluster_finish(&old, old_claim, 1200).unwrap();
    assert_eq!(
        store.cluster_placement(p.id).unwrap().phase,
        PlacementPhase::Deleted
    );
    assert!(
        store
            .cluster_hosts(1200)
            .unwrap()
            .iter()
            .all(|h| h.reserved.cpu == 0)
    );
}

#[test]
fn worker_tombstones_reject_late_creates_foreign_controllers_and_reused_payloads() {
    let (_root, store, spec) = world(2);
    let id = Uuid::new_v4();
    let owner = Uuid::from_u128(500);
    let intent = WorkerIntent {
        generation: 1,
        action: PlacementAction::Delete,
        target: spec.targets[0].clone(),
        spec,
    };
    let mut receipt = store
        .cluster_worker_admit(id, owner, intent.clone())
        .unwrap();
    receipt.phase = PlacementPhase::Deleted;
    store.cluster_worker_finish(&receipt).unwrap();
    assert_eq!(
        store
            .cluster_worker_admit(id, owner, intent.clone())
            .unwrap(),
        receipt
    );
    let mut late = intent.clone();
    late.action = PlacementAction::Create;
    late.generation = 0;
    assert!(matches!(
        store.cluster_worker_admit(id, owner, late),
        Err(ClusterError::Conflict(_))
    ));
    assert!(matches!(
        store.cluster_worker_admit(id, Uuid::new_v4(), intent.clone()),
        Err(ClusterError::Conflict(_))
    ));
    let mut reused = intent;
    reused.spec.name = "other".into();
    assert!(matches!(
        store.cluster_worker_admit(id, owner, reused),
        Err(ClusterError::Conflict(_))
    ));
}

#[test]
fn stale_worker_acknowledgement_cannot_overwrite_a_delete_generation() {
    let (_root, store, spec) = world(1);
    let id = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let intent = WorkerIntent {
        generation: 1,
        action: PlacementAction::Create,
        target: spec.targets[0].clone(),
        spec,
    };
    let mut stale = store
        .cluster_worker_admit(id, owner, intent.clone())
        .unwrap();
    let mut delete = intent;
    delete.generation = 2;
    delete.action = PlacementAction::Delete;
    let mut receipt = store.cluster_worker_admit(id, owner, delete).unwrap();
    receipt.phase = PlacementPhase::Deleted;
    store.cluster_worker_finish(&receipt).unwrap();
    stale.phase = PlacementPhase::Confirmed;
    assert!(matches!(
        store.cluster_worker_finish(&stale),
        Err(ClusterError::Conflict(_))
    ));
    assert_eq!(store.cluster_worker(id).unwrap(), Some(receipt));
}

#[test]
fn disk_quota_is_stable_across_vm_writes_and_controller_restarts() {
    let (root, store, _spec) = world(1);
    assert_eq!(store.cluster_disk_budget("default", 40).unwrap(), 40);
    assert_eq!(store.cluster_disk_budget("default", 35).unwrap(), 40);
    store.cluster_set_disk_budget("default", 64).unwrap();
    drop(store);
    let store = Store::open(&root.path().join("state.db")).unwrap();
    assert_eq!(store.cluster_disk_budget("default", 30).unwrap(), 64);
}

#[test]
fn due_reconciliation_does_not_starve_placements_beyond_the_concurrency_limit() {
    let (_root, store, spec) = world(1);
    let mut report = report(1);
    report.capacity = HostCapacity {
        cpu: 16,
        ram_mib: 8192,
    };
    report.storages[0].capacity_gib = 64;
    store.cluster_register(&report, 1000).unwrap();
    for i in 0..12 {
        store
            .cluster_admit(&format!("queued-{i}"), spec.clone(), 1001)
            .unwrap();
    }
    let first = store.cluster_due(1002, 8).unwrap();
    assert_eq!(first.len(), 8);
    for id in &first {
        store.cluster_claim(*id, 1002, false).unwrap().unwrap();
    }
    let second = store.cluster_due(1003, 8).unwrap();
    assert_eq!(second.len(), 4);
    assert!(second.iter().all(|id| !first.contains(id)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_vm_resource_growth_cannot_consume_controller_reservations() {
    let world = World::new(1).await;
    let client = world.client();
    let spec = world.spec();
    let p: VmPlacement = client
        .post(format!("{}/api/cluster/placements", world.url))
        .header("Idempotency-Key", "reserved")
        .json(&spec)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    world.settled(p.id, PlacementPhase::Confirmed).await;
    let host = &world.hosts[0];
    let local = host.state.cluster.client(host.report.id).unwrap();
    let request = CreateVmRequest {
        name: "unmanaged".into(),
        template: spec.image.alias,
        cpu: 1,
        ram: 512,
        disk_gb: 2,
        egress_policy: EgressPolicy::Isolated,
        micro_network_id: host.report.networks[0].id,
        storage_root: Some("default".into()),
        security_group_id: None,
        shell_ids: vec![],
        port_forwards: vec![],
        env: BTreeMap::new(),
    };
    let response = local
        .post(format!("{}/api/vms", host.report.endpoint))
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let vm: VmResponse = response.json().await.unwrap();
    let update = UpdateVmResourcesRequest {
        cpu: 2,
        ram: 512,
        disk_gb: 2,
        egress_policy: EgressPolicy::Isolated,
        env: None,
        security_group_id: None,
    };
    let path = format!("{}/api/vms/{}", host.report.endpoint, vm.id);
    assert_eq!(
        local
            .put(&path)
            .json(&update)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(host.state.vms.lock().unwrap()[&vm.id].cpu, 1);
    client
        .post(format!(
            "{}/api/cluster/placements/{}/delete",
            world.url, p.id
        ))
        .send()
        .await
        .unwrap();
    world.settled(p.id, PlacementPhase::Deleted).await;
    assert_eq!(
        local
            .put(&path)
            .json(&update)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(host.state.vms.lock().unwrap()[&vm.id].cpu, 2);
}

#[test]
fn a_worker_cannot_adopt_or_delete_a_preexisting_unmanaged_vm() {
    let (_root, store, spec) = world(1);
    let id = Uuid::new_v4();
    let vm = crate::handlers::vms::test_support::record("unmanaged", id);
    store.insert(&vm).unwrap();
    let intent = WorkerIntent {
        generation: 1,
        action: PlacementAction::Delete,
        target: spec.targets[0].clone(),
        spec,
    };
    assert!(matches!(
        store.cluster_worker_admit(id, Uuid::from_u128(500), intent),
        Err(ClusterError::Conflict(_))
    ));
    assert_eq!(store.load_all().unwrap()[&id].name, "unmanaged");
    assert!(store.cluster_worker(id).unwrap().is_none());
}

#[tokio::test]
async fn enrollment_rejects_plaintext_and_spoofed_http_identity() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let app = router(state);
    let report = report(1);
    let response = app
        .oneshot(
            Request::post("/api/cluster/hosts/register")
                .header("Content-Type", "application/json")
                .header("X-Firecrab-Host-Id", report.id.to_string())
                .body(Body::from(serde_json::to_vec(&report).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
}

async fn state(root: &std::path::Path) -> AppState {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("kernel"), b"kernel").unwrap();
    std::fs::write(root.join("rootfs"), b"rootfs").unwrap();
    let templates = crate::templates::TemplateRegistry::from_specs(
        root,
        [crate::templates::TemplateSpec {
            alias: "fixture".into(),
            version: "v1".into(),
            kernel: "kernel".into(),
            initrd: None,
            rootfs: "rootfs".into(),
            boot_args: "console=ttyS0".into(),
        }],
    )
    .unwrap();
    let mut state = AppState::with_db_file(templates, root.join("state.db"))
        .await
        .unwrap();
    state.storage = Arc::new(crate::storage::StorageRegistry::single(
        "default",
        root.join("storage"),
    ));
    let socket = root.join("helper.sock");
    // Reopened controller fixtures never use network mutations.
    if !socket.exists() {
        let _helper = crate::network::test_support::spawn_always_ok_helper(&socket);
    }
    state.network = crate::network::NetworkClient::with_socket_path(socket);
    state
}

fn router(state: AppState) -> axum::Router {
    let mut config = crate::server::HttpConfig::load().unwrap();
    config.static_root = None;
    crate::server::build_router(state, &config)
}

async fn serve(state: AppState, paths: TlsPaths) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let tls = crate::tls::configuration(address, Some(paths)).unwrap();
    let task = tokio::spawn(async move {
        crate::tls::serve(listener, router(state), tls)
            .await
            .unwrap();
    });
    (format!("https://{address}"), task)
}

struct Host {
    state: AppState,
    report: HostReport,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Host {
    fn drop(&mut self) {
        self.task.abort();
    }
}
struct World {
    root: tempfile::TempDir,
    controller: AppState,
    paths: TlsPaths,
    url: String,
    task: tokio::task::JoinHandle<()>,
    hosts: Vec<Host>,
}
impl Drop for World {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl World {
    async fn new(count: u128) -> Self {
        let root = tempfile::Builder::new()
            .prefix("fc455-")
            .tempdir_in("/tmp")
            .unwrap();
        let ca = root.path().join("ca");
        firecrab_pki::init_ca(&ca).unwrap();
        let controller_id = Uuid::from_u128(500);
        let paths =
            firecrab_pki::issue_host_cert(&ca, controller_id, vec!["127.0.0.1".into()], 1).unwrap();
        let mut controller = state(&root.path().join("controller")).await;
        controller.cluster = Arc::new(Runtime {
            identity: Some(controller_id),
            paths: Some(paths.clone()),
            ..Runtime::default()
        });
        let (url, task) = serve(controller.clone(), paths.clone()).await;
        let mut hosts = Vec::new();
        for n in 1..=count {
            let mut state = state(&root.path().join(format!("worker-{n}"))).await;
            let mut report = report(n);
            report.images = state
                .templates
                .list_aliases()
                .iter()
                .map(|v| image(v))
                .collect();
            state
                .store
                .insert_micro_network(&MicroNetworkResponse {
                    id: report.networks[0].id,
                    name: "private".into(),
                    subnet_cidr: "172.30.0.0/24".into(),
                    gateway: "172.30.0.1".into(),
                    internet_enabled: false,
                    uplink: None,
                    ipv6_cidr: None,
                    ipv6_gateway: None,
                    ipv6_address_mode: None,
                    ipv6_egress: None,
                })
                .unwrap();
            let cert =
                firecrab_pki::issue_host_cert(&ca, report.id, vec!["127.0.0.1".into()], 1).unwrap();
            state.cluster = Arc::new(Runtime {
                identity: Some(report.id),
                paths: Some(cert.clone()),
                worker: Some(WorkerConfig {
                    controller_id,
                    controller_url: url.clone(),
                    endpoint: report.endpoint.clone(),
                    name: report.name.clone(),
                    labels: report.labels.clone(),
                    disk_limits: BTreeMap::new(),
                }),
                report_override: Some(report.clone()),
                ..Runtime::default()
            });
            let (endpoint, task) = serve(state.clone(), cert).await;
            report.endpoint = endpoint;
            // The fixture's capacity is intentionally small and independent of CI hardware.
            state.cluster.worker.as_ref().unwrap();
            let client = state.cluster.client(controller_id).unwrap();
            assert!(
                client
                    .post(format!("{url}/api/cluster/hosts/register"))
                    .json(&report)
                    .send()
                    .await
                    .unwrap()
                    .status()
                    .is_success()
            );
            hosts.push(Host {
                state,
                report,
                task,
            });
        }
        Self {
            root,
            controller,
            paths,
            url,
            task,
            hosts,
        }
    }

    fn spec(&self) -> PlacementSpec {
        spec(
            &self
                .hosts
                .iter()
                .map(|h| h.report.clone())
                .collect::<Vec<_>>(),
        )
    }
    fn client(&self) -> reqwest::Client {
        self.controller
            .cluster
            .client(Uuid::from_u128(500))
            .unwrap()
    }
    async fn settled(&self, id: Uuid, phase: PlacementPhase) -> VmPlacement {
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        loop {
            let p = self.controller.store.cluster_placement(id).unwrap();
            if p.phase == phase {
                return p;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "placement failed to settle: {p:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_and_three_tls_hosts_place_retry_drain_and_delete_through_the_recorded_owner() {
    for count in [2, 3] {
        let world = World::new(count).await;
        let client = world.client();
        let spec = world.spec();
        let mut placements = Vec::new();
        for n in 0..count {
            let response = client
                .post(format!("{}/api/cluster/placements", world.url))
                .header("Idempotency-Key", format!("vm-{n}"))
                .json(&spec)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 202);
            let p: VmPlacement = response.json().await.unwrap();
            placements.push(world.settled(p.id, PlacementPhase::Confirmed).await);
        }
        assert_eq!(
            placements
                .iter()
                .map(|p| p.vm.host_id)
                .collect::<BTreeSet<_>>()
                .len(),
            count as usize
        );
        let original = &placements[0];
        let replay = client
            .post(format!("{}/api/cluster/placements", world.url))
            .header("Idempotency-Key", "vm-0")
            .json(&spec)
            .send()
            .await
            .unwrap()
            .json::<VmPlacement>()
            .await
            .unwrap();
        assert_eq!(replay.id, original.id);
        assert_eq!(replay.vm, original.vm);
        assert_eq!(
            world
                .hosts
                .iter()
                .map(|h| h.state.vms.lock().unwrap().len())
                .sum::<usize>(),
            count as usize
        );
        let foreign = world.hosts[0]
            .state
            .cluster
            .client(Uuid::from_u128(500))
            .unwrap()
            .post(format!("{}/api/cluster/hosts/register", world.url))
            .header("X-Firecrab-Host-Id", world.hosts[1].report.id.to_string())
            .json(&world.hosts[1].report)
            .send()
            .await
            .unwrap();
        assert_eq!(foreign.status(), 403);
        assert_eq!(
            client
                .patch(format!(
                    "{}/api/cluster/hosts/{}",
                    world.url, original.vm.host_id
                ))
                .json(&DrainHostRequest { draining: true })
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        let owner = world
            .hosts
            .iter()
            .find(|h| h.report.id == original.vm.host_id)
            .unwrap();
        let direct = owner
            .state
            .cluster
            .client(owner.report.id)
            .unwrap()
            .delete(format!("{}/api/vms/{}", owner.report.endpoint, original.id))
            .send()
            .await
            .unwrap();
        assert_eq!(direct.status(), 409);
        let stack = owner.state.store.register_stack("foreign-stack").unwrap();
        assert!(matches!(
            owner.state.store.bind_stack_resource(
                &stack.name,
                &firecrab_stack::Binding {
                    kind: firecrab_stack::ResourceKind::Vm,
                    key: "adopted".into(),
                    resource_id: original.id,
                }
            ),
            Err(crate::persistence::PersistenceError::InvalidStackResource)
        ));
        let schedule = ScheduleSpec {
            name: "bypass".into(),
            vm_id: original.id,
            action: ScheduleAction::Start,
            timing: ScheduleTiming::Cron {
                expression: "0 * * * *".into(),
                time_zone: "UTC".into(),
            },
            enabled: true,
        };
        let response = owner
            .state
            .cluster
            .client(owner.report.id)
            .unwrap()
            .post(format!("{}/api/schedules", owner.report.endpoint))
            .json(&schedule)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        assert_eq!(
            client
                .post(format!(
                    "{}/api/cluster/placements/{}/stop",
                    world.url, original.id
                ))
                .send()
                .await
                .unwrap()
                .status(),
            202
        );
        let stopped = world.settled(original.id, PlacementPhase::Confirmed).await;
        assert_eq!(stopped.vm, original.vm);
        assert_eq!(
            client
                .post(format!(
                    "{}/api/cluster/placements/{}/delete",
                    world.url, original.id
                ))
                .send()
                .await
                .unwrap()
                .status(),
            202
        );
        world.settled(original.id, PlacementPhase::Deleted).await;
        assert!(!owner.state.vms.lock().unwrap().contains_key(&original.id));
        assert!(
            owner
                .state
                .store
                .active_lease(original.id)
                .unwrap()
                .is_none()
        );
        let late = WorkerIntent {
            generation: 0,
            action: PlacementAction::Create,
            spec: spec.clone(),
            target: original.target.clone(),
        };
        let response = world
            .controller
            .cluster
            .client(owner.report.id)
            .unwrap()
            .put(format!(
                "{}/api/cluster/worker/placements/{}",
                owner.report.endpoint, original.id
            ))
            .json(&late)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 409);
        assert_eq!(
            world
                .hosts
                .iter()
                .map(|h| h.state.vms.lock().unwrap().len())
                .sum::<usize>(),
            count as usize - 1
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_recovery_releases_a_lease_after_the_vm_row_is_already_gone() {
    let world = World::new(1).await;
    let p = world
        .controller
        .store
        .cluster_admit("interrupted-delete", world.spec(), now_ms())
        .unwrap();
    reconcile_one(world.controller.clone(), p.id, true)
        .await
        .unwrap();
    world.settled(p.id, PlacementPhase::Confirmed).await;
    let owner = &world.hosts[0];
    assert!(
        owner
            .state
            .store
            .active_leases()
            .unwrap()
            .iter()
            .any(|lease| lease.vm_id == p.id)
    );
    let paths =
        crate::artifacts::VmArtifactPaths::for_vm(&owner.state.vms_dir_for("default"), p.id);
    assert!(paths.dir.exists());
    let delete = world
        .controller
        .store
        .cluster_action(p.id, PlacementAction::Delete)
        .unwrap();
    owner
        .state
        .store
        .cluster_worker_admit(
            p.id,
            Uuid::from_u128(500),
            WorkerIntent {
                generation: delete.generation,
                action: delete.action,
                target: delete.target,
                spec: delete.spec,
            },
        )
        .unwrap();
    owner.state.store.delete(p.id).unwrap();
    owner.state.vms.lock().unwrap().remove(&p.id);
    reconcile_one(world.controller.clone(), p.id, true)
        .await
        .unwrap();
    world.settled(p.id, PlacementPhase::Deleted).await;
    assert!(!paths.dir.exists());
    assert!(
        !owner
            .state
            .store
            .active_leases()
            .unwrap()
            .iter()
            .any(|lease| lease.vm_id == p.id)
    );
    assert_eq!(
        world.controller.store.cluster_hosts(now_ms()).unwrap()[0]
            .reserved
            .cpu,
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_create_response_survives_controller_restart_without_recreating_the_vm() {
    let mut world = World::new(2).await;
    let spec = world.spec();
    let p = world
        .controller
        .store
        .cluster_admit("lost-response", spec, now_ms())
        .unwrap();
    world
        .controller
        .store
        .cluster_claim(p.id, now_ms() - 31_000, true)
        .unwrap()
        .unwrap();
    let owner = world
        .hosts
        .iter()
        .find(|h| h.report.id == p.vm.host_id)
        .unwrap();
    let intent = WorkerIntent {
        generation: p.generation,
        action: p.action,
        spec: p.spec.clone(),
        target: p.target.clone(),
    };
    let response = world
        .controller
        .cluster
        .client(owner.report.id)
        .unwrap()
        .put(format!(
            "{}/api/cluster/worker/placements/{}",
            owner.report.endpoint, p.id
        ))
        .json(&intent)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    drop(response);
    world.task.abort();
    let mut reopened = state(&world.root.path().join("controller")).await;
    reopened.cluster = Arc::new(Runtime {
        identity: Some(Uuid::from_u128(500)),
        paths: Some(world.paths.clone()),
        ..Runtime::default()
    });
    reconcile_one(reopened.clone(), p.id, true).await.unwrap();
    let restored = reopened.store.cluster_placement(p.id).unwrap();
    assert_eq!(restored.phase, PlacementPhase::Confirmed);
    assert_eq!(restored.vm, p.vm);
    assert_eq!(owner.state.vms.lock().unwrap().len(), 1);
    assert_eq!(
        reopened
            .store
            .cluster_hosts(now_ms())
            .unwrap()
            .iter()
            .map(|h| h.reserved.cpu)
            .sum::<u32>(),
        1
    );
    world.controller = reopened;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_different_enrolled_tls_host_cannot_impersonate_the_recorded_owner() {
    let world = World::new(2).await;
    let mut spec = world.spec();
    spec.targets.truncate(1);
    let p = world
        .controller
        .store
        .cluster_admit("wrong-peer", spec, now_ms())
        .unwrap();
    let mut report = world.hosts[0].report.clone();
    report.endpoint = world.hosts[1].report.endpoint.clone();
    world
        .controller
        .store
        .cluster_register(&report, now_ms())
        .unwrap();
    reconcile_one(world.controller.clone(), p.id, true)
        .await
        .unwrap();
    let uncertain = world.controller.store.cluster_placement(p.id).unwrap();
    assert_eq!(uncertain.phase, PlacementPhase::Uncertain);
    assert_eq!(uncertain.vm, p.vm);
    assert!(
        world
            .hosts
            .iter()
            .all(|h| h.state.vms.lock().unwrap().is_empty())
    );
    assert_eq!(
        world.controller.store.cluster_hosts(now_ms()).unwrap()[0]
            .reserved
            .cpu,
        1
    );
    world
        .controller
        .store
        .cluster_register(&world.hosts[0].report, now_ms())
        .unwrap();
    reconcile_one(world.controller.clone(), p.id, true)
        .await
        .unwrap();
    assert_eq!(
        world
            .controller
            .store
            .cluster_placement(p.id)
            .unwrap()
            .phase,
        PlacementPhase::Confirmed
    );
    assert_eq!(world.hosts[0].state.vms.lock().unwrap().len(), 1);
    assert!(world.hosts[1].state.vms.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_restart_finishes_an_interrupted_create_without_inserting_another_vm() {
    let mut world = World::new(2).await;
    let p = world
        .controller
        .store
        .cluster_admit("worker-restart", world.spec(), now_ms())
        .unwrap();
    let index = world
        .hosts
        .iter()
        .position(|h| h.report.id == p.vm.host_id)
        .unwrap();
    let owner = &world.hosts[index];
    let intent = WorkerIntent {
        generation: 0,
        action: PlacementAction::Create,
        target: p.target.clone(),
        spec: p.spec.clone(),
    };
    owner
        .state
        .store
        .cluster_worker_admit(p.id, Uuid::from_u128(500), intent)
        .unwrap();
    let mut vm = crate::handlers::vms::test_support::record(&p.spec.name, p.id);
    vm.state = VmState::Created;
    vm.template = p.spec.image.alias.clone();
    vm.template_version = p.spec.image.version.clone();
    vm.template_kernel_sha256 = p.spec.image.kernel_sha256.clone();
    vm.template_rootfs_sha256 = p.spec.image.rootfs_sha256.clone();
    vm.template_boot_args_sha256 = p.spec.image.boot_args_sha256.clone();
    vm.cpu = p.spec.cpu;
    vm.ram = p.spec.ram;
    vm.disk_gb = p.spec.disk_gb;
    vm.micro_network_id = Uuid::parse_str(&p.target.network.resource_id).unwrap();
    vm.storage_root = p.target.storage.resource_id.clone();
    vm.egress_policy = p.spec.egress_policy;
    vm.env = p.spec.env.clone();
    owner.state.store.insert(&vm).unwrap();
    owner.task.abort();
    let runtime = Runtime {
        identity: Some(owner.report.id),
        paths: owner.state.cluster.paths.clone(),
        worker: owner.state.cluster.worker.clone(),
        report_override: Some(owner.report.clone()),
        ..Runtime::default()
    };
    let cert = runtime.paths.clone().unwrap();
    let mut reopened = state(&world.root.path().join(format!("worker-{}", index + 1))).await;
    reopened.cluster = Arc::new(runtime);
    let (endpoint, task) = serve(reopened.clone(), cert).await;
    let mut report = world.hosts[index].report.clone();
    report.endpoint = endpoint;
    let response = reopened
        .cluster
        .client(Uuid::from_u128(500))
        .unwrap()
        .post(format!("{}/api/cluster/hosts/register", world.url))
        .json(&report)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    world.hosts[index] = Host {
        state: reopened,
        report,
        task,
    };
    reconcile_one(world.controller.clone(), p.id, true)
        .await
        .unwrap();
    let restored = world.controller.store.cluster_placement(p.id).unwrap();
    assert_eq!(restored.phase, PlacementPhase::Confirmed);
    assert_eq!(restored.vm, p.vm);
    let host = &world.hosts[index];
    assert_eq!(host.state.store.load_all().unwrap().len(), 1);
    assert!(host.state.store.active_lease(p.id).unwrap().is_some());
    assert_eq!(host.state.vms.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workers_revalidate_foreign_network_and_storage_references_and_reject_other_controllers() {
    let world = World::new(2).await;
    let owner = &world.hosts[0];
    let client = world.controller.cluster.client(owner.report.id).unwrap();
    for storage in [false, true] {
        let id = Uuid::new_v4();
        let mut spec = world.spec();
        let mut target = spec.targets[0].clone();
        if storage {
            target.storage.resource_id = "foreign-storage".into();
        } else {
            target.network.resource_id = world.hosts[1].report.networks[0].id.to_string();
        }
        spec.targets[0] = target.clone();
        let intent = WorkerIntent {
            generation: 0,
            action: PlacementAction::Create,
            target,
            spec,
        };
        let response = client
            .put(format!(
                "{}/api/cluster/worker/placements/{id}",
                owner.report.endpoint
            ))
            .json(&intent)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let receipt: WorkerReceipt = response.json().await.unwrap();
        assert_eq!(receipt.phase, PlacementPhase::Rejected);
        assert!(owner.state.vms.lock().unwrap().is_empty());
    }
    let response = world.hosts[1]
        .state
        .cluster
        .client(owner.report.id)
        .unwrap()
        .get(format!(
            "{}/api/cluster/worker/placements/{}",
            owner.report.endpoint,
            Uuid::new_v4()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
}

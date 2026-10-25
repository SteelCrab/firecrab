//! Exercise the public management contract and autonomous worker/controller loops.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn authenticated_heartbeats_refresh_inventory_without_resetting_drain() {
    let world = World::new(2).await;
    let client = world.client();
    let hosts: Vec<ClusterHost> = client
        .get(format!("{}/api/cluster/hosts", world.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hosts.len(), 2);
    let host = &world.hosts[0];
    let id = host.report.id;
    let url = format!("{}/api/cluster/hosts/{id}", world.url);
    let drained: ClusterHost = client
        .patch(&url)
        .json(&DrainHostRequest { draining: true })
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(drained.draining);
    world
        .controller
        .store
        .cluster_register(&host.report, now_ms() - 31_000)
        .unwrap();
    assert!(!world.controller.store.cluster_hosts(now_ms()).unwrap()[0].online);

    let mut updated = host.report.clone();
    updated.capacity.cpu = 4;
    updated.labels.insert("zone".into(), "updated".into());
    let signed = host.state.cluster.client(Uuid::from_u128(500)).unwrap();
    let heartbeat = format!("{url}/heartbeat");
    let response = signed.post(&heartbeat).json(&updated).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let refreshed: ClusterHost = response.json().await.unwrap();
    assert!(refreshed.online && refreshed.draining);
    assert_eq!(refreshed.report, updated);
    let fetched: ClusterHost = client.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(fetched, refreshed);

    let wrong_path = format!(
        "{}/api/cluster/hosts/{}/heartbeat",
        world.url, world.hosts[1].report.id
    );
    assert_eq!(
        signed
            .post(wrong_path)
            .json(&updated)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let foreign = world.hosts[1]
        .state
        .cluster
        .client(Uuid::from_u128(500))
        .unwrap();
    assert_eq!(
        foreign
            .post(&heartbeat)
            .json(&updated)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let mut invalid = updated.clone();
    invalid.endpoint = "http://worker.test".into();
    assert_eq!(
        signed
            .post(&heartbeat)
            .json(&invalid)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let fetched: ClusterHost = client.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(
        fetched.report, updated,
        "invalid reports must not replace trusted inventory"
    );
    assert_eq!(
        client
            .get(format!(
                "{}/api/cluster/hosts/{}",
                world.url,
                Uuid::new_v4()
            ))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn placement_management_preserves_retry_identity_and_exposes_deletion_tombstones() {
    let world = World::new(1).await;
    let client = world.client();
    let url = format!("{}/api/cluster/placements", world.url);
    let spec = world.spec();
    let response = client.post(&url).json(&spec).send().await.unwrap();
    assert_eq!(response.status(), 400);
    assert!(
        world
            .controller
            .store
            .cluster_placements()
            .unwrap()
            .is_empty()
    );
    let response = client
        .post(&url)
        .header("Idempotency-Key", "management")
        .json(&spec)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let placed: VmPlacement = response.json().await.unwrap();
    world.settled(placed.id, PlacementPhase::Confirmed).await;
    let detail = format!("{url}/{}", placed.id);
    let fetched: VmPlacement = client
        .get(&detail)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fetched.vm, placed.vm);
    let placements: Vec<VmPlacement> = client.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(placements, vec![fetched]);
    assert_eq!(
        client
            .get(format!("{url}/{}", Uuid::new_v4()))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );

    let mut conflicting = spec.clone();
    conflicting.name = "different-vm".into();
    assert_eq!(
        client
            .post(&url)
            .header("Idempotency-Key", "management")
            .json(&conflicting)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    world
        .controller
        .store
        .cluster_drain(placed.vm.host_id, true)
        .unwrap();
    assert_eq!(
        client
            .post(&url)
            .header("Idempotency-Key", "new-on-drained-host")
            .json(&spec)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let retry: VmPlacement = client
        .post(&url)
        .header("Idempotency-Key", "management")
        .json(&spec)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retry.id, placed.id);
    let reconciled: VmPlacement = client
        .post(format!("{detail}/reconcile"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reconciled.vm, placed.vm);
    assert_eq!(world.hosts[0].state.vms.lock().unwrap().len(), 1);
    assert_eq!(client.delete(&detail).send().await.unwrap().status(), 202);
    world.settled(placed.id, PlacementPhase::Deleted).await;
    let tombstone: VmPlacement = client
        .get(&detail)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tombstone.phase, PlacementPhase::Deleted);
    assert_eq!(tombstone.vm, placed.vm);
    let placements: Vec<VmPlacement> = client.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(placements, vec![tombstone]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accepted_start_reports_worker_boot_failure_without_losing_ownership_or_capacity() {
    let world = World::new(1).await;
    let placed = world
        .controller
        .store
        .cluster_admit("start-contract", world.spec(), now_ms())
        .unwrap();
    reconcile_one(world.controller.clone(), placed.id, true)
        .await
        .unwrap();
    let url = format!("{}/api/cluster/placements/{}", world.url, placed.id);
    let response = world
        .client()
        .post(format!("{url}/start"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let intent: VmPlacement = response.json().await.unwrap();
    assert_eq!(intent.vm, placed.vm);
    assert_eq!(intent.action, PlacementAction::Start);
    assert_eq!(intent.generation, placed.generation + 1);
    world.settled(placed.id, PlacementPhase::Confirmed).await;
    // The fixture has no bootable rootfs or VMM. The controller must still retain the VM.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if world.hosts[0].state.vms.lock().unwrap()[&placed.id].state == VmState::Error {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fixture VM did not report its boot failure"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let observed: VmPlacement = world
        .client()
        .post(format!("{url}/reconcile"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(observed.vm, placed.vm);
    assert_eq!(observed.observed.unwrap().state, VmState::Error);
    assert!(observed.phase.reserves_capacity());
    assert_eq!(
        world.controller.store.cluster_hosts(now_ms()).unwrap()[0]
            .reserved
            .cpu,
        1
    );
}

#[tokio::test]
async fn production_inventory_separates_unmanaged_usage_from_worker_reservations() {
    let world = World::new(1).await;
    let mut state = world.hosts[0].state.clone();
    let runtime = state.cluster.clone();
    let mut config = runtime.worker.clone().unwrap();
    config.disk_limits.insert("default".into(), 13);
    state.cluster = Arc::new(Runtime {
        identity: runtime.identity,
        paths: runtime.paths.clone(),
        worker: Some(config.clone()),
        ..Runtime::default()
    });
    let local_id = Uuid::new_v4();
    let mut local = crate::handlers::vms::test_support::record("local", local_id);
    local.disk_gb = 2;
    let managed_id = Uuid::new_v4();
    let mut managed = local.clone();
    managed.id = managed_id;
    let spec = world.spec();
    state
        .store
        .cluster_worker_admit(
            managed_id,
            Uuid::from_u128(500),
            WorkerIntent {
                generation: 0,
                action: PlacementAction::Create,
                target: spec.targets[0].clone(),
                spec,
            },
        )
        .unwrap();
    state
        .vms
        .lock()
        .unwrap()
        .extend([(local_id, local.clone()), (managed_id, managed)]);
    let report = inventory(&state).await.unwrap();
    assert_eq!(report.id, world.hosts[0].report.id);
    assert_eq!(report.name, config.name);
    assert_eq!(report.labels, config.labels);
    assert!(report.capacity.cpu > 0);
    assert_eq!(
        report.unmanaged,
        HostCapacity {
            cpu: u32::from(local.cpu),
            ram_mib: u64::from(local.ram)
        }
    );
    assert_eq!(report.storages[0].unmanaged_gib, 2);
    assert_eq!(report.storages[0].capacity_gib, 13);
    assert_eq!(report.images, world.hosts[0].report.images);
    assert_eq!(report.networks, world.hosts[0].report.networks);
    assert_eq!(state.store.cluster_disk_budget("default", 99).unwrap(), 13);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_heartbeat_and_reconciliation_resume_without_an_http_request() {
    let world = World::new(1).await;
    let host = &world.hosts[0];
    world
        .controller
        .store
        .cluster_drain(host.report.id, true)
        .unwrap();
    world
        .controller
        .store
        .cluster_register(&host.report, now_ms() - 31_000)
        .unwrap();
    let mut worker = host.state.clone();
    let mut config = worker.cluster.worker.clone().unwrap();
    config.endpoint = host.report.endpoint.clone();
    worker.cluster = Arc::new(Runtime {
        identity: worker.cluster.identity,
        paths: worker.cluster.paths.clone(),
        worker: Some(config),
        report_override: Some(host.report.clone()),
        ..Runtime::default()
    });
    spawn(worker);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let refreshed = world
            .controller
            .store
            .cluster_hosts(now_ms())
            .unwrap()
            .remove(0);
        if refreshed.online {
            assert!(refreshed.draining);
            assert_eq!(refreshed.report.endpoint, host.report.endpoint);
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "background heartbeat did not refresh host"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    world
        .controller
        .store
        .cluster_drain(host.report.id, false)
        .unwrap();
    let placed = world
        .controller
        .store
        .cluster_admit("queued-at-restart", world.spec(), now_ms())
        .unwrap();
    assert!(host.state.vms.lock().unwrap().is_empty());
    spawn(world.controller.clone());
    let settled = world.settled(placed.id, PlacementPhase::Confirmed).await;
    assert_eq!(settled.vm, placed.vm);
    assert_eq!(host.state.vms.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn loopback_management_reads_state_but_refuses_dispatch_without_mutual_tls() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let app = router(state.clone());
    for path in ["/api/cluster/hosts", "/api/cluster/placements"] {
        let response = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!([])
        );
    }
    let id = Uuid::new_v4();
    let payload = serde_json::to_vec(&spec(&[report(1)])).unwrap();
    for (path, body) in [
        ("/api/cluster/placements".into(), payload),
        (format!("/api/cluster/placements/{id}/start"), vec![]),
        (format!("/api/cluster/placements/{id}/reconcile"), vec![]),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::post(path)
                    .header("Content-Type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 503);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["code"], "cluster_tls_required");
    }
    assert!(state.store.cluster_placements().unwrap().is_empty());
}

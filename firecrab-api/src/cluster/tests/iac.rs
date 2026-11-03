//! Shared IaC authority must preserve the controller's immutable workload owners.
use super::*;
use crate::persistence::PersistenceError;
use firecrab_stack::{
    ChangeResourceOwner, ManagedResource, Manager, ResourceOwner, ResourceOwnership,
};

fn ownership_request(store: &Store, kind: &str, id: Uuid) -> ChangeResourceOwner {
    let resource = ManagedResource {
        kind: kind.into(),
        id: id.to_string(),
    };
    let before = store.iac_resource_owner(&resource).unwrap();
    ChangeResourceOwner {
        operation_id: Uuid::new_v4(),
        expected_endpoint_id: before.endpoint_id,
        resource,
        expected_generation: before.generation,
        expected_owner: before.owner,
        owner: Some(ResourceOwner {
            manager: Manager::Terraform,
            id: Uuid::new_v4(),
            key: format!("firecrab_{kind}.test"),
        }),
    }
}

fn release(store: &Store, owned: ResourceOwnership) {
    store
        .change_iac_resource_owner(&ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: owned.endpoint_id,
            resource: owned.resource,
            expected_generation: owned.generation,
            expected_owner: owned.owner,
            owner: None,
        })
        .unwrap();
}

#[test]
fn iac_cluster_host_transfer_preserves_authenticated_inventory_and_blocks_direct_drain() {
    let (root, store, _) = world(1);
    store.cluster_drain(report(1).id, true).unwrap();
    let before = store.cluster_hosts(1001).unwrap();
    let request = ownership_request(&store, "cluster_host", report(1).id);
    let owned = store.change_iac_resource_owner(&request).unwrap();
    assert!(matches!(
        store.cluster_drain(report(1).id, false),
        Err(ClusterError::Persistence(
            PersistenceError::IacOwnershipConflict
        ))
    ));
    assert_eq!(store.cluster_hosts(1001).unwrap(), before);
    let mut updated = report(1);
    updated.capacity.cpu += 2;
    store.cluster_register(&updated, 1002).unwrap();
    let saved = store.cluster_hosts(1003).unwrap().remove(0);
    assert_eq!(saved.report, updated);
    assert!(saved.draining);
    let reopened = Store::open(&root.path().join("state.db")).unwrap();
    assert_eq!(
        reopened.iac_resource_owner(&request.resource).unwrap(),
        owned
    );
    release(&reopened, owned);
    reopened.cluster_drain(report(1).id, false).unwrap();
    assert_eq!(
        reopened.cluster_hosts(1003).unwrap()[0].report.id,
        report(1).id
    );
}

#[test]
fn iac_cluster_placement_transfer_waits_for_claims_and_preserves_host_qualified_identity() {
    let (_root, store, spec) = world(1);
    let original = store.cluster_admit("original", spec, 1001).unwrap();
    let request = ownership_request(&store, "cluster_placement", original.id);
    assert!(matches!(
        store.change_iac_resource_owner(&request),
        Err(PersistenceError::IacResourceBusy)
    ));
    let (token, mut observed) = store
        .cluster_claim(original.id, 1002, true)
        .unwrap()
        .unwrap();
    observed.phase = PlacementPhase::Confirmed;
    store.cluster_finish(&observed, token, 1003).unwrap();
    let (token, observed) = store
        .cluster_claim(original.id, 2000, true)
        .unwrap()
        .unwrap();
    assert!(matches!(
        store.change_iac_resource_owner(&request),
        Err(PersistenceError::IacResourceBusy)
    ));
    assert!(
        store
            .iac_ownership_operation(request.operation_id)
            .unwrap()
            .is_none()
    );
    store.cluster_finish(&observed, token, 2001).unwrap();
    let owned = store.change_iac_resource_owner(&request).unwrap();
    for action in [
        PlacementAction::Start,
        PlacementAction::Stop,
        PlacementAction::Delete,
    ] {
        assert!(matches!(
            store.cluster_action(original.id, action),
            Err(ClusterError::Persistence(
                PersistenceError::IacOwnershipConflict
            ))
        ));
    }
    assert_eq!(store.cluster_placement(original.id).unwrap(), observed);
    // Existing-intent reconciliation remains available without changing desired configuration.
    let (token, current) = store
        .cluster_claim(original.id, 3000, true)
        .unwrap()
        .unwrap();
    assert_eq!(current.vm, original.vm);
    store.cluster_finish(&current, token, 3001).unwrap();
    release(&store, owned);
    let deletion = store
        .cluster_action(original.id, PlacementAction::Delete)
        .unwrap();
    assert_eq!(deletion.vm, original.vm);
    assert_eq!(deletion.target, original.target);
}

#[test]
fn iac_cluster_placement_new_intent_and_transfer_have_one_winner_across_connections() {
    let (root, store, spec) = world(1);
    let placement = store.cluster_admit("original", spec, 1001).unwrap();
    let (token, mut observed) = store
        .cluster_claim(placement.id, 1002, true)
        .unwrap()
        .unwrap();
    observed.phase = PlacementPhase::Confirmed;
    store.cluster_finish(&observed, token, 1003).unwrap();
    let request = ownership_request(&store, "cluster_placement", placement.id);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let gate = barrier.clone();
    let other = Store::open(&root.path().join("state.db")).unwrap();
    let task = std::thread::spawn(move || {
        gate.wait();
        other
            .cluster_action(placement.id, PlacementAction::Delete)
            .is_ok()
    });
    barrier.wait();
    let transferred = store.change_iac_resource_owner(&request).is_ok();
    let admitted = task.join().unwrap();
    assert_ne!(transferred, admitted);
    assert_eq!(
        store
            .iac_ownership_operation(request.operation_id)
            .unwrap()
            .is_some(),
        transferred
    );
}

#[test]
fn iac_cluster_service_transfer_preserves_release_history_and_rejects_new_intents() {
    let (_root, store, _) = world(1);
    let mut service = services::create(&store, services::service_spec(&[report(1)]));
    let request = ownership_request(&store, "cluster_service", service.id);
    assert!(matches!(
        store.change_iac_resource_owner(&request),
        Err(PersistenceError::IacResourceBusy)
    ));
    let (token, _) = store
        .cluster_service_claim(service.id, 1002, true)
        .unwrap()
        .unwrap();
    service.pending = false;
    service.observed = ClusterServiceObserved::Running;
    for host in &mut service.hosts {
        host.observed_generation = Some(host.intent.generation);
    }
    store.cluster_service_finish(&service, token, 1003).unwrap();
    let before_ops = store.cluster_service_operations(service.id).unwrap();
    let owned = store.change_iac_resource_owner(&request).unwrap();
    for action in [ClusterServiceAction::Update, ClusterServiceAction::Stop] {
        let spec = (action == ClusterServiceAction::Update).then(|| service.spec.clone());
        assert!(matches!(
            store.cluster_service_admit(
                &Uuid::new_v4().to_string(),
                Some(service.id),
                action,
                Some(service.generation),
                spec,
                1004
            ),
            Err(ClusterError::Persistence(
                PersistenceError::IacOwnershipConflict
            ))
        ));
    }
    assert_eq!(store.cluster_service(service.id).unwrap(), service);
    assert_eq!(
        store.cluster_service_operations(service.id).unwrap(),
        before_ops
    );
    // A lost acknowledgement for the original request does not create a new release.
    assert_eq!(services::create(&store, service.spec.clone()), service);
    release(&store, owned);
    let stopped = store
        .cluster_service_admit(
            "stop-after-release",
            Some(service.id),
            ClusterServiceAction::Stop,
            Some(service.generation),
            None,
            1005,
        )
        .unwrap();
    assert_eq!(stopped.id, service.id);
    assert_eq!(stopped.release_generation, service.release_generation);
    assert_eq!(stopped.hosts[0].host_id, service.hosts[0].host_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn iac_cluster_mtls_heartbeat_stays_available_without_granting_direct_drain_authority() {
    let world = World::new(1).await;
    let host = &world.hosts[0];
    let request = ownership_request(&world.controller.store, "cluster_host", host.report.id);
    let owned = world
        .controller
        .store
        .change_iac_resource_owner(&request)
        .unwrap();
    let url = format!("{}/api/cluster/hosts/{}", world.url, host.report.id);
    let response = world
        .client()
        .patch(&url)
        .json(&DrainHostRequest { draining: true })
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    let error: serde_json::Value = response.json().await.unwrap();
    assert_eq!(error["error"]["code"], "iac_resource_managed");
    let signed = host.state.cluster.client(Uuid::from_u128(500)).unwrap();
    let mut updated = host.report.clone();
    updated.capacity.cpu += 2;
    let response = signed
        .post(format!("{url}/heartbeat"))
        .json(&updated)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let observed: ClusterHost = response.json().await.unwrap();
    assert_eq!(observed.report, updated);
    assert!(!observed.draining);
    assert_eq!(
        world
            .controller
            .store
            .iac_resource_owner(&request.resource)
            .unwrap(),
        owned
    );
    let wrong = signed
        .post(format!(
            "{}/api/cluster/hosts/{}/heartbeat",
            world.url,
            Uuid::new_v4()
        ))
        .json(&updated)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 403);
}

#[tokio::test]
async fn iac_cluster_owned_http_routes_reject_changes_before_transport_or_payload_side_effects() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    state.store.cluster_register(&report(1), 1000).unwrap();
    let placement = state
        .store
        .cluster_admit("placement", spec(&[report(1)]), 1001)
        .unwrap();
    let (token, mut observed) = state
        .store
        .cluster_claim(placement.id, 1002, true)
        .unwrap()
        .unwrap();
    observed.phase = PlacementPhase::Confirmed;
    state.store.cluster_finish(&observed, token, 1003).unwrap();
    let mut service = services::create(&state.store, services::service_spec(&[report(1)]));
    let (token, _) = state
        .store
        .cluster_service_claim(service.id, 1002, true)
        .unwrap()
        .unwrap();
    service.pending = false;
    for host in &mut service.hosts {
        host.observed_generation = Some(host.intent.generation);
    }
    state
        .store
        .cluster_service_finish(&service, token, 1003)
        .unwrap();
    for (kind, id) in [
        ("cluster_host", report(1).id),
        ("cluster_placement", placement.id),
        ("cluster_service", service.id),
    ] {
        state
            .store
            .change_iac_resource_owner(&ownership_request(&state.store, kind, id))
            .unwrap();
    }
    let app = router(state.clone());
    for (method, path) in [
        ("PATCH", format!("/api/cluster/hosts/{}", report(1).id)),
        (
            "POST",
            format!("/api/cluster/placements/{}/start", placement.id),
        ),
        (
            "POST",
            format!("/api/cluster/placements/{}/stop", placement.id),
        ),
        (
            "DELETE",
            format!("/api/cluster/placements/{}", placement.id),
        ),
        (
            "POST",
            format!("/api/cluster/placements/{}/delete", placement.id),
        ),
        ("PUT", format!("/api/cluster/services/{}", service.id)),
        (
            "POST",
            format!("/api/cluster/services/{}/update", service.id),
        ),
        ("POST", format!("/api/cluster/services/{}/stop", service.id)),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 409);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let error: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(error["error"]["code"], "iac_resource_managed");
    }
    for path in [
        format!("/api/cluster/hosts/{}", report(1).id),
        format!("/api/cluster/placements/{}", placement.id),
        format!("/api/cluster/services/{}", service.id),
    ] {
        assert_eq!(
            app.clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status(),
            200
        );
    }
    assert_eq!(
        state.store.cluster_placement(placement.id).unwrap(),
        observed
    );
    assert_eq!(state.store.cluster_service(service.id).unwrap(), service);
}

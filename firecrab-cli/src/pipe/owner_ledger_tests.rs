use super::*;
use firecrab_helper_protocol::host_iac::OwnerChange;
use firecrab_stack::{ChangeResourceOwner, Manager, ResourceOwner};

#[test]
fn native_systemd_jobs_block_a_new_transfer_but_not_a_retained_receipt() {
    let mut ledger = Ledger::default();
    let config = config();
    let request = request(&ledger, Some(&config), "listener");
    ledger.set_runtime_busy(true);
    assert!(
        ledger
            .change(&host(), Some(&config), request.clone())
            .is_err()
    );
    ledger.set_runtime_busy(false);
    let receipt = ledger
        .change(&host(), Some(&config), request.clone())
        .unwrap();
    ledger.set_runtime_busy(true);
    assert_eq!(ledger.change(&host(), None, request).unwrap(), receipt);
}

#[test]
fn long_existing_route_paths_use_one_canonical_digest_and_remain_protected() {
    let mut current = config();
    current.routes[0].path_prefix = format!("/{}", "long".repeat(512));
    let key = route_key(&current.routes[0]);
    assert!(key.starts_with("route:sha256:"));
    let mut ledger = Ledger::default();
    let intent = request(&ledger, Some(&current), &key);
    ledger.change(&host(), Some(&current), intent).unwrap();
    let mut changed = current.clone();
    changed.routes[0].upstreams[0].set_port(9000);
    assert!(
        ledger
            .check_configuration(Some(&current), &changed)
            .is_err()
    );
    changed = current.clone();
    changed.routes[0].path_prefix.push('/');
    assert_eq!(key, route_key(&changed.routes[0]));
}

#[test]
fn no_effect_rejection_is_immutable_even_when_the_configuration_is_restored() {
    let mut ledger = Ledger::default();
    let original = config();
    let request = request(&ledger, Some(&original), "listener");
    let mut changed = original.clone();
    changed.listen.set_port(8088);
    let rejected = ledger
        .transition(&host(), Some(&changed), request.clone())
        .unwrap();
    assert!(matches!(rejected, Reply::OwnerRejected { .. }));
    let mut restarted: Ledger =
        serde_json::from_slice(&serde_json::to_vec(&ledger).unwrap()).unwrap();
    assert!(restarted.has_outcome(request.change.operation_id));
    assert_eq!(
        restarted
            .transition(&host(), Some(&original), request.clone())
            .unwrap(),
        rejected
    );
    assert!(restarted.check_ingress_control().is_ok());
    let mut substituted = request.clone();
    substituted.change.owner = None;
    assert!(restarted.transition(&host(), None, substituted).is_err());
    let mut retry = request;
    retry.change.operation_id = Uuid::new_v4();
    assert!(matches!(
        restarted
            .transition(&host(), Some(&original), retry)
            .unwrap(),
        Reply::OwnerReceipt { receipt: Some(_) }
    ));
}

fn host() -> HostIdentity {
    HostIdentity {
        machine_id: "a".repeat(32),
        configuration_root: "/etc/firecrab".into(),
        device: 1,
        inode: 42,
    }
}
fn config() -> Config {
    serde_json::from_value(serde_json::json!({"routes":[
        {"host":"app.example.com","pathPrefix":"/", "upstreams":["127.0.0.1:8081"]},
        {"host":"peer.example.com","pathPrefix":"/", "upstreams":["127.0.0.1:8082"]}
    ],"tunnel":null}))
    .unwrap()
}
fn request(ledger: &Ledger, config: Option<&Config>, resource: &str) -> OwnerChange {
    let resource = ManagedResource {
        kind: "ingress".into(),
        id: resource.into(),
    };
    let endpoint = Uuid::from_u128(548);
    let observed = ledger
        .observe(&host(), endpoint, &resource, config)
        .unwrap();
    OwnerChange {
        host: host(),
        configuration_sha256: observed.configuration_sha256,
        change: ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: endpoint,
            resource,
            expected_generation: observed.ownership.generation,
            expected_owner: observed.ownership.owner,
            owner: Some(ResourceOwner {
                manager: Manager::Terraform,
                id: Uuid::from_u128(2),
                key: "firecrab_ingress.app".into(),
            }),
        },
    }
}
#[test]
fn ownership_survives_restart_and_native_writes_preserve_unrelated_routes() {
    let mut ledger = Ledger::default();
    let current = config();
    let original = request(&ledger, Some(&current), "route:app.example.com/");
    let receipt = ledger
        .change(&host(), Some(&current), original.clone())
        .unwrap();
    assert!(!receipt.confirmed);
    let persisted = serde_json::to_vec(&ledger).unwrap();
    let mut ledger: Ledger = serde_json::from_slice(&persisted).unwrap();
    assert_eq!(
        ledger.change(&host(), None, original.clone()).unwrap(),
        receipt
    );
    assert_eq!(
        ledger
            .receipt(
                &host(),
                original.change.expected_endpoint_id,
                receipt.operation_id
            )
            .unwrap(),
        Some(receipt.clone())
    );
    let mut peer = current.clone();
    peer.routes[1].upstreams[0].set_port(8090);
    assert!(ledger.check_configuration(Some(&current), &peer).is_ok());
    let mut changed_listener = current.clone();
    changed_listener.listen.set_port(8088);
    assert!(
        ledger
            .check_configuration(Some(&current), &changed_listener)
            .is_err()
    );
    peer.routes[0].upstreams[0].set_port(8091);
    assert!(ledger.check_configuration(Some(&current), &peer).is_err());
    assert!(ledger.check_ingress_control().is_err());
    ledger
        .confirm(
            &host(),
            original.change.expected_endpoint_id,
            receipt.operation_id,
        )
        .unwrap();
    assert!(ledger.check_ingress_control().is_err());
}
#[test]
fn release_retains_reservation_until_the_exact_api_mirror_is_confirmed() {
    let mut ledger = Ledger::default();
    let current = config();
    let adopt = request(&ledger, Some(&current), "listener");
    let first = ledger
        .change(&host(), Some(&current), adopt.clone())
        .unwrap();
    let mut release = request(&ledger, Some(&current), "listener");
    release.change.owner = None;
    assert!(
        ledger
            .change(&host(), Some(&current), release.clone())
            .is_err()
    );
    ledger
        .confirm(&host(), first.ownership.endpoint_id, first.operation_id)
        .unwrap();
    let released = ledger.change(&host(), Some(&current), release).unwrap();
    assert!(ledger.check_ingress_control().is_err());
    assert!(
        ledger
            .confirm(&host(), Uuid::new_v4(), released.operation_id)
            .is_err()
    );
    ledger
        .confirm(
            &host(),
            released.ownership.endpoint_id,
            released.operation_id,
        )
        .unwrap();
    assert!(ledger.check_ingress_control().is_ok());
    assert_eq!(
        ledger.change(&host(), None, adopt).unwrap().ownership,
        first.ownership
    );
    assert_eq!(released.ownership.generation, 2);
}
#[test]
fn stale_host_configuration_owner_and_reused_request_ids_cannot_change_metadata() {
    let mut ledger = Ledger::default();
    let current = config();
    let original = request(&ledger, Some(&current), "listener");
    let mut replacement = host();
    replacement.inode += 1;
    assert!(
        ledger
            .change(&replacement, Some(&current), original.clone())
            .is_err()
    );
    let mut next = current.clone();
    next.listen.set_port(8088);
    assert!(
        ledger
            .change(&host(), Some(&next), original.clone())
            .is_err()
    );
    let receipt = ledger
        .change(&host(), Some(&current), original.clone())
        .unwrap();
    ledger
        .confirm(&host(), receipt.ownership.endpoint_id, receipt.operation_id)
        .unwrap();
    let mut stale = original.clone();
    stale.change.operation_id = Uuid::new_v4();
    assert!(ledger.change(&host(), Some(&current), stale).is_err());
    let mut substituted = original;
    substituted.change.owner = None;
    assert!(ledger.change(&host(), None, substituted).is_err());
    assert!(
        ledger
            .receipt(
                &replacement,
                receipt.ownership.endpoint_id,
                receipt.operation_id
            )
            .is_err()
    );
}
#[test]
fn generated_and_absent_routes_cannot_be_adopted_and_aliases_cannot_bypass_the_lock() {
    let mut ledger = Ledger::default();
    let mut current = config();
    let own: firecrab_helper_protocol::pipe::ServiceRouteOwner = serde_json::from_value(
        serde_json::json!({"serviceId":Uuid::new_v4(),"deploymentId":Uuid::new_v4()}),
    )
    .unwrap();
    current.routes[0].owner = Some(own);
    let generated = request(&ledger, Some(&current), "route:app.example.com/");
    assert!(ledger.change(&host(), Some(&current), generated).is_err());
    let absent = request(&ledger, Some(&current), "route:missing.example.com/");
    assert!(ledger.change(&host(), Some(&current), absent).is_err());
    for id in [
        "route:App.example.com/",
        "route:app.example.com/a/",
        "route:app.example.com/$bad",
        "route:quick.firecrab.internal/",
        "route:app.example.com/*",
        "arbitrary.service",
    ] {
        assert!(
            firecrab_helper_protocol::host_iac::validate_resource(&ManagedResource {
                kind: "ingress".into(),
                id: id.into()
            })
            .is_err(),
            "{id}"
        );
    }
    assert!(
        firecrab_helper_protocol::host_iac::validate_resource(&ManagedResource {
            kind: "ingress".into(),
            id: "route:app.example.com/a".into()
        })
        .is_ok()
    );
}

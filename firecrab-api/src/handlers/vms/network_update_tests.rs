use super::test_support::{record, seed_vm, test_state};
use super::*;
use crate::firecracker::test_support::short_tempdir;
use crate::network::NetworkClient;
use firecrab_helper_protocol::{
    PROTOCOL_VERSION,
    framing::{read_frame, write_frame},
    network::{HelperFailure, NetworkRequest, NetworkRequestEnvelope, NetworkResponseEnvelope},
};
use std::sync::Mutex;

fn network(state: &AppState, cidr: &str) -> Uuid {
    let id = Uuid::new_v4();
    let spec = SubnetSpec::parse(id, cidr).unwrap();
    let value = serde_json::from_value(serde_json::json!({
        "id": id,
        "name": format!("network-{id}"),
        "subnetCidr": cidr,
        "gateway": spec.gateway(),
        "internetEnabled": false,
        "dnsMode": "legacy_forward",
    }))
    .unwrap();
    state.store.insert_micro_network(&value).unwrap();
    id
}
fn request(vm: &VmRecord, target: Option<Uuid>) -> UpdateVmResourcesRequest {
    UpdateVmResourcesRequest {
        micro_network_id: target,
        cpu: vm.cpu,
        ram: vm.ram,
        disk_gb: vm.disk_gb,
        egress_policy: vm.egress_policy,
        env: None,
        security_group_id: None,
    }
}
async fn update(
    state: &AppState,
    vm: &VmRecord,
    req: UpdateVmResourcesRequest,
) -> Result<Json<VmResponse>, AppError> {
    update_vm(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(vm.id.to_string()),
        ValidatedJson(req),
    )
    .await
}
fn helper(
    root: &std::path::Path,
    fail_sync: bool,
) -> (NetworkClient, Arc<Mutex<Vec<NetworkRequest>>>) {
    let socket = root.join("capture.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let received = log.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let received = received.clone();
            tokio::spawn(async move {
                let Ok(envelope) = read_frame::<_, NetworkRequestEnvelope>(&mut stream).await
                else {
                    return;
                };
                let fail =
                    fail_sync && matches!(&envelope.request, NetworkRequest::SyncDhcpLeases { .. });
                received.lock().unwrap().push(envelope.request);
                let response = NetworkResponseEnvelope {
                    version: PROTOCOL_VERSION,
                    request_id: envelope.request_id,
                    pipe: None,
                    result: if fail {
                        Err(HelperFailure::Internal {
                            detail: "injected DHCP error".into(),
                        })
                    } else {
                        Ok(())
                    },
                };
                let _ = write_frame(&mut stream, &response).await;
            });
        }
    });
    (NetworkClient::with_socket_path(socket), log)
}
fn assert_lease(state: &AppState, vm: Uuid, lease: &Lease) {
    let actual = state.store.active_lease(vm).unwrap().unwrap();
    assert_eq!(
        (actual.ipv4, actual.ipv6, actual.mac),
        (lease.ipv4, lease.ipv6, lease.mac)
    );
}

#[tokio::test]
async fn move_updates_record_lease_dhcp_membership_and_next_start_without_live_tap_work() {
    for status in [VmState::Created, VmState::Stopped, VmState::Error] {
        let root = short_tempdir();
        let state = test_state(root.path()).await;
        let mut vm = record("network-move", Uuid::new_v4());
        vm.state = status;
        seed_vm(&state, &vm);
        let target = network(&state, "10.50.0.0/24");
        let old = state.store.active_lease(vm.id).unwrap().unwrap();
        let (client, log) = helper(root.path(), false);
        let state = state.with_test_network(client);
        let changed = update(&state, &vm, request(&vm, Some(target)))
            .await
            .unwrap()
            .0;
        assert_eq!(changed.micro_network_id, target);
        assert!(changed.ipv4.as_ref().unwrap().starts_with("10.50.0."));
        assert_eq!(
            state.store.load_all().unwrap()[&vm.id].micro_network_id,
            target
        );
        assert_eq!(state.vms.lock().unwrap()[&vm.id].micro_network_id, target);
        assert!(
            !state
                .store
                .micro_network_has_active_leases(vm.micro_network_id)
                .unwrap()
        );
        assert!(state.store.micro_network_has_active_leases(target).unwrap());
        let original_members = crate::handlers::micro_networks::get_micro_network(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(vm.micro_network_id.to_string()),
        )
        .await
        .unwrap()
        .0;
        let destination_members = crate::handlers::micro_networks::get_micro_network(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(target.to_string()),
        )
        .await
        .unwrap()
        .0;
        assert!(original_members.vms.is_empty());
        assert_eq!(destination_members.vms.len(), 1);
        assert_eq!(destination_members.vms[0].id, vm.id);
        assert_eq!(destination_members.vms[0].ipv4, changed.ipv4);
        let lease = state.store.active_lease(vm.id).unwrap().unwrap();
        {
            let log = log.lock().unwrap();
            let leases = log
                .iter()
                .find_map(|message| match message {
                    NetworkRequest::SyncDhcpLeases { leases, .. } => Some(leases),
                    _ => None,
                })
                .expect("DHCP synchronization was not requested");
            assert!(leases.iter().any(|entry| entry.ipv4 == lease.ipv4));
            assert!(leases.iter().all(|entry| entry.ipv4 != old.ipv4));
            assert!(!log.iter().any(|message| matches!(
                message,
                NetworkRequest::CreateTap { .. } | NetworkRequest::ApplyVmPolicy { .. }
            )));
        }
        let revision = state.store.lease_revision().unwrap();
        setup_vm_network(&state, vm.id, vm.egress_policy, target)
            .await
            .unwrap();
        assert_eq!(
            state.store.lease_revision().unwrap(),
            revision,
            "start reuses the new lease"
        );
        assert_lease(&state, vm.id, &lease);
        assert!(log.lock().unwrap().iter().any(|message| matches!(
            message,
            NetworkRequest::CreateTap { vm_id, micro_network_id }
                if *vm_id == vm.id && *micro_network_id == target
        )));
        let _ = crate::handlers::micro_networks::delete_micro_network(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(vm.micro_network_id.to_string()),
        )
        .await
        .unwrap();
        assert!(
            state
                .store
                .micro_network(vm.micro_network_id)
                .unwrap()
                .is_none()
        );
        let error = crate::handlers::micro_networks::delete_micro_network(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(target.to_string()),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    }
}

#[tokio::test]
async fn same_or_omitted_network_preserves_the_lease_and_running_env_updates() {
    let root = short_tempdir();
    let state = test_state(root.path()).await;
    let mut vm = record("unchanged", Uuid::new_v4());
    vm.state = VmState::Running;
    seed_vm(&state, &vm);
    let lease = state.store.active_lease(vm.id).unwrap().unwrap();
    let revision = state.store.lease_revision().unwrap();
    let (client, log) = helper(root.path(), false);
    let state = state.with_test_network(client);
    for target in [None, Some(vm.micro_network_id)] {
        let mut req = request(&vm, target);
        req.env = Some(BTreeMap::from([("APP".into(), "new".into())]));
        let result = update(&state, &vm, req).await.unwrap().0;
        assert_eq!(result.env.get("APP").map(String::as_str), Some("new"));
        assert_eq!(result.micro_network_id, vm.micro_network_id);
        assert_lease(&state, vm.id, &lease);
        assert_eq!(state.store.lease_revision().unwrap(), revision);
    }
    assert!(log.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unknown_network_and_live_states_fail_before_changing_resources_or_lease() {
    for status in [
        VmState::Created,
        VmState::Running,
        VmState::Starting,
        VmState::Stopping,
        VmState::Paused,
    ] {
        let root = short_tempdir();
        let state = test_state(root.path()).await;
        let mut vm = record("reject-move", Uuid::new_v4());
        vm.state = status;
        seed_vm(&state, &vm);
        let lease = state.store.active_lease(vm.id).unwrap().unwrap();
        let error = update(&state, &vm, request(&vm, Some(Uuid::new_v4())))
            .await
            .err()
            .unwrap();
        assert!(error.field_names().contains(&"microNetworkId".to_string()));
        assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
        if status != VmState::Created {
            let target = network(&state, "10.51.0.0/24");
            let mut req = request(&vm, Some(target));
            req.env = Some(BTreeMap::new());
            let error = update(&state, &vm, req).await.err().unwrap();
            assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
        }
        assert_eq!(state.vms.lock().unwrap()[&vm.id], vm);
        assert_eq!(state.store.load_all().unwrap()[&vm.id], vm);
        assert_lease(&state, vm.id, &lease);
    }
}

#[tokio::test]
async fn exhausted_destination_and_failed_lease_insert_roll_back_vm_and_lease_together() {
    for failure in ["exhausted", "insert"] {
        let root = short_tempdir();
        let state = test_state(root.path()).await;
        let vm = record("rollback-move", Uuid::new_v4());
        seed_vm(&state, &vm);
        let target = network(&state, "10.52.0.0/30");
        if failure == "exhausted" {
            state
                .store
                .allocate_lease(
                    Uuid::new_v4(),
                    SubnetSpec::parse(target, "10.52.0.0/30").unwrap(),
                )
                .unwrap();
        } else {
            let conn = rusqlite::Connection::open(root.path().join("data/firecrab.db")).unwrap();
            conn.execute_batch(&format!(
                "CREATE TRIGGER reject_new_lease BEFORE INSERT ON network_leases
                 WHEN NEW.micro_network_id='{target}'
                 BEGIN SELECT RAISE(FAIL,'injected lease insert error'); END;"
            ))
            .unwrap();
        }
        let lease = state.store.active_lease(vm.id).unwrap().unwrap();
        let revision = state.store.lease_revision().unwrap();
        let mut req = request(&vm, Some(target));
        req.cpu = 2;
        req.env = Some(BTreeMap::from([("APP".into(), "changed".into())]));
        let error = update(&state, &vm, req).await.err().unwrap();
        if failure == "exhausted" {
            assert!(error.field_names().contains(&"microNetworkId".to_string()));
        }
        assert_eq!(
            error.into_response().status(),
            if failure == "exhausted" {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        );
        assert_eq!(state.store.load_all().unwrap()[&vm.id], vm);
        assert_eq!(state.vms.lock().unwrap()[&vm.id], vm);
        assert_lease(&state, vm.id, &lease);
        assert_eq!(state.store.lease_revision().unwrap(), revision);
    }
}

#[tokio::test]
async fn network_move_does_not_detach_an_incompatible_security_group_implicitly() {
    let root = short_tempdir();
    let state = test_state(root.path()).await;
    let mut vm = record("protected", Uuid::new_v4());
    seed_vm(&state, &vm);
    let group = Uuid::new_v4();
    state
        .store
        .insert_security_group(
            &serde_json::from_value(serde_json::json!({
                "id": group,
                "name": "original",
                "microNetworkId": vm.micro_network_id,
                "rules": [],
            }))
            .unwrap(),
        )
        .unwrap();
    vm.security_group_id = Some(group);
    state.store.update(&vm).unwrap();
    state.vms.lock().unwrap().insert(vm.id, vm.clone());
    let target = network(&state, "10.53.0.0/24");
    let lease = state.store.active_lease(vm.id).unwrap().unwrap();
    let error = update(&state, &vm, request(&vm, Some(target)))
        .await
        .err()
        .unwrap();
    assert!(error.field_names().contains(&"securityGroupId".to_string()));
    assert_eq!(
        state.store.load_all().unwrap()[&vm.id].security_group_id,
        Some(group)
    );
    assert_lease(&state, vm.id, &lease);
    let mut req = request(&vm, Some(target));
    req.security_group_id = Some(None);
    let result = update(&state, &vm, req).await.unwrap().0;
    assert_eq!(result.security_group_id, None);
    assert_eq!(result.micro_network_id, target);
    let moved = state.vms.lock().unwrap()[&vm.id].clone();
    let mut req = request(&moved, Some(vm.micro_network_id));
    req.security_group_id = Some(Some(group));
    let restored = update(&state, &moved, req).await.unwrap().0;
    assert_eq!(restored.micro_network_id, vm.micro_network_id);
    assert_eq!(restored.security_group_id, Some(group));
}

#[tokio::test]
async fn stale_error_state_with_a_live_shim_lock_cannot_change_network() {
    use std::os::fd::AsRawFd;
    let root = short_tempdir();
    let state = test_state(root.path()).await;
    let mut vm = record("orphaned", Uuid::new_v4());
    vm.state = VmState::Error;
    seed_vm(&state, &vm);
    let target = network(&state, "10.54.0.0/24");
    let paths =
        crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id);
    std::fs::create_dir_all(&paths.dir).unwrap();
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(paths.dir.join("vm.lock"))
        .unwrap();
    // SAFETY: this test owns the file descriptor and drop releases its lock.
    assert_eq!(
        unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let error = update(&state, &vm, request(&vm, Some(target)))
        .await
        .err()
        .unwrap();
    assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    assert_eq!(
        state.store.load_all().unwrap()[&vm.id].micro_network_id,
        vm.micro_network_id
    );
}

#[tokio::test]
async fn dhcp_failure_reports_the_committed_network_and_keeps_the_new_lease() {
    let root = short_tempdir();
    let state = test_state(root.path()).await;
    let vm = record("dhcp-failure", Uuid::new_v4());
    seed_vm(&state, &vm);
    let target = network(&state, "10.55.0.0/24");
    let (client, _log) = helper(root.path(), true);
    let state = state.with_test_network(client);
    let error = update(&state, &vm, request(&vm, Some(target)))
        .await
        .err()
        .unwrap();
    assert!(error.description().contains("change was saved"));
    assert_eq!(
        error.into_response().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        state.store.load_all().unwrap()[&vm.id].micro_network_id,
        target
    );
    assert_eq!(state.vms.lock().unwrap()[&vm.id].micro_network_id, target);
    assert!(
        state
            .store
            .active_lease(vm.id)
            .unwrap()
            .unwrap()
            .ipv4
            .to_string()
            .starts_with("10.55.0.")
    );
}

#[tokio::test]
async fn cancelled_http_waiter_cannot_release_the_move_guard_before_commit_and_sync() {
    let root = short_tempdir();
    let state = test_state(root.path()).await;
    let vm = record("cancelled-move", Uuid::new_v4());
    seed_vm(&state, &vm);
    let target = network(&state, "10.56.0.0/24");
    let (client, log) = helper(root.path(), false);
    let state = state.with_test_network(client);
    let blocker = rusqlite::Connection::open(root.path().join("data/firecrab.db")).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let moving = state.clone();
    let original = vm.clone();
    let first =
        tokio::spawn(
            async move { update(&moving, &original, request(&original, Some(target))).await },
        );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while state.vms.lock().unwrap()[&vm.id].micro_network_id != target {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    first.abort();
    let following = state.clone();
    let original = vm.clone();
    let second =
        tokio::spawn(async move { update(&following, &original, request(&original, None)).await });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert!(
        !second.is_finished(),
        "a cancelled HTTP waiter released the live move's guard"
    );
    blocker.execute_batch("ROLLBACK").unwrap();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        state.store.load_all().unwrap()[&vm.id].micro_network_id,
        target
    );
    assert_eq!(state.vms.lock().unwrap()[&vm.id].micro_network_id, target);
    assert!(
        log.lock()
            .unwrap()
            .iter()
            .any(|request| matches!(request, NetworkRequest::SyncDhcpLeases { .. }))
    );
}

#[tokio::test]
async fn managed_pool_members_cannot_be_moved_through_vm_resource_updates() {
    let root = short_tempdir();
    let state = test_state(root.path()).await;
    let mut vm = record("pool-member", Uuid::new_v4());
    vm.purpose = crate::model::VmPurpose::Pool;
    seed_vm(&state, &vm);
    let target = network(&state, "10.57.0.0/24");
    let error = update(&state, &vm, request(&vm, Some(target)))
        .await
        .err()
        .unwrap();
    assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    assert_eq!(state.store.load_all().unwrap()[&vm.id], vm);
}

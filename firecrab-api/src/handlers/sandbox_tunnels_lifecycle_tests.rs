use super::*;
use firecrab_helper_protocol::{
    framing::{read_frame, write_frame},
    network::{NetworkRequest, NetworkRequestEnvelope, NetworkResponseEnvelope},
};

#[tokio::test]
async fn address_discovery_returns_saved_claim_without_registration_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = state(dir.path()).await;
    let row = binding();
    task(&state, &row);
    state.store.save_sandbox_tunnel(&row).unwrap();
    let worker = helper(
        &mut state,
        dir.path(),
        vec![(Operation::Inspect, Some(snapshot(&row)))],
    )
    .await;
    let listed = addresses(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Caller::default(),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(
        serde_json::to_value(listed).unwrap(),
        json!([{
            "subdomain":row.spec.subdomain,"publicUrl":"https://sandbox-preview.firecrab.dev",
            "connected":true,"claimedBy":row.id,
        }])
    );
    worker.await.unwrap();
    let delegate: Caller =
        serde_json::from_value(json!({"principal":{"host":Uuid::new_v4()},"operator":false}))
            .unwrap();
    assert_eq!(
        addresses(State(state), Extension(RequestId(Uuid::new_v4())), delegate)
            .await
            .unwrap_err()
            .code(),
        "sandbox_permission_denied"
    );
}

async fn helper(
    state: &mut AppState,
    root: &std::path::Path,
    replies: Vec<(Operation, Option<Snapshot>)>,
) -> tokio::task::JoinHandle<()> {
    let socket = root.join(format!(
        "h{}.sock",
        &Uuid::new_v4().simple().to_string()[..8]
    ));
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    state.network = crate::network::NetworkClient::with_socket_path(socket);
    tokio::spawn(async move {
        for (operation, response) in replies {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(30), listener.accept())
                .await
                .expect("expected helper operation")
                .unwrap();
            let envelope: NetworkRequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert_eq!(
                envelope.request,
                NetworkRequest::Pipe { request: operation }
            );
            if let Some(snapshot) = response {
                write_frame(
                    &mut stream,
                    &NetworkResponseEnvelope {
                        version: firecrab_helper_protocol::PROTOCOL_VERSION,
                        request_id: envelope.request_id,
                        result: Ok(()),
                        pipe: Some(snapshot),
                    },
                )
                .await
                .unwrap();
            }
            // A dropped reply models an accepted helper mutation with a lost result.
        }
    })
}

fn live_vm(state: &AppState, binding: &SandboxTunnelBinding) -> std::net::Ipv4Addr {
    let network = Uuid::from_u128(0x414);
    if state.store.micro_network(network).unwrap().is_none() {
        let row = serde_json::from_value(json!({"id":network,"name":"test-loopback","subnetCidr":"127.0.0.0/24","gateway":"127.0.0.1","internetEnabled":false,"uplink":null,"ipv6Cidr":null,"ipv6Gateway":null,"ipv6AddressMode":null,"ipv6Egress":null})).unwrap();
        state.store.insert_micro_network(&row).unwrap();
    }
    let mut record = crate::handlers::vms::test_support::record("preview", binding.runtime.vm_id);
    record.state = VmState::Running;
    record.last_runtime_id = Some(binding.runtime.runtime_id);
    record.micro_network_id = network;
    state.store.insert(&record).unwrap();
    state.vms.lock().unwrap().insert(record.id, record.clone());
    state
        .store
        .allocate_lease(
            record.id,
            crate::ipam::SubnetSpec::parse(network, "127.0.0.0/24").unwrap(),
        )
        .unwrap()
        .ipv4
}

#[cfg(target_os = "linux")]
fn applied(binding: &mut SandboxTunnelBinding, ip: std::net::Ipv4Addr, port: u16) {
    binding.spec.target = SandboxTunnelTarget::VmApp {
        runtime: binding.runtime,
        port,
    };
    binding.applied_target = Some(SandboxTunnelResolvedTarget::Apps {
        upstreams: vec![SocketAddr::new(ip.into(), port)],
    });
}

// Linux routes 127/8 without adding host interface aliases.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn saved_address_lifecycle_retains_claims_until_local_delete_and_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = state(dir.path()).await;
    let app = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let mut row = binding();
    task(&state, &row);
    let ip = live_vm(&state, &row);
    applied(&mut row, ip, app.local_addr().unwrap().port());
    let connected = snapshot(&row);
    let mut inactive = connected.clone();
    inactive.firecrab_tunnels[0].connected = false;
    inactive.quick_tunnels[0].unit.active_state = "inactive".into();
    inactive.quick_tunnels[0].unit.main_pid = 0;
    let mut removed = inactive.clone();
    removed.firecrab_tunnels.clear();
    removed.quick_tunnels.clear();
    let worker = helper(
        &mut state,
        dir.path(),
        vec![
            (Operation::Inspect, Some(connected.clone())),
            (Operation::Inspect, Some(connected.clone())),
            (
                Operation::ManageFirecrabTunnel {
                    request: firecrab_tunnel::Request::Save {
                        subdomain: row.spec.subdomain.clone(),
                        target: resolved(row.applied_target.as_ref().unwrap()),
                    },
                },
                Some(connected.clone()),
            ),
            (Operation::Inspect, Some(connected.clone())),
            (Operation::Inspect, Some(connected)),
            (
                Operation::ManageQuickTunnel {
                    request: QuickTunnelRequest::Control {
                        id: row.connector_id.unwrap(),
                        service_action: ServiceAction::Stop,
                    },
                },
                Some(inactive.clone()),
            ),
            (Operation::Inspect, Some(inactive)),
            (
                Operation::ManageFirecrabTunnel {
                    request: firecrab_tunnel::Request::Delete {
                        subdomain: row.spec.subdomain.clone(),
                    },
                },
                Some(removed),
            ),
        ],
    )
    .await;
    let request = RequestId(Uuid::new_v4());
    let Json(saved) = save(
        State(state.clone()),
        Path((row.project_id, row.id)),
        Extension(request),
        Caller::default(),
        ValidatedJson(row.spec.clone()),
    )
    .await
    .unwrap();
    assert_eq!(saved.state, SandboxTunnelState::Connected);
    assert_eq!(
        (saved.task_id, saved.attempt_id, saved.runtime),
        (row.task_id, row.attempt_id, row.runtime)
    );
    assert_eq!(saved.public_https, SandboxTunnelVerification::Unverified);
    // Repeating the same selection neither republishes nor changes the immutable identity.
    assert_eq!(
        save(
            State(state.clone()),
            Path((row.project_id, row.id)),
            Extension(request),
            Caller::default(),
            ValidatedJson(row.spec.clone())
        )
        .await
        .unwrap()
        .0,
        saved
    );
    let listed = list(
        State(state.clone()),
        Path(row.project_id),
        Extension(request),
        Caller::default(),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(listed, vec![saved.clone()]);
    assert_eq!(
        get(
            State(state.clone()),
            Path(row.id),
            Extension(request),
            Caller::default()
        )
        .await
        .unwrap()
        .0,
        saved
    );
    let reconciled = reconcile(
        State(state.clone()),
        Path(row.id),
        Extension(request),
        Caller::default(),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(reconciled.state, SandboxTunnelState::Connected);
    let stopped = stop(
        State(state.clone()),
        Path(row.id),
        Extension(request),
        Caller::default(),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(stopped.state, SandboxTunnelState::Stopped);
    assert_eq!(stopped.public_url, saved.public_url);
    assert_eq!(
        state
            .store
            .sandbox_tunnel_claim(&row.spec.subdomain)
            .unwrap(),
        Some(row.id)
    );
    assert_eq!(
        stop(
            State(state.clone()),
            Path(row.id),
            Extension(request),
            Caller::default()
        )
        .await
        .unwrap()
        .0,
        stopped
    );
    let deleted = delete(
        State(state.clone()),
        Path(row.id),
        Extension(request),
        Caller::default(),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(deleted.state, SandboxTunnelState::Deleted);
    assert!(deleted.local_removed);
    assert_eq!(
        deleted.remote_retirement,
        SandboxTunnelRetirement::OperatorRequired
    );
    assert_eq!(
        deleted.public_shutdown,
        SandboxTunnelVerification::Unverified
    );
    assert_eq!(
        state
            .store
            .sandbox_tunnel_claim(&row.spec.subdomain)
            .unwrap(),
        None
    );
    worker.await.unwrap();
    drop(state);
    let reopened = super::state(dir.path()).await;
    assert_eq!(
        get(
            State(reopened.clone()),
            Path(row.id),
            Extension(request),
            Caller::default()
        )
        .await
        .unwrap()
        .0,
        deleted
    );
    assert_eq!(
        delete(
            State(reopened.clone()),
            Path(row.id),
            Extension(request),
            Caller::default()
        )
        .await
        .unwrap()
        .0,
        deleted
    );
    assert_eq!(
        reconcile(
            State(reopened.clone()),
            Path(row.id),
            Extension(request),
            Caller::default()
        )
        .await
        .unwrap()
        .0,
        deleted
    );
    assert_eq!(
        publish(
            State(reopened),
            Path(row.id),
            Extension(request),
            Caller::default()
        )
        .await
        .unwrap_err()
        .code(),
        "sandbox_preview_deleted"
    );
}

// Linux routes 127/8 without adding host interface aliases.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn readiness_defers_publish_and_uncertain_helper_result_reserves_exact_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = state(dir.path()).await;
    let mut row = binding();
    task(&state, &row);
    let ip = live_vm(&state, &row);
    let app = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = app.local_addr().unwrap().port();
    applied(&mut row, ip, port);
    let registered = snapshot(&row);
    drop(app);
    let worker = helper(
        &mut state,
        dir.path(),
        vec![
            (Operation::Inspect, Some(registered.clone())),
            (Operation::Inspect, Some(registered.clone())),
        ],
    )
    .await;
    let id = RequestId(Uuid::new_v4());
    let waiting = save(
        State(state.clone()),
        Path((row.project_id, row.id)),
        Extension(id),
        Caller::default(),
        ValidatedJson(row.spec.clone()),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(waiting.state, SandboxTunnelState::Waiting);
    assert!(!waiting.app_ready);
    assert!(waiting.applied_target.is_none() && waiting.connector_id.is_none());
    assert_eq!(
        waiting.error_code.as_deref(),
        Some("sandbox_preview_app_not_ready")
    );
    worker.await.unwrap();
    let app = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .unwrap();
    let worker = helper(
        &mut state,
        dir.path(),
        vec![
            (Operation::Inspect, Some(registered.clone())),
            (
                Operation::ManageFirecrabTunnel {
                    request: firecrab_tunnel::Request::Save {
                        subdomain: row.spec.subdomain.clone(),
                        target: resolved(row.applied_target.as_ref().unwrap()),
                    },
                },
                None,
            ),
        ],
    )
    .await;
    let uncertain = publish(
        State(state.clone()),
        Path(row.id),
        Extension(id),
        Caller::default(),
    )
    .await
    .unwrap()
    .0;
    worker.await.unwrap();
    assert_eq!(uncertain.state, SandboxTunnelState::NeedsReconciliation);
    assert_eq!(
        uncertain.error_code.as_deref(),
        Some("sandbox_preview_publish_uncertain")
    );
    assert_eq!(uncertain.applied_target, row.applied_target);
    assert!(
        state
            .store
            .sandbox_vm_has_live_tunnels(row.runtime.vm_id)
            .unwrap()
    );
    let worker = helper(
        &mut state,
        dir.path(),
        vec![(Operation::Inspect, Some(registered))],
    )
    .await;
    let recovered = reconcile(
        State(state.clone()),
        Path(row.id),
        Extension(id),
        Caller::default(),
    )
    .await
    .unwrap()
    .0;
    worker.await.unwrap();
    assert_eq!(recovered.state, SandboxTunnelState::Connected);
    assert_eq!(recovered.connector_id, row.connector_id);
    assert_eq!(
        recovered.public_https,
        SandboxTunnelVerification::Unverified
    );
    drop(app);
}

#[tokio::test]
async fn owned_nginx_routes_reject_peer_vm_and_missing_routes() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    let mut row = binding();
    task(&state, &row);
    let ip = live_vm(&state, &row);
    row.spec.target = SandboxTunnelTarget::Nginx {
        host: "preview.example".into(),
    };
    let mut observed = snapshot(&row);
    observed.config = Some(serde_json::from_value(json!({"routes":[{"host":"preview.example","pathPrefix":"/","upstreams":[format!("{ip}:8080"),format!("{ip}:8080")] }]})).unwrap());
    let id = RequestId(Uuid::new_v4());
    let (resolved, upstreams) = target_addresses(&state, id, &row, &observed).unwrap();
    assert_eq!(
        resolved,
        SandboxTunnelResolvedTarget::Nginx {
            host: "preview.example".into()
        }
    );
    assert_eq!(
        upstreams,
        vec![format!("{ip}:8080").parse::<SocketAddr>().unwrap()]
    );
    observed.config.as_mut().unwrap().routes[0]
        .upstreams
        .push("127.0.0.99:8080".parse().unwrap());
    assert_eq!(
        target_addresses(&state, id, &row, &observed)
            .unwrap_err()
            .code(),
        "sandbox_preview_route_not_owned"
    );
    observed.config.as_mut().unwrap().routes.clear();
    assert_eq!(
        target_addresses(&state, id, &row, &observed)
            .unwrap_err()
            .code(),
        "sandbox_preview_route_not_owned"
    );
    row.runtime.runtime_id = Uuid::new_v4();
    assert_eq!(
        target_addresses(&state, id, &row, &observed)
            .unwrap_err()
            .code(),
        "sandbox_preview_runtime_changed"
    );
}

#[tokio::test]
async fn public_verification_rejects_unsafe_paths_and_missing_hash_without_outbound_requests() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = state(dir.path()).await;
    let mut row = binding();
    row.state = SandboxTunnelState::Connected;
    task(&state, &row);
    state.store.save_sandbox_tunnel(&row).unwrap();
    let paths = [
        "relative",
        "//peer.example",
        "/bad#fragment",
        "/bad\\path",
        "/bad path",
    ];
    let mut replies = vec![(Operation::Inspect, Some(snapshot(&row))); paths.len() + 2];
    let worker = helper(&mut state, dir.path(), std::mem::take(&mut replies)).await;
    for path in paths {
        let error = verify(
            State(state.clone()),
            Path(row.id),
            Extension(RequestId(Uuid::new_v4())),
            Caller::default(),
            ValidatedJson(SandboxTunnelVerify {
                path: path.into(),
                body_sha256: Some("0".repeat(64)),
                web_socket: false,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), "sandbox_preview_invalid_verification");
    }
    for digest in [None, Some("not-a-sha256".into())] {
        let error = verify(
            State(state.clone()),
            Path(row.id),
            Extension(RequestId(Uuid::new_v4())),
            Caller::default(),
            ValidatedJson(SandboxTunnelVerify {
                path: "/health".into(),
                body_sha256: digest,
                web_socket: false,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), "sandbox_preview_body_digest_required");
    }
    worker.await.unwrap();
    assert_eq!(state.store.sandbox_tunnel(row.id).unwrap().unwrap(), row);
}

// Linux routes 127/8 without adding host interface aliases.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn explicit_successor_moves_one_url_only_after_new_target_and_ingress_retirement_are_observed()
 {
    let dir = tempfile::tempdir().unwrap();
    let mut state = state(dir.path()).await;
    let app = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = app.local_addr().unwrap().port();
    let mut old = binding();
    task(&state, &old);
    let ip = live_vm(&state, &old);
    applied(&mut old, ip, port);
    state.store.save_sandbox_tunnel(&old).unwrap();
    let mut next = successor(&old);
    task(&state, &next);
    let next_ip = live_vm(&state, &next);
    applied(&mut next, next_ip, port);
    next.connector_id = old.connector_id;
    let mut new_snapshot = snapshot(&next);
    new_snapshot.ingress_workers = Some(vec![]);
    let worker = helper(
        &mut state,
        dir.path(),
        vec![
            (Operation::Inspect, Some(snapshot(&old))),
            (Operation::Inspect, Some(snapshot(&old))),
            (
                Operation::ManageFirecrabTunnel {
                    request: firecrab_tunnel::Request::Save {
                        subdomain: old.spec.subdomain.clone(),
                        target: resolved(next.applied_target.as_ref().unwrap()),
                    },
                },
                Some(new_snapshot),
            ),
        ],
    )
    .await;
    let request = SandboxTunnelRetarget {
        id: next.id,
        task_id: next.task_id,
        target: next.spec.target.clone(),
    };
    let id = RequestId(Uuid::new_v4());
    let result = retarget(
        State(state.clone()),
        Path(old.id),
        Extension(id),
        Caller::default(),
        ValidatedJson(request.clone()),
    )
    .await
    .unwrap()
    .0;
    worker.await.unwrap();
    assert_eq!(result.state, SandboxTunnelState::Connected);
    assert_eq!(
        (result.task_id, result.attempt_id, result.runtime),
        (next.task_id, next.attempt_id, next.runtime)
    );
    assert_eq!(result.spec.subdomain, old.spec.subdomain);
    assert_eq!(
        result.public_url,
        snapshot(&old).firecrab_tunnels[0].public_url.clone().into()
    );
    let original = state.store.sandbox_tunnel(old.id).unwrap().unwrap();
    assert_eq!(
        (original.task_id, original.attempt_id, original.runtime),
        (old.task_id, old.attempt_id, old.runtime)
    );
    assert!(original.handoff_confirmed_at.is_some());
    assert!(
        !state
            .store
            .sandbox_vm_has_live_tunnels(old.runtime.vm_id)
            .unwrap()
    );
    assert!(
        state
            .store
            .sandbox_vm_has_live_tunnels(next.runtime.vm_id)
            .unwrap()
    );
    assert_eq!(
        state
            .store
            .sandbox_tunnel_claim(&old.spec.subdomain)
            .unwrap(),
        Some(next.id)
    );
    // An explicit retry inspects the existing successor; it does not contact helper again.
    assert_eq!(
        retarget(
            State(state.clone()),
            Path(old.id),
            Extension(id),
            Caller::default(),
            ValidatedJson(request.clone())
        )
        .await
        .unwrap()
        .0,
        result
    );
    let mut conflict = request;
    conflict.task_id = Uuid::new_v4();
    assert_eq!(
        retarget(
            State(state.clone()),
            Path(old.id),
            Extension(id),
            Caller::default(),
            ValidatedJson(conflict)
        )
        .await
        .unwrap_err()
        .code(),
        "sandbox_preview_identity_conflict"
    );
    assert_eq!(
        stop(State(state), Path(old.id), Extension(id), Caller::default())
            .await
            .unwrap_err()
            .code(),
        "sandbox_preview_deleted"
    );
}

// Linux routes 127/8 without adding host interface aliases.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn uncertain_stop_and_delete_keep_vm_hold_until_observed_retirement() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = state(dir.path()).await;
    let app = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let mut row = binding();
    task(&state, &row);
    let ip = live_vm(&state, &row);
    applied(&mut row, ip, app.local_addr().unwrap().port());
    state.store.save_sandbox_tunnel(&row).unwrap();
    let mut inactive = snapshot(&row);
    inactive.firecrab_tunnels[0].connected = false;
    inactive.quick_tunnels[0].unit.active_state = "inactive".into();
    inactive.quick_tunnels[0].unit.main_pid = 0;
    let mut removed = inactive.clone();
    removed.firecrab_tunnels.clear();
    let worker = helper(
        &mut state,
        dir.path(),
        vec![
            (Operation::Inspect, Some(snapshot(&row))),
            (
                Operation::ManageQuickTunnel {
                    request: QuickTunnelRequest::Control {
                        id: row.connector_id.unwrap(),
                        service_action: ServiceAction::Stop,
                    },
                },
                None,
            ),
            (Operation::Inspect, Some(inactive.clone())),
            (Operation::Inspect, Some(inactive)),
            (
                Operation::ManageFirecrabTunnel {
                    request: firecrab_tunnel::Request::Delete {
                        subdomain: row.spec.subdomain.clone(),
                    },
                },
                None,
            ),
            (Operation::Inspect, Some(removed)),
        ],
    )
    .await;
    let id = RequestId(Uuid::new_v4());
    let uncertain = stop(
        State(state.clone()),
        Path(row.id),
        Extension(id),
        Caller::default(),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(uncertain.state, SandboxTunnelState::Stopping);
    assert_eq!(
        uncertain.error_code.as_deref(),
        Some("sandbox_preview_stop_uncertain")
    );
    assert!(
        state
            .store
            .sandbox_vm_has_live_tunnels(row.runtime.vm_id)
            .unwrap()
    );
    let stopped = reconcile(
        State(state.clone()),
        Path(row.id),
        Extension(id),
        Caller::default(),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(stopped.state, SandboxTunnelState::Stopped);
    assert!(
        !state
            .store
            .sandbox_vm_has_live_tunnels(row.runtime.vm_id)
            .unwrap()
    );
    let uncertain = delete(
        State(state.clone()),
        Path(row.id),
        Extension(id),
        Caller::default(),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(uncertain.state, SandboxTunnelState::Deleting);
    assert_eq!(
        uncertain.error_code.as_deref(),
        Some("sandbox_preview_delete_uncertain")
    );
    assert!(
        state
            .store
            .sandbox_vm_has_live_tunnels(row.runtime.vm_id)
            .unwrap()
    );
    let deleted = reconcile(
        State(state.clone()),
        Path(row.id),
        Extension(id),
        Caller::default(),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(deleted.state, SandboxTunnelState::Deleted);
    assert!(deleted.local_removed);
    assert_eq!(
        deleted.remote_retirement,
        SandboxTunnelRetirement::OperatorRequired
    );
    assert_eq!(
        deleted.public_shutdown,
        SandboxTunnelVerification::Unverified
    );
    assert!(
        !state
            .store
            .sandbox_vm_has_live_tunnels(row.runtime.vm_id)
            .unwrap()
    );
    worker.await.unwrap();
}

#[tokio::test]
async fn save_rejects_identity_takeover_claim_collision_and_missing_runtime_without_helper_changes()
{
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    let row = binding();
    task(&state, &row);
    state.store.save_sandbox_tunnel(&row).unwrap();
    let id = RequestId(Uuid::new_v4());
    for (key, spec, code) in [
        (
            Uuid::nil(),
            row.spec.clone(),
            "sandbox_preview_invalid_target",
        ),
        (
            Uuid::new_v4(),
            row.spec.clone(),
            "sandbox_preview_address_claimed",
        ),
        (
            row.id,
            SandboxTunnelSpec {
                subdomain: "changed-preview".into(),
                ..row.spec.clone()
            },
            "sandbox_preview_identity_conflict",
        ),
        (
            Uuid::new_v4(),
            SandboxTunnelSpec {
                subdomain: "invalid/subdomain".into(),
                ..row.spec.clone()
            },
            "sandbox_preview_invalid_target",
        ),
    ] {
        assert_eq!(
            save(
                State(state.clone()),
                Path((row.project_id, key)),
                Extension(id),
                Caller::default(),
                ValidatedJson(spec)
            )
            .await
            .unwrap_err()
            .code(),
            code
        );
    }
    assert_eq!(state.store.sandbox_tunnel(row.id).unwrap().unwrap(), row);
    let mut saved = state.store.sandbox_task(row.task_id).unwrap().unwrap();
    saved.task.id = Uuid::new_v4();
    saved.task.attempt_id = Uuid::new_v4();
    saved.task.runtime = None;
    saved.guest = None;
    state.store.save_sandbox_task(&saved).unwrap();
    let mut spec = row.spec.clone();
    spec.task_id = saved.task.id;
    spec.subdomain = "different-preview".into();
    assert_eq!(
        save(
            State(state.clone()),
            Path((row.project_id, Uuid::new_v4())),
            Extension(id),
            Caller::default(),
            ValidatedJson(spec)
        )
        .await
        .unwrap_err()
        .code(),
        "sandbox_preview_runtime_missing"
    );
}

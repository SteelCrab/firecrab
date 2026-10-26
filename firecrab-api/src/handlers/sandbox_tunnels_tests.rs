use super::*;
use firecrab_helper_protocol::pipe::{
    QuickTunnelSnapshot, QuickTunnelTarget, UnitStatus, quick_tunnel_unit,
};
use serde_json::json;

fn binding() -> SandboxTunnelBinding {
    let runtime = SandboxRuntimeRef {
        vm_id: Uuid::new_v4(),
        runtime_id: Uuid::new_v4(),
    };
    let task_id = Uuid::new_v4();
    SandboxTunnelBinding {
        id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        task_id,
        attempt_id: Uuid::new_v4(),
        runtime,
        owner: SandboxPrincipal::Local,
        spec: SandboxTunnelSpec {
            task_id,
            subdomain: "sandbox-preview".into(),
            target: SandboxTunnelTarget::VmApp {
                runtime,
                port: 8080,
            },
        },
        state: SandboxTunnelState::Connecting,
        app_ready: true,
        connector_id: Some(Uuid::new_v4()),
        applied_target: Some(SandboxTunnelResolvedTarget::Apps {
            upstreams: vec!["172.30.1.2:8080".parse().unwrap()],
        }),
        public_url: None,
        helper_connected: false,
        public_https: Default::default(),
        web_socket_handshake: Default::default(),
        public_shutdown: Default::default(),
        remote_retirement: SandboxTunnelRetirement::NotRequested,
        local_removed: false,
        created_at: 1,
        updated_at: 1,
        error_code: None,
        previous_binding_id: None,
        successor_binding_id: None,
        handoff_confirmed_at: None,
    }
}

#[test]
fn preview_mapping_reserves_only_its_vm_until_confirmed_stop() {
    use SandboxTunnelState::*;
    for (status, connector, target, blocked) in [
        (Waiting, true, false, true),
        (Connecting, true, true, true),
        (Connected, true, true, true),
        (Stopping, true, true, true),
        (Deleting, false, true, true),
        (NeedsReconciliation, true, false, true),
        (NeedsReconciliation, false, true, true),
        (Waiting, false, false, false),
        (Stopped, true, true, false),
        (Deleted, true, true, false),
        (Superseded, true, true, false),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.db");
        let store = crate::persistence::Store::open(&path).unwrap();
        let mut row = binding();
        row.state = status;
        if !connector {
            row.connector_id = None;
        }
        if !target {
            row.applied_target = None;
        }
        store.save_sandbox_tunnel(&row).unwrap();
        drop(store);
        let store = crate::persistence::Store::open(&path).unwrap();
        assert_eq!(
            store
                .sandbox_vm_has_live_tunnels(row.runtime.vm_id)
                .unwrap(),
            blocked,
            "{status:?} connector={connector} target={target}",
        );
        assert!(!store.sandbox_vm_has_live_tunnels(Uuid::new_v4()).unwrap());
    }
}

#[tokio::test]
async fn preview_mapping_blocks_vm_stop_and_delete_before_resource_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(directory.path()).await;
    let mut row = binding();
    row.state = SandboxTunnelState::NeedsReconciliation;
    row.helper_connected = false;
    state.store.save_sandbox_tunnel(&row).unwrap();
    let request = RequestId(Uuid::new_v4());
    let error = crate::handlers::vms::stop_vm(
        State(state.clone()),
        Extension(request),
        Path(row.runtime.vm_id.to_string()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "sandbox_preview_in_use");
    let error = crate::handlers::vms::delete_vm(
        State(state.clone()),
        Extension(request),
        Path(row.runtime.vm_id.to_string()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "sandbox_preview_in_use");
    assert_eq!(state.store.sandbox_tunnel(row.id).unwrap().unwrap(), row);
    row.state = SandboxTunnelState::Stopped;
    state.store.save_sandbox_tunnel(&row).unwrap();
    let error = crate::handlers::vms::delete_vm(
        State(state),
        Extension(request),
        Path(row.runtime.vm_id.to_string()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "not_found");
}
fn snapshot(binding: &SandboxTunnelBinding) -> Snapshot {
    let connector = binding.connector_id.unwrap();
    let mut snapshot: Snapshot = serde_json::from_value(
        json!({"config":null,"units":[],"nginxAvailable":true,"cloudflaredAvailable":true}),
    )
    .unwrap();
    snapshot.firecrab_tunnels.push(firecrab_tunnel::Address {
        subdomain: binding.spec.subdomain.clone(),
        public_url: "https://sandbox-preview.firecrab.dev".into(),
        target: resolved(binding.applied_target.as_ref().unwrap()),
        connector_id: connector,
        connected: true,
        error: None,
    });
    snapshot.quick_tunnels.push(QuickTunnelSnapshot {
        id: connector,
        name: "Preview".into(),
        target: match binding.applied_target.as_ref().unwrap() {
            SandboxTunnelResolvedTarget::Apps { upstreams } => QuickTunnelTarget::Apps {
                upstreams: upstreams.clone(),
            },
            SandboxTunnelResolvedTarget::Nginx { host } => {
                QuickTunnelTarget::Routes { host: host.clone() }
            }
        },
        unit: UnitStatus {
            unit: quick_tunnel_unit(connector),
            load_state: "loaded".into(),
            active_state: "active".into(),
            sub_state: "running".into(),
            main_pid: 42,
        },
        public_url: Some("https://connector.trycloudflare.com".into()),
        error: None,
    });
    snapshot
}

#[test]
fn helper_connection_does_not_fabricate_public_or_websocket_evidence() {
    let mut binding = binding();
    let snapshot = snapshot(&binding);
    observe(&mut binding, &snapshot);
    assert_eq!(binding.state, SandboxTunnelState::Connected);
    assert!(binding.helper_connected);
    assert_eq!(binding.public_https, SandboxTunnelVerification::Unverified);
    assert_eq!(
        binding.web_socket_handshake,
        SandboxTunnelVerification::Unverified
    );
    assert_eq!(
        binding.public_shutdown,
        SandboxTunnelVerification::Unverified
    );
}
#[test]
fn expired_connections_and_cross_host_claims_require_explicit_operator_repair() {
    for (message, code) in [
        (
            "The connection token is invalid or revoked",
            "sandbox_preview_token_expired_or_revoked",
        ),
        (
            "This subdomain belongs to another host",
            "sandbox_preview_connection_claim_conflict",
        ),
    ] {
        let mut binding = binding();
        let mut snapshot = snapshot(&binding);
        snapshot.firecrab_tunnels[0].connected = false;
        snapshot.firecrab_tunnels[0].error = Some(message.into());
        observe(&mut binding, &snapshot);
        assert_eq!(binding.state, SandboxTunnelState::NeedsReconciliation);
        assert_eq!(binding.error_code.as_deref(), Some(code));
        assert!(!binding.helper_connected);
        assert_eq!(binding.public_https, SandboxTunnelVerification::Unverified);
    }
}

#[test]
fn another_pipe_edit_or_shared_connector_cannot_be_stopped_or_deleted() {
    let binding = binding();
    let mut snapshot = snapshot(&binding);
    let id = RequestId(Uuid::new_v4());
    require_applied(&snapshot, &binding, id).unwrap();
    let mut peer = snapshot.firecrab_tunnels[0].clone();
    peer.subdomain = "unrelated".into();
    snapshot.firecrab_tunnels.push(peer);
    assert!(require_applied(&snapshot, &binding, id).is_err());
    snapshot.firecrab_tunnels.pop();
    snapshot.firecrab_tunnels[0].target = Target::Nginx {
        host: "changed.example".into(),
    };
    assert!(require_applied(&snapshot, &binding, id).is_err());
    let mut observed = binding;
    observe(&mut observed, &snapshot);
    assert_eq!(observed.state, SandboxTunnelState::NeedsReconciliation);
}

#[test]
fn stop_preserves_saved_address_and_delete_reports_remote_operator_work() {
    let mut binding = binding();
    let mut snapshot = snapshot(&binding);
    let url = snapshot.firecrab_tunnels[0].public_url.clone();
    binding.state = SandboxTunnelState::Stopping;
    snapshot.quick_tunnels[0].unit.active_state = "inactive".into();
    snapshot.quick_tunnels[0].unit.main_pid = 0;
    observe(&mut binding, &snapshot);
    assert_eq!(binding.state, SandboxTunnelState::Stopped);
    assert_eq!(binding.public_url.as_deref(), Some(url.as_str()));
    assert!(!binding.local_removed);
    binding.state = SandboxTunnelState::Deleting;
    snapshot.firecrab_tunnels.clear();
    observe(&mut binding, &snapshot);
    assert_eq!(binding.state, SandboxTunnelState::Deleted);
    assert!(binding.local_removed);
    assert_eq!(
        binding.remote_retirement,
        SandboxTunnelRetirement::OperatorRequired
    );
    assert_eq!(
        binding.public_shutdown,
        SandboxTunnelVerification::Unverified
    );
}

#[test]
fn durable_address_claims_and_attempt_ownership_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("preview.db");
    let mut row = binding();
    let store = crate::persistence::Store::open(&path).unwrap();
    store.save_sandbox_tunnel(&row).unwrap();
    let mut peer = row.clone();
    peer.id = Uuid::new_v4();
    assert!(store.save_sandbox_tunnel(&peer).is_err());
    let mut changed = row.clone();
    changed.runtime.runtime_id = Uuid::new_v4();
    assert!(store.save_sandbox_tunnel(&changed).is_err());
    let mut changed = row.clone();
    changed.owner = SandboxPrincipal::Host(Uuid::new_v4());
    assert!(store.save_sandbox_tunnel(&changed).is_err());
    drop(store);
    let store = crate::persistence::Store::open(&path).unwrap();
    assert_eq!(store.sandbox_tunnel(row.id).unwrap().unwrap(), row);
    assert!(
        store
            .sandbox_project_has_live_tunnels(row.project_id)
            .unwrap()
    );
    row.state = SandboxTunnelState::Deleted;
    store.save_sandbox_tunnel(&row).unwrap();
    assert!(
        !store
            .sandbox_project_has_live_tunnels(row.project_id)
            .unwrap()
    );
    assert_eq!(
        store.sandbox_tunnel_claim(&row.spec.subdomain).unwrap(),
        None
    );
}

#[test]
fn credential_input_cannot_enter_preview_contract() {
    for value in [
        json!({"taskId":Uuid::new_v4(),"subdomain":"preview","target":{"kind":"nginx","host":"app.example"},"token":"private"}),
        json!({"taskId":Uuid::new_v4(),"subdomain":"preview","target":{"kind":"vm_app","runtime":{"vmId":Uuid::new_v4(),"runtimeId":Uuid::new_v4()},"port":8080,"ip":"127.0.0.1"}}),
    ] {
        assert!(serde_json::from_value::<SandboxTunnelSpec>(value).is_err());
    }
}

async fn state(root: &std::path::Path) -> AppState {
    AppState::with_db_file(
        crate::templates::TemplateRegistry::from_specs(root, std::iter::empty()).unwrap(),
        root.join("state.db"),
    )
    .await
    .unwrap()
}
fn task(state: &AppState, binding: &SandboxTunnelBinding) {
    let spec = serde_json::json!({"name":"Preview project","sourcePath":null,"vmId":binding.runtime.vm_id,"selection":null,"instructions":""});
    let project: SandboxProject = serde_json::from_value(
        json!({"id":binding.project_id,"owner":"local","spec":spec,"createdAt":1}),
    )
    .unwrap();
    state.store.save_sandbox_project(&project).unwrap();
    let task:crate::ai_sandbox::SavedTask=serde_json::from_value(json!({"caller":{"principal":"local","operator":true},"project":spec,"workspace":null,"guest":binding.runtime,"guest_pending":false,"guest_staged":false,"task":{"id":binding.task_id,"projectId":binding.project_id,"attemptId":binding.attempt_id,"runtime":binding.runtime,"request":{"kind":"code","input":"App task","selection":null,"retryOf":null},"selection":{"connectionId":Uuid::new_v4(),"model":"fixture"},"state":"result","createdAt":1,"updatedAt":2,"result":"Ready","errorCode":null,"error":null,"events":[],"changes":[]}})).unwrap();
    state.store.save_sandbox_task(&task).unwrap();
}

#[tokio::test]
async fn rest_authorization_is_project_scoped_and_delegates_cannot_publish_or_stop() {
    use axum::{
        body::Body,
        extract::ConnectInfo,
        http::{Method, Request},
    };
    use tower::ServiceExt;
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    let binding = binding();
    task(&state, &binding);
    state.store.save_sandbox_tunnel(&binding).unwrap();
    let host = Uuid::new_v4();
    state
        .store
        .save_sandbox_access(
            host,
            &SandboxAccessPolicy {
                grants: vec![SandboxResourceGrant {
                    resource: SandboxResource::Project {
                        id: binding.project_id,
                    },
                    permissions: vec![SandboxPermission::Read, SandboxPermission::Execute],
                }],
            },
        )
        .unwrap();
    let router = crate::server::build_router(
        state.clone(),
        &crate::server::HttpConfig {
            bind_addr: "127.0.0.1:5523".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 8,
            request_timeout: Duration::from_secs(10),
            static_root: None,
        },
    );
    for (method, path, status) in [
        (
            Method::GET,
            format!("/api/ai-sandbox/tunnels/{}", binding.id),
            StatusCode::OK,
        ),
        (
            Method::POST,
            format!("/api/ai-sandbox/tunnels/{}/stop", binding.id),
            StatusCode::FORBIDDEN,
        ),
        (
            Method::POST,
            format!("/api/ai-sandbox/tunnels/{}/publish", binding.id),
            StatusCode::FORBIDDEN,
        ),
        (
            Method::GET,
            "/api/ai-sandbox/tunnel-addresses".into(),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1:5523")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(crate::tls::AuthenticatedHost { host_id: host }));
        assert_eq!(
            router.clone().oneshot(request).await.unwrap().status(),
            status
        );
    }
    state
        .store
        .save_sandbox_access(host, &SandboxAccessPolicy::default())
        .unwrap();
    let caller: Caller =
        serde_json::from_value(json!({"principal":{"host":host},"operator":false})).unwrap();
    assert!(authorize(&state, RequestId(Uuid::new_v4()), caller, &binding, false).is_err());
}

#[tokio::test]
async fn environment_preview_requires_the_admitted_generation_and_ready_declared_service() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    let binding = binding();
    let reference = SandboxEnvironmentRef {
        id: Uuid::new_v4(),
        generation: Uuid::new_v4(),
        role: "backend".into(),
    };
    let mut environment:SandboxEnvironment=serde_json::from_value(json!({"id":reference.id,"projectId":binding.project_id,"owner":"local","generation":reference.generation,"spec":{"version":1,"subnetCidr":"172.29.80.0/24","roles":[{"name":"backend","template":{"name":"ubuntu","version":"1","kernelSha256":"1".repeat(64),"rootfsSha256":"2".repeat(64),"initrdSha256":null,"bootArgsSha256":"3".repeat(64)},"cpu":1,"memoryMib":512,"diskGb":4,"services":[{"name":"api","port":8080,"readiness":true}],"executionMode":{"kind":"persistent_workspace"}}]},"networkId":Uuid::new_v4(),"roles":[{"role":"backend","vmId":binding.runtime.vm_id,"securityGroupId":Uuid::new_v4(),"runtime":binding.runtime,"ipv4":"172.29.80.2","agent":"ready","network":"ready","application":"ready","services":[{"name":"api","address":"172.29.80.2","port":8080,"ready":true}],"error":null}],"state":"ready","createdAt":1,"updatedAt":2,"expiresAt":null,"exportedAt":null,"error":null})).unwrap();
    state
        .store
        .reserve_sandbox_environment(&environment)
        .unwrap();
    let id = RequestId(Uuid::new_v4());
    assert_eq!(
        environment_port(&state, id, &binding, &reference, Some("api"), None).unwrap(),
        8080
    );
    assert!(environment_port(&state, id, &binding, &reference, Some("db"), None).is_err());
    assert!(environment_port(&state, id, &binding, &reference, None, Some(5432)).is_err());
    let mut stale = reference.clone();
    stale.generation = Uuid::new_v4();
    assert!(environment_port(&state, id, &binding, &stale, Some("api"), None).is_err());
    environment.roles[0].services[0].ready = false;
    state.store.save_sandbox_environment(&environment).unwrap();
    assert!(environment_port(&state, id, &binding, &reference, Some("api"), None).is_err());
}

#[tokio::test]
async fn active_preview_prevents_project_and_result_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    let binding = binding();
    task(&state, &binding);
    state.store.save_sandbox_tunnel(&binding).unwrap();
    let retained = state.sandbox.root.join(binding.project_id.to_string());
    std::fs::create_dir_all(&retained).unwrap();
    std::fs::write(retained.join("result"), "exported evidence").unwrap();
    let result = crate::handlers::ai_sandbox::delete_project(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Caller::default(),
        Path(binding.project_id),
    )
    .await;
    assert!(result.is_err());
    assert!(
        state
            .store
            .sandbox_project(binding.project_id)
            .unwrap()
            .is_some()
    );
    assert!(state.store.sandbox_task(binding.task_id).unwrap().is_some());
    assert_eq!(
        std::fs::read_to_string(retained.join("result")).unwrap(),
        "exported evidence"
    );
    assert_eq!(
        state.store.sandbox_tunnel(binding.id).unwrap().unwrap(),
        binding
    );
}
#[test]
fn successor_claim_transfer_is_atomic_and_never_rewrites_original_attempt_or_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::persistence::Store::open(&dir.path().join("state.db")).unwrap();
    let original = binding();
    store.save_sandbox_tunnel(&original).unwrap();
    let mut old = original.clone();
    let mut next = original.clone();
    next.id = Uuid::new_v4();
    next.task_id = Uuid::new_v4();
    next.spec.task_id = next.task_id;
    next.attempt_id = Uuid::new_v4();
    next.runtime.runtime_id = Uuid::new_v4();
    next.previous_binding_id = Some(old.id);
    old.state = SandboxTunnelState::Superseded;
    old.successor_binding_id = Some(next.id);
    old.helper_connected = false;
    let mut collision = next.clone();
    collision.spec.subdomain = "unrelated-peer".into();
    collision.project_id = Uuid::new_v4();
    collision.previous_binding_id = None;
    store.save_sandbox_tunnel(&collision).unwrap();
    assert!(store.supersede_sandbox_tunnel(&old, &next).is_err());
    assert_eq!(
        store.sandbox_tunnel_claim(&old.spec.subdomain).unwrap(),
        Some(original.id)
    );
    assert_eq!(
        store.sandbox_tunnel(old.id).unwrap().unwrap().state,
        original.state
    );
    next.id = Uuid::new_v4();
    old.successor_binding_id = Some(next.id);
    store.supersede_sandbox_tunnel(&old, &next).unwrap();
    assert_eq!(
        store.sandbox_tunnel_claim(&old.spec.subdomain).unwrap(),
        Some(next.id)
    );
    let saved = store.sandbox_tunnel(old.id).unwrap().unwrap();
    assert_eq!(saved.runtime, original.runtime);
    assert_eq!(saved.attempt_id, original.attempt_id);
    assert_eq!(saved.task_id, original.task_id);
    assert_eq!(saved.state, SandboxTunnelState::Superseded);
    assert!(store.save_sandbox_tunnel(&original).is_err());
    store.confirm_sandbox_tunnel_handoff(&next).unwrap();
    let mut deleted = next.clone();
    deleted.state = SandboxTunnelState::Deleted;
    store.save_sandbox_tunnel(&deleted).unwrap();
    assert!(
        !store
            .sandbox_project_has_live_tunnels(original.project_id)
            .unwrap()
    );
}

fn successor(previous: &SandboxTunnelBinding) -> SandboxTunnelBinding {
    let mut next = previous.clone();
    next.id = Uuid::new_v4();
    next.task_id = Uuid::new_v4();
    next.spec.task_id = next.task_id;
    next.attempt_id = Uuid::new_v4();
    next.runtime = SandboxRuntimeRef {
        vm_id: Uuid::new_v4(),
        runtime_id: Uuid::new_v4(),
    };
    next.spec.target = SandboxTunnelTarget::VmApp {
        runtime: next.runtime,
        port: 8080,
    };
    next.previous_binding_id = Some(previous.id);
    next.successor_binding_id = None;
    next.handoff_confirmed_at = None;
    next.connector_id = None;
    next.applied_target = None;
    next.state = SandboxTunnelState::Waiting;
    next
}

fn transfer(
    store: &crate::persistence::Store,
    previous: &SandboxTunnelBinding,
) -> SandboxTunnelBinding {
    let next = successor(previous);
    let mut old = previous.clone();
    old.state = SandboxTunnelState::Superseded;
    old.successor_binding_id = Some(next.id);
    store.supersede_sandbox_tunnel(&old, &next).unwrap();
    next
}

#[tokio::test]
async fn failed_first_handoff_inspection_reserves_both_vms_across_restart() {
    let directory = tempfile::tempdir().unwrap();
    let mut state = state(directory.path()).await;
    state.network =
        crate::network::NetworkClient::with_socket_path(directory.path().join("missing.sock"));
    let old = binding();
    task(&state, &old);
    state.store.save_sandbox_tunnel(&old).unwrap();
    let mut next = transfer(&state.store, &old);
    task(&state, &next);
    let id = RequestId(Uuid::new_v4());
    publish_successor(&state, id, &mut next).await.unwrap();
    assert_eq!(next.state, SandboxTunnelState::NeedsReconciliation);
    assert!(next.connector_id.is_none() && next.applied_target.is_none());
    assert_eq!(
        next.error_code.as_deref(),
        Some("sandbox_preview_retarget_pending")
    );
    drop(state);
    let state = self::state(directory.path()).await;
    for vm in [old.runtime.vm_id, next.runtime.vm_id] {
        assert!(state.store.sandbox_vm_has_live_tunnels(vm).unwrap());
        for result in [
            crate::handlers::vms::stop_vm(
                State(state.clone()),
                Extension(id),
                Path(vm.to_string()),
            )
            .await
            .map(|_| ()),
            crate::handlers::vms::delete_vm(
                State(state.clone()),
                Extension(id),
                Path(vm.to_string()),
            )
            .await
            .map(|_| ()),
        ] {
            assert_eq!(result.unwrap_err().code(), "sandbox_preview_in_use");
        }
    }
    let saved = state.store.sandbox_tunnel(old.id).unwrap().unwrap();
    assert_eq!(
        (saved.task_id, saved.attempt_id, saved.runtime),
        (old.task_id, old.attempt_id, old.runtime)
    );
    assert!(saved.handoff_confirmed_at.is_none());
    let caller: Caller =
        serde_json::from_value(json!({"principal":"local","operator":true})).unwrap();
    let error = delete(State(state.clone()), Path(next.id), Extension(id), caller)
        .await
        .unwrap_err();
    assert_eq!(error.code(), "sandbox_preview_reconcile_or_stop_handoff");
    assert!(
        state
            .store
            .sandbox_project_has_live_tunnels(old.project_id)
            .unwrap()
    );
}

#[test]
fn helper_target_and_retired_connector_evidence_are_both_required_for_handoff() {
    let old = binding();
    let mut next = successor(&old);
    next.connector_id = old.connector_id;
    next.applied_target = Some(SandboxTunnelResolvedTarget::Apps {
        upstreams: vec!["172.30.2.2:8080".parse().unwrap()],
    });
    let previous = [old.clone()];
    let expected = next.applied_target.as_ref();
    assert!(!handoff_observed(
        &next,
        &previous,
        expected,
        &snapshot(&old)
    ));
    let mut updated = snapshot(&next);
    updated.quick_tunnels[0].target = snapshot(&old).quick_tunnels[0].target.clone();
    assert!(!handoff_observed(&next, &previous, expected, &updated));
    updated.quick_tunnels[0].target = snapshot(&next).quick_tunnels[0].target.clone();
    assert!(handoff_observed(&next, &previous, expected, &updated));
    updated.ingress_graceful_drain = true;
    assert!(!handoff_observed(&next, &previous, expected, &updated));
    updated.ingress_workers = Some(vec![firecrab_helper_protocol::pipe::IngressWorker {
        draining: true,
        pid: 4,
        start_time: 10,
        boot_id: Uuid::new_v4().to_string(),
    }]);
    assert!(!handoff_observed(&next, &previous, expected, &updated));
    updated.ingress_workers = Some(vec![]);
    assert!(handoff_observed(&next, &previous, expected, &updated));
    next.connector_id = Some(Uuid::new_v4());
    let mut changed = snapshot(&next);
    assert!(!handoff_observed(&next, &previous, expected, &changed));
    changed
        .quick_tunnels
        .push(snapshot(&old).quick_tunnels[0].clone());
    assert!(!handoff_observed(&next, &previous, expected, &changed));
    changed.quick_tunnels[1].unit.active_state = "inactive".into();
    changed.quick_tunnels[1].unit.main_pid = 0;
    assert!(handoff_observed(&next, &previous, expected, &changed));
}

#[test]
fn confirmed_handoffs_allow_serial_retarget_without_releasing_the_pending_pair() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let store = crate::persistence::Store::open(&path).unwrap();
    let a = binding();
    store.save_sandbox_tunnel(&a).unwrap();
    let b = transfer(&store, &a);
    let c = successor(&b);
    let mut unconfirmed = b.clone();
    unconfirmed.state = SandboxTunnelState::Superseded;
    unconfirmed.successor_binding_id = Some(c.id);
    assert!(store.supersede_sandbox_tunnel(&unconfirmed, &c).is_err());
    assert_eq!(
        store.sandbox_tunnel_claim(&a.spec.subdomain).unwrap(),
        Some(b.id)
    );
    store.confirm_sandbox_tunnel_handoff(&b).unwrap();
    let c = transfer(&store, &b);
    drop(store);
    let store = crate::persistence::Store::open(&path).unwrap();
    assert!(!store.sandbox_vm_has_live_tunnels(a.runtime.vm_id).unwrap());
    assert!(store.sandbox_vm_has_live_tunnels(b.runtime.vm_id).unwrap());
    assert!(store.sandbox_vm_has_live_tunnels(c.runtime.vm_id).unwrap());
    for original in [&a, &b] {
        let saved = store.sandbox_tunnel(original.id).unwrap().unwrap();
        assert_eq!(
            (saved.task_id, saved.attempt_id, saved.runtime),
            (original.task_id, original.attempt_id, original.runtime)
        );
    }
    // Missing fields in older persisted JSON keep the predecessor hold in force.
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute("UPDATE ai_sandbox_tunnels SET spec=json_remove(spec,'$.handoffConfirmedAt') WHERE id=?1", [a.id.to_string()]).unwrap();
    assert!(store.sandbox_vm_has_live_tunnels(a.runtime.vm_id).unwrap());
    assert_eq!(store.sandbox_tunnel_handoff(&c).unwrap().len(), 2);
}

#[tokio::test]
async fn pending_handoff_can_stop_the_verified_predecessor_before_any_new_publish() {
    use firecrab_helper_protocol::framing::{read_frame, write_frame};
    use firecrab_helper_protocol::network::{
        NetworkRequest, NetworkRequestEnvelope, NetworkResponseEnvelope,
    };
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("handoff.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let mut state = state(directory.path()).await;
    state.network = crate::network::NetworkClient::with_socket_path(socket);
    let old = binding();
    state.store.save_sandbox_tunnel(&old).unwrap();
    let next = transfer(&state.store, &old);
    task(&state, &next);
    let mut stopped = snapshot(&old);
    stopped.firecrab_tunnels[0].connected = false;
    stopped.quick_tunnels[0].unit.active_state = "inactive".into();
    stopped.quick_tunnels[0].unit.main_pid = 0;
    let responses = [
        (Operation::Inspect, snapshot(&old)),
        (
            Operation::ManageQuickTunnel {
                request: QuickTunnelRequest::Control {
                    id: old.connector_id.unwrap(),
                    service_action: ServiceAction::Stop,
                },
            },
            stopped,
        ),
    ];
    let helper = tokio::spawn(async move {
        for (operation, snapshot) in responses {
            let (mut stream, _) = listener.accept().await.unwrap();
            let envelope: NetworkRequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert_eq!(
                envelope.request,
                NetworkRequest::Pipe { request: operation }
            );
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
    });
    let caller: Caller =
        serde_json::from_value(json!({"principal":"local","operator":true})).unwrap();
    let Json(stopped) = stop(
        State(state.clone()),
        Path(next.id),
        Extension(RequestId(Uuid::new_v4())),
        caller,
    )
    .await
    .unwrap();
    helper.await.unwrap();
    assert_eq!(stopped.state, SandboxTunnelState::Stopped);
    assert_eq!(stopped.applied_target, old.applied_target);
    assert!(
        state
            .store
            .sandbox_tunnel(old.id)
            .unwrap()
            .unwrap()
            .handoff_confirmed_at
            .is_some()
    );
    assert!(
        !state
            .store
            .sandbox_vm_has_live_tunnels(old.runtime.vm_id)
            .unwrap()
    );
    assert!(
        !state
            .store
            .sandbox_vm_has_live_tunnels(next.runtime.vm_id)
            .unwrap()
    );
}

#[tokio::test]
async fn stopped_preview_rejects_raw_connector_address_and_global_mutations() {
    use super::super::pipe;
    let directory = tempfile::tempdir().unwrap();
    let mut state = state(directory.path()).await;
    state.network =
        crate::network::NetworkClient::with_socket_path(directory.path().join("missing.sock"));
    let mut binding = binding();
    binding.state = SandboxTunnelState::Stopped;
    state.store.save_sandbox_tunnel(&binding).unwrap();
    let connector = binding.connector_id.unwrap();
    let id = RequestId(Uuid::new_v4());
    let target = json!({"type":"apps","upstreams":["172.30.1.2:8080"]});
    let config =
        json!({"routes":[{"host":"app.test","upstreams":["172.30.1.2:8080"]}],"tunnel":null});
    let mut quick_config = config.clone();
    quick_config["quickTunnel"] = json!({"upstreams":["172.30.1.2:8080"]});
    let results = [
        pipe::control_quick_tunnel(
            State(state.clone()),
            Path(connector),
            Extension(id),
            Json(serde_json::from_value(json!({"action":"start"})).unwrap()),
        )
        .await,
        pipe::control_quick_tunnel(
            State(state.clone()),
            Path(connector),
            Extension(id),
            Json(serde_json::from_value(json!({"action":"restart"})).unwrap()),
        )
        .await,
        pipe::save_quick_tunnel(
            State(state.clone()),
            Extension(id),
            Json(
                serde_json::from_value(json!({"id":connector,"name":"Edit","target":target}))
                    .unwrap(),
            ),
        )
        .await,
        pipe::delete_quick_tunnel(State(state.clone()), Path(connector), Extension(id)).await,
        pipe::save_firecrab_tunnel(
            State(state.clone()),
            Extension(id),
            Json(
                serde_json::from_value(json!({"subdomain":binding.spec.subdomain,"target":target}))
                    .unwrap(),
            ),
        )
        .await,
        pipe::delete_firecrab_tunnel(
            State(state.clone()),
            Path(binding.spec.subdomain.clone()),
            Extension(id),
        )
        .await,
        pipe::save_firecrab_tunnel(
            State(state.clone()),
            Extension(id),
            Json(serde_json::from_value(json!({"token":"a".repeat(64),"target":target})).unwrap()),
        )
        .await,
        pipe::apply(
            State(state.clone()),
            Extension(id),
            Json(serde_json::from_value(json!({"config":config,"credentials":null})).unwrap()),
        )
        .await,
        pipe::connect_quick_tunnel(
            State(state.clone()),
            Extension(id),
            Json(serde_json::from_value(json!({"config":quick_config})).unwrap()),
        )
        .await,
        pipe::control(
            State(state.clone()),
            Extension(id),
            Json(serde_json::from_value(json!({"service":"ingress","action":"restart"})).unwrap()),
        )
        .await,
        pipe::disable(State(state.clone()), Extension(id)).await,
    ];
    for result in results {
        assert_eq!(result.unwrap_err().code(), "sandbox_preview_claimed");
    }
    assert_eq!(
        state.store.sandbox_tunnel(binding.id).unwrap().unwrap(),
        binding
    );
    let error = pipe::control_quick_tunnel(
        State(state.clone()),
        Path(Uuid::new_v4()),
        Extension(id),
        Json(serde_json::from_value(json!({"action":"start"})).unwrap()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "pipe_unavailable");
    binding.state = SandboxTunnelState::Deleted;
    state.store.save_sandbox_tunnel(&binding).unwrap();
    assert!(!state.store.sandbox_tunnel_has_claims().unwrap());
    assert!(
        !state
            .store
            .sandbox_tunnel_claims_connector(connector)
            .unwrap()
    );
}

#[tokio::test]
async fn raw_start_waits_until_sandbox_stop_has_persisted_its_inactive_snapshot() {
    use firecrab_helper_protocol::framing::{read_frame, write_frame};
    use firecrab_helper_protocol::network::{
        NetworkRequest, NetworkRequestEnvelope, NetworkResponseEnvelope,
    };
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("stop-race.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let mut state = state(directory.path()).await;
    state.network = crate::network::NetworkClient::with_socket_path(socket);
    let binding = binding();
    task(&state, &binding);
    state.store.save_sandbox_tunnel(&binding).unwrap();
    let connector = binding.connector_id.unwrap();
    let mut stopped = snapshot(&binding);
    stopped.firecrab_tunnels[0].connected = false;
    stopped.quick_tunnels[0].unit.active_state = "inactive".into();
    stopped.quick_tunnels[0].unit.main_pid = 0;
    let (seen, observed) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let helper = tokio::spawn(async move {
        for (operation, response) in [
            (Operation::Inspect, snapshot(&binding)),
            (
                Operation::ManageQuickTunnel {
                    request: QuickTunnelRequest::Control {
                        id: connector,
                        service_action: ServiceAction::Stop,
                    },
                },
                stopped,
            ),
        ] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let envelope: NetworkRequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert_eq!(
                envelope.request,
                NetworkRequest::Pipe {
                    request: operation.clone()
                }
            );
            if matches!(operation, Operation::ManageQuickTunnel { .. }) {
                seen.send(()).unwrap();
                released.await.unwrap();
                write_frame(
                    &mut stream,
                    &NetworkResponseEnvelope {
                        version: firecrab_helper_protocol::PROTOCOL_VERSION,
                        request_id: envelope.request_id,
                        result: Ok(()),
                        pipe: Some(response),
                    },
                )
                .await
                .unwrap();
                break;
            }
            write_frame(
                &mut stream,
                &NetworkResponseEnvelope {
                    version: firecrab_helper_protocol::PROTOCOL_VERSION,
                    request_id: envelope.request_id,
                    result: Ok(()),
                    pipe: Some(response),
                },
            )
            .await
            .unwrap();
        }
    });
    let stop_state = state.clone();
    let binding_id = state
        .store
        .sandbox_tunnel_claim("sandbox-preview")
        .unwrap()
        .unwrap();
    let stopping = tokio::spawn(async move {
        let caller: Caller =
            serde_json::from_value(json!({"principal":"local","operator":true})).unwrap();
        stop(
            State(stop_state),
            Path(binding_id),
            Extension(RequestId(Uuid::new_v4())),
            caller,
        )
        .await
    });
    observed.await.unwrap();
    let raw_state = state.clone();
    let mut raw = tokio::spawn(async move {
        super::super::pipe::control_quick_tunnel(
            State(raw_state),
            Path(connector),
            Extension(RequestId(Uuid::new_v4())),
            Json(serde_json::from_value(json!({"action":"start"})).unwrap()),
        )
        .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut raw)
            .await
            .is_err()
    );
    release.send(()).unwrap();
    let Json(stopped) = stopping.await.unwrap().unwrap();
    assert_eq!(stopped.state, SandboxTunnelState::Stopped);
    assert_eq!(
        raw.await.unwrap().unwrap_err().code(),
        "sandbox_preview_claimed"
    );
    helper.await.unwrap();
    assert!(
        !state
            .store
            .sandbox_vm_has_live_tunnels(stopped.runtime.vm_id)
            .unwrap()
    );
}

#[test]
fn historical_connectors_remain_claimed_until_the_current_binding_is_deleted() {
    let directory = tempfile::tempdir().unwrap();
    let store = crate::persistence::Store::open(&directory.path().join("state.db")).unwrap();
    let old = binding();
    store.save_sandbox_tunnel(&old).unwrap();
    let mut next = transfer(&store, &old);
    store.confirm_sandbox_tunnel_handoff(&next).unwrap();
    next.connector_id = Some(Uuid::new_v4());
    next.applied_target = old.applied_target.clone();
    next.state = SandboxTunnelState::Stopped;
    store.save_sandbox_tunnel(&next).unwrap();
    assert!(
        store
            .sandbox_tunnel_claims_connector(old.connector_id.unwrap())
            .unwrap()
    );
    assert!(
        store
            .sandbox_tunnel_claims_connector(next.connector_id.unwrap())
            .unwrap()
    );
    next.state = SandboxTunnelState::Deleted;
    store.save_sandbox_tunnel(&next).unwrap();
    assert!(
        !store
            .sandbox_tunnel_claims_connector(old.connector_id.unwrap())
            .unwrap()
    );
    assert!(
        !store
            .sandbox_tunnel_claims_connector(next.connector_id.unwrap())
            .unwrap()
    );
}

#[tokio::test]
async fn only_operator_inventory_retains_retirement_evidence_after_project_deletion() {
    use axum::{
        body::{Body, to_bytes},
        extract::ConnectInfo,
        http::{Method, Request},
    };
    use tower::ServiceExt;
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    let mut binding = binding();
    task(&state, &binding);
    binding.state = SandboxTunnelState::Deleted;
    binding.local_removed = true;
    binding.remote_retirement = SandboxTunnelRetirement::OperatorRequired;
    state.store.save_sandbox_tunnel(&binding).unwrap();
    state
        .store
        .delete_sandbox_project(binding.project_id)
        .unwrap();
    assert!(
        !state
            .store
            .sandbox_project_has_live_tunnels(binding.project_id)
            .unwrap()
    );
    let app = crate::server::build_router(
        state.clone(),
        &crate::server::HttpConfig {
            bind_addr: "127.0.0.1:5523".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 8,
            request_timeout: Duration::from_secs(10),
            static_root: None,
        },
    )
    .layer(Extension(crate::ai_sandbox::access::LocalOperator));
    for path in [
        "/api/ai-sandbox/tunnels".into(),
        format!("/api/ai-sandbox/tunnels/{}", binding.id),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&path)
                    .header("host", "127.0.0.1:5523")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 65_536).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("operator_required"));
        assert!(body.contains("unverified"));
        let mut request = Request::builder()
            .method(Method::GET)
            .uri(path)
            .header("host", "127.0.0.1:5523")
            .body(Body::empty())
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(crate::tls::AuthenticatedHost {
                host_id: Uuid::new_v4(),
            }));
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
}

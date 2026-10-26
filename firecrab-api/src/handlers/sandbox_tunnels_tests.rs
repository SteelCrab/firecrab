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
    }
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
        target: QuickTunnelTarget::Apps {
            upstreams: vec!["172.30.1.2:8080".parse().unwrap()],
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

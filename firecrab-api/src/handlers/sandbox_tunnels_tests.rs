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
    SandboxTunnelBinding {
        id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        task_id: Uuid::new_v4(),
        attempt_id: Uuid::new_v4(),
        runtime,
        owner: SandboxPrincipal::Local,
        spec: SandboxTunnelSpec {
            task_id: Uuid::new_v4(),
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

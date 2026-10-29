use super::*;
use crate::persistence::Store;

#[tokio::test]
async fn retained_stop_refuses_to_power_cut_an_unconfirmed_guest() {
    let directory = tempfile::tempdir().unwrap();
    let registry =
        crate::templates::TemplateRegistry::from_specs(directory.path(), std::iter::empty())
            .unwrap();
    let state = AppState::with_db_file(registry, directory.path().join("state.db"))
        .await
        .unwrap();
    let mut manifest = spec();
    manifest.roles.truncate(1);
    manifest.roles[0].depends_on.clear();
    manifest.roles[0].execution_mode = SandboxExecutionMode::PersistentWorkspace;
    manifest.retention.ttl_seconds = None;
    manifest.allow.clear();
    let environment =
        reservation(Uuid::new_v4(), Uuid::new_v4(), Caller::default(), manifest).unwrap();
    state
        .store
        .reserve_sandbox_environment(&environment)
        .unwrap();
    let reserved = &environment.roles[0];
    let role = &environment.spec.roles[0];
    let mut record = crate::handlers::vms::test_support::record(
        &name(&environment, &reserved.role),
        reserved.vm_id,
    );
    record.template = role.template.name.clone();
    record.template_version = role.template.version.clone();
    record.template_kernel_sha256 = role.template.kernel_sha256.clone();
    record.template_rootfs_sha256 = role.template.rootfs_sha256.clone();
    record.template_boot_args_sha256 = role.template.boot_args_sha256.clone();
    record.cpu = role.cpu;
    record.ram = role.memory_mib;
    record.disk_gb = role.disk_gb;
    record.micro_network_id = environment.network_id;
    record.security_group_id = Some(reserved.security_group_id);
    record.egress_policy = EgressPolicy::Isolated;
    record.state = VmState::Running;
    record.last_runtime_id = Some(Uuid::new_v4());
    state.store.insert(&record).unwrap();
    state.vms.lock().unwrap().insert(record.id, record.clone());

    let error = vms::stop_vm(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(record.id.to_string()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "workspace_sync_unconfirmed");
    assert_eq!(vm(&state, record.id).unwrap().state, VmState::Running);
    assert_eq!(
        load(&state, environment.id).unwrap().state,
        SandboxEnvironmentState::Provisioning
    );

    let error = set_running(&state, environment.id, false)
        .await
        .unwrap_err();
    assert!(error.contains("workspace_sync_unconfirmed"), "{error}");
    assert_eq!(vm(&state, record.id).unwrap().state, VmState::Running);
    assert_eq!(
        load(&state, environment.id).unwrap().state,
        SandboxEnvironmentState::Stopped
    );
    let held = load(&state, environment.id).unwrap();
    assert!(held.error.unwrap().contains("guest retained"));
    assert_eq!(held.roles[0].agent, SandboxReadiness::Failed);
    assert!(binding(&state, environment.id, None).is_err());
}

fn role(name: &str, services: Vec<(&str, u16)>) -> SandboxEnvironmentRole {
    SandboxEnvironmentRole {
        name: name.into(),
        template: SandboxTemplatePin {
            name: "fixture".into(),
            version: "v1".into(),
            kernel_sha256: "a".repeat(64),
            rootfs_sha256: "b".repeat(64),
            initrd_sha256: None,
            boot_args_sha256: "c".repeat(64),
        },
        cpu: 1,
        memory_mib: 256,
        disk_gb: 2,
        depends_on: vec![],
        services: services
            .into_iter()
            .map(|(name, port)| SandboxEnvironmentService {
                name: name.into(),
                port,
                readiness: true,
            })
            .collect(),
        execution_mode: SandboxExecutionMode::AgentSandbox {
            runtime: SandboxContainerRuntime::Docker,
            image: format!("example.invalid/runtime@sha256:{}", "d".repeat(64)),
        },
    }
}
fn spec() -> SandboxEnvironmentSpec {
    let mut roles = vec![
        role("debug", vec![]),
        role("test", vec![]),
        role("backend", vec![("http", 8080)]),
        role("db", vec![("postgres", 5432)]),
    ];
    roles[0].depends_on = vec!["backend".into()];
    roles[1].depends_on = vec!["backend".into()];
    roles[2].depends_on = vec!["db".into()];
    SandboxEnvironmentSpec {
        version: 1,
        subnet_cidr: "10.240.1.0/24".into(),
        roles,
        allow: vec![
            SandboxEnvironmentAllow {
                from: "debug".into(),
                to: "backend".into(),
                port: 8080,
            },
            SandboxEnvironmentAllow {
                from: "test".into(),
                to: "backend".into(),
                port: 8080,
            },
            SandboxEnvironmentAllow {
                from: "backend".into(),
                to: "db".into(),
                port: 5432,
            },
        ],
        retention: SandboxEnvironmentRetention {
            ttl_seconds: Some(3600),
        },
    }
}
fn reserved() -> SandboxEnvironment {
    reservation(Uuid::new_v4(), Uuid::new_v4(), Caller::default(), spec()).unwrap()
}

async fn lifecycle_fixture() -> (tempfile::TempDir, AppState, SandboxEnvironment) {
    let dir = tempfile::tempdir().unwrap();
    let registry =
        crate::templates::TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
    let mut state = AppState::with_db_file(registry, dir.path().join("state.db"))
        .await
        .unwrap();
    state.network = crate::network::NetworkClient::with_socket_path(dir.path().join("helper.sock"));
    let mut environment = reserved();
    state
        .store
        .reserve_sandbox_environment(&environment)
        .unwrap();
    state
        .store
        .insert_micro_network(&MicroNetworkResponse {
            dns_mode: environment.dns_mode,
            id: environment.network_id,
            name: name(&environment, "network"),
            subnet_cidr: environment.spec.subnet_cidr.clone(),
            gateway: "10.240.1.1".into(),
            internet_enabled: false,
            uplink: None,
            ipv6_cidr: None,
            ipv6_gateway: None,
            ipv6_address_mode: None,
            ipv6_egress: None,
        })
        .unwrap();
    for index in 0..environment.roles.len() {
        let reserved = &environment.roles[index];
        let role = &environment.spec.roles[index];
        let mut record =
            vms::test_support::record(&name(&environment, &reserved.role), reserved.vm_id);
        record.template = role.template.name.clone();
        record.template_version = role.template.version.clone();
        record.template_kernel_sha256 = role.template.kernel_sha256.clone();
        record.template_rootfs_sha256 = role.template.rootfs_sha256.clone();
        record.template_boot_args_sha256 = role.template.boot_args_sha256.clone();
        record.cpu = role.cpu;
        record.ram = role.memory_mib;
        record.disk_gb = role.disk_gb;
        record.micro_network_id = environment.network_id;
        record.security_group_id = Some(reserved.security_group_id);
        record.egress_policy = EgressPolicy::Isolated;
        state.store.insert(&record).unwrap();
        state.vms.lock().unwrap().insert(record.id, record.clone());
        let lease = state
            .store
            .allocate_lease(
                record.id,
                SubnetSpec::parse(environment.network_id, &environment.spec.subnet_cidr).unwrap(),
            )
            .unwrap();
        environment.roles[index].ipv4 = Some(lease.ipv4.to_string());
    }
    state.store.save_sandbox_environment(&environment).unwrap();
    (dir, state, environment)
}

// This protocol peer records requests and never performs privileged host operations.
async fn helper(
    state: &AppState,
    fail_remove: bool,
) -> (
    tokio::task::JoinHandle<()>,
    Arc<std::sync::Mutex<Vec<firecrab_helper_protocol::network::NetworkRequest>>>,
) {
    use firecrab_helper_protocol::{
        framing::{read_frame, write_frame},
        network::{NetworkRequest, NetworkRequestEnvelope, NetworkResponseEnvelope},
    };
    let socket = state.sandbox.database.parent().unwrap().join("helper.sock");
    let listener = tokio::net::UnixListener::bind(socket).unwrap();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = requests.clone();
    let worker = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let envelope: NetworkRequestEnvelope = read_frame(&mut stream).await.unwrap();
            assert!(
                matches!(
                    envelope.request,
                    NetworkRequest::CheckPrivateDnsSupport
                        | NetworkRequest::RemoveVmPolicy { .. }
                        | NetworkRequest::DeleteTap { .. }
                        | NetworkRequest::RemoveMicroNetworkBridge { .. }
                        | NetworkRequest::EnsureFirewall { .. }
                        | NetworkRequest::SyncDhcpLeases { .. }
                ),
                "unexpected host operation: {:?}",
                envelope.request
            );
            let rejected = fail_remove
                && matches!(
                    envelope.request,
                    NetworkRequest::RemoveMicroNetworkBridge { .. }
                );
            seen.lock().unwrap().push(envelope.request);
            if rejected {
                drop(stream);
                continue;
            }
            write_frame(
                &mut stream,
                &NetworkResponseEnvelope {
                    version: firecrab_helper_protocol::PROTOCOL_VERSION,
                    request_id: envelope.request_id,
                    result: Ok(()),
                    pipe: None,
                },
            )
            .await
            .unwrap();
        }
    });
    (worker, requests)
}

#[tokio::test]
async fn provisioning_reuses_reserved_vms_and_saves_exact_policy_without_starting_them() {
    let (_dir, state, mut environment) = lifecycle_fixture().await;
    let (worker, requests) = helper(&state, false).await;
    let before = state.vms.lock().unwrap().clone();
    provision(&state, &mut environment).await.unwrap();
    for role in &environment.roles {
        let group = state
            .store
            .security_group(role.security_group_id)
            .unwrap()
            .unwrap();
        assert_eq!(group.rules, policy(&environment, &role.role).unwrap());
        assert_eq!(
            state
                .store
                .active_lease(role.vm_id)
                .unwrap()
                .unwrap()
                .ipv4
                .to_string(),
            role.ipv4.as_deref().unwrap()
        );
    }
    provision(&state, &mut environment).await.unwrap();
    assert_eq!(*state.vms.lock().unwrap(), before);
    assert_eq!(requests.lock().unwrap().len(), 2);
    let stopped = set_running(&state, environment.id, false).await.unwrap();
    assert_eq!(stopped.state, SandboxEnvironmentState::Stopped);
    assert_eq!(stopped.generation, environment.generation);
    assert!(
        stopped
            .roles
            .iter()
            .all(|r| r.services.iter().all(|s| !s.ready))
    );
    let exported = export(&state, environment.id).await.unwrap();
    assert!(exported.task_ids.is_empty());
    assert_eq!(exported.environment.generation, environment.generation);
    let deleted = cleanup(&state, environment.id, false).await.unwrap();
    assert_eq!(deleted.state, SandboxEnvironmentState::Deleted);
    assert!(state.vms.lock().unwrap().is_empty());
    for role in &deleted.roles {
        assert!(state.store.active_lease(role.vm_id).unwrap().is_none());
        assert!(
            state
                .store
                .security_group(role.security_group_id)
                .unwrap()
                .is_none()
        );
    }
    assert!(
        state
            .store
            .micro_network(environment.network_id)
            .unwrap()
            .is_none()
    );
    worker.abort();
}

#[tokio::test]
async fn partial_network_cleanup_keeps_checkpoint_then_restart_cleanup_finishes_owned_resources() {
    let (dir, state, mut environment) = lifecycle_fixture().await;
    let (worker, _) = helper(&state, true).await;
    provision(&state, &mut environment).await.unwrap();
    let unrelated = vms::test_support::record("unrelated", Uuid::new_v4());
    state.store.insert(&unrelated).unwrap();
    state
        .vms
        .lock()
        .unwrap()
        .insert(unrelated.id, unrelated.clone());
    assert!(cleanup(&state, environment.id, false).await.is_err());
    let held = load(&state, environment.id).unwrap();
    assert_eq!(held.state, SandboxEnvironmentState::CleanupPending);
    assert!(held.exported_at.is_some());
    assert!(
        state
            .store
            .micro_network(environment.network_id)
            .unwrap()
            .is_some()
    );
    assert_eq!(vm(&state, unrelated.id), Some(unrelated.clone()));
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    std::fs::remove_file(state.sandbox.database.parent().unwrap().join("helper.sock")).unwrap();
    drop(state);
    let registry =
        crate::templates::TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
    let mut state = AppState::with_db_file(registry, dir.path().join("state.db"))
        .await
        .unwrap();
    state.network = crate::network::NetworkClient::with_socket_path(dir.path().join("helper.sock"));
    let (worker, requests) = helper(&state, false).await;
    reconcile_cleanup(state.clone()).await;
    assert_eq!(
        load(&state, environment.id).unwrap().state,
        SandboxEnvironmentState::Deleted
    );
    assert!(
        state
            .store
            .micro_network(environment.network_id)
            .unwrap()
            .is_none()
    );
    let persisted_vms = state.store.load_all().unwrap();
    for role in &environment.roles {
        assert!(vm(&state, role.vm_id).is_none());
        assert!(!persisted_vms.contains_key(&role.vm_id));
        assert!(
            state
                .store
                .security_group(role.security_group_id)
                .unwrap()
                .is_none()
        );
        assert!(state.store.active_lease(role.vm_id).unwrap().is_none());
    }
    let removed: Vec<_> = requests
        .lock()
        .unwrap()
        .iter()
        .filter_map(|request| match request {
            firecrab_helper_protocol::network::NetworkRequest::RemoveMicroNetworkBridge {
                micro_network_id,
            } => Some(*micro_network_id),
            _ => None,
        })
        .collect();
    assert_eq!(removed, vec![environment.network_id]);
    assert_eq!(vm(&state, unrelated.id), Some(unrelated));
    worker.abort();
}

#[tokio::test]
async fn discovery_exposes_only_allowed_peers_and_rejects_changed_generation_or_unsafe_addresses() {
    let (_dir, state, mut environment) = lifecycle_fixture().await;
    for index in 0..environment.roles.len() {
        environment.roles[index].services = environment.spec.roles[index]
            .services
            .iter()
            .map(|service| SandboxDiscoveredService {
                name: service.name.clone(),
                port: service.port,
                address: environment.roles[index].ipv4.clone().unwrap(),
                ready: true,
            })
            .collect();
    }
    state.store.save_sandbox_environment(&environment).unwrap();
    let mut reference = SandboxEnvironmentRef {
        id: environment.id,
        generation: environment.generation,
        role: "debug".into(),
    };
    let debug = service_environment(&state, &reference).unwrap();
    assert_eq!(debug.len(), 2);
    assert_eq!(debug["FIRECRAB_SERVICE_BACKEND_HTTP_PORT"], "8080");
    assert_eq!(
        debug["FIRECRAB_SERVICE_BACKEND_HTTP_HOST"],
        environment.roles[2].ipv4.as_deref().unwrap()
    );
    assert!(!debug.keys().any(|key| key.contains("DB")));
    reference.role = "backend".into();
    let backend = service_environment(&state, &reference).unwrap();
    assert_eq!(backend.len(), 2);
    assert_eq!(backend["FIRECRAB_SERVICE_DB_POSTGRES_PORT"], "5432");
    reference.generation = Uuid::new_v4();
    assert!(
        service_environment(&state, &reference)
            .unwrap_err()
            .contains("generation")
    );
    reference.generation = environment.generation;
    environment.roles[3].services[0].address = "unexpected.example.invalid".into();
    state.store.save_sandbox_environment(&environment).unwrap();
    assert!(
        service_environment(&state, &reference)
            .unwrap_err()
            .contains("address")
    );
}

#[tokio::test]
async fn readiness_reconciliation_does_not_start_dependents_when_authenticated_agent_probe_fails() {
    let (_dir, state, mut environment) = lifecycle_fixture().await;
    let (worker, _) = helper(&state, false).await;
    provision(&state, &mut environment).await.unwrap();
    // Only the leaf is marked running. Its missing SSH material must keep every dependent stopped.
    let leaf = environment
        .roles
        .iter()
        .position(|r| r.role == "db")
        .unwrap();
    let mut record = vm(&state, environment.roles[leaf].vm_id).unwrap();
    record.state = VmState::Running;
    record.last_runtime_id = Some(Uuid::new_v4());
    state.vms.lock().unwrap().insert(record.id, record.clone());
    let observed = reconcile(&state, environment.id).await.unwrap();
    assert_eq!(observed.state, SandboxEnvironmentState::Provisioning);
    assert_eq!(observed.roles[leaf].agent, SandboxReadiness::Failed);
    assert_eq!(observed.roles[leaf].network, SandboxReadiness::Ready);
    assert_eq!(
        observed.roles[leaf].runtime.unwrap().runtime_id,
        record.last_runtime_id.unwrap()
    );
    for role in observed.roles.iter().filter(|r| r.role != "db") {
        assert_eq!(role.agent, SandboxReadiness::Pending);
        assert_eq!(vm(&state, role.vm_id).unwrap().state, VmState::Created);
    }
    assert!(binding(&state, environment.id, Some("debug")).is_err());
    worker.abort();
}

#[tokio::test]
async fn provisioning_refuses_changed_network_vm_and_group_ownership() {
    let (_dir, state, mut environment) = lifecycle_fixture().await;
    let (worker, _) = helper(&state, false).await;
    provision(&state, &mut environment).await.unwrap();
    let id = environment.roles[0].vm_id;
    state.vms.lock().unwrap().get_mut(&id).unwrap().cpu += 1;
    let error = reconcile(&state, environment.id).await.unwrap();
    assert_eq!(error.state, SandboxEnvironmentState::Failed);
    assert!(error.error.unwrap().contains("ownership"));
    assert_eq!(
        state.vms.lock().unwrap().get(&id).unwrap().state,
        VmState::Created
    );
    let group_id = environment.roles[0].security_group_id;
    let mut group = state.store.security_group(group_id).unwrap().unwrap();
    group.name = "borrowed-group".into();
    state
        .store
        .rename_security_group(group_id, &group.name)
        .unwrap();
    assert!(
        provision(&state, &mut environment)
            .await
            .unwrap_err()
            .contains("group ownership")
    );
    assert_eq!(
        state.store.security_group(group_id).unwrap().unwrap(),
        group
    );
    worker.abort();
}

#[test]
fn dependency_order_is_explicit_and_cycles_are_rejected() {
    let mut manifest = spec();
    let order = validate(&manifest).unwrap();
    assert_eq!(
        order
            .iter()
            .map(|i| manifest.roles[*i].name.as_str())
            .collect::<Vec<_>>(),
        vec!["db", "backend", "debug", "test"]
    );
    manifest.roles[3].depends_on = vec!["debug".into()];
    assert!(validate(&manifest).is_err());
}

#[test]
fn malformed_manifests_and_mutable_container_sources_fail_before_resource_reservation() {
    use serde_json::json;
    for (pointer, value, reason) in [
        ("/version", json!(2), "version"),
        ("/roles", json!([]), "1–8 roles"),
        ("/subnetCidr", json!("10.240.1.0/30"), "14 guest addresses"),
        ("/roles/0/name", json!("Debug;command"), "role name"),
        ("/roles/0/memoryMib", json!(257), "memory"),
        (
            "/roles/0/template/initrdSha256",
            json!("A".repeat(64)),
            "pinned image",
        ),
        (
            "/roles/0/services",
            json!([{"name":"http","port":0}]),
            "declared service",
        ),
        (
            "/roles/0/services",
            json!([{"name":"http","port":80},{"name":"http","port":81}]),
            "declared service",
        ),
        ("/roles/0/dependsOn", json!(["missing"]), "dependency"),
        (
            "/roles/0/dependsOn",
            json!(["backend", "backend"]),
            "dependency",
        ),
        ("/roles/1/name", json!("debug"), "unique"),
        ("/retention/ttlSeconds", json!(0), "TTL"),
    ] {
        let mut manifest = serde_json::to_value(spec()).unwrap();
        *manifest.pointer_mut(pointer).unwrap() = value;
        let manifest = serde_json::from_value(manifest).unwrap();
        let error = validate(&manifest).unwrap_err();
        assert!(error.contains(reason), "{pointer}: {error}");
    }
    for (runtime, image) in [
        (
            SandboxContainerRuntime::Docker,
            format!("-option@sha256:{}", "a".repeat(64)),
        ),
        (
            SandboxContainerRuntime::Docker,
            format!("bad;name@sha256:{}", "a".repeat(64)),
        ),
        (
            SandboxContainerRuntime::Docker,
            format!("safe@sha256:{}", "A".repeat(64)),
        ),
        (SandboxContainerRuntime::Crun, "ubuntu:latest".into()),
        (
            SandboxContainerRuntime::Crun,
            format!("bad;name@sha256:{}", "a".repeat(64)),
        ),
        (SandboxContainerRuntime::Nspawn, "mutable.tar".into()),
        (SandboxContainerRuntime::Nspawn, "sha256:short".into()),
    ] {
        assert!(validate_mode(&SandboxExecutionMode::AgentSandbox { runtime, image }).is_err());
    }
    assert!(
        validate_mode(&SandboxExecutionMode::AgentSandbox {
            runtime: SandboxContainerRuntime::Crun,
            image: format!("example.invalid/image@sha256:{}", "a".repeat(64)),
        })
        .is_ok()
    );
    assert!(
        validate_mode(&SandboxExecutionMode::AgentSandbox {
            runtime: SandboxContainerRuntime::Nspawn,
            image: format!("sha256:{}", "a".repeat(64)),
        })
        .is_ok()
    );
}

#[test]
fn mutable_images_ipv6_and_implicit_policy_ports_are_rejected() {
    let mut manifest = spec();
    manifest.roles[0].execution_mode = SandboxExecutionMode::AgentSandbox {
        runtime: SandboxContainerRuntime::Docker,
        image: "ubuntu:latest".into(),
    };
    assert!(validate(&manifest).is_err());
    let mut manifest = spec();
    manifest.subnet_cidr = "fd00::/64".into();
    assert!(validate(&manifest).is_err());
    let mut manifest = spec();
    manifest.allow[0].port = 22;
    assert!(validate(&manifest).is_err());
    let mut manifest = spec();
    manifest.roles[0].execution_mode = SandboxExecutionMode::PersistentWorkspace;
    assert!(validate(&manifest).is_err());
    manifest.retention.ttl_seconds = None;
    assert!(validate(&manifest).is_ok());
}

#[test]
fn four_role_policy_allows_only_declared_connections_and_no_other_environment() {
    let mut environment = reserved();
    for (index, role) in environment.roles.iter_mut().enumerate() {
        role.ipv4 = Some(format!("10.240.1.{}", index + 2));
    }
    let debug = policy(&environment, "debug").unwrap();
    assert_eq!(debug.len(), 1);
    assert_eq!(debug[0].direction, SecurityGroupDirection::Egress);
    assert_eq!(debug[0].peer_cidr, "10.240.1.4/32");
    assert_eq!(debug[0].port_from, 8080);
    let backend = policy(&environment, "backend").unwrap();
    assert_eq!(backend.len(), 3);
    assert!(
        backend
            .iter()
            .any(|r| r.direction == SecurityGroupDirection::Egress
                && r.peer_cidr == "10.240.1.5/32"
                && r.port_from == 5432)
    );
    let db = policy(&environment, "db").unwrap();
    assert_eq!(db.len(), 1);
    assert_eq!(db[0].direction, SecurityGroupDirection::Ingress);
    assert_eq!(db[0].peer_cidr, "10.240.1.4/32");
    for role in &environment.roles {
        for rule in policy(&environment, &role.role).unwrap() {
            assert!(!rule.peer_cidr.starts_with("10.240.2."));
            assert_ne!(rule.peer_cidr, "0.0.0.0/0");
            assert_eq!(rule.protocol, PortProtocol::Tcp);
            assert_eq!(rule.port_from, rule.port_to);
        }
    }
}

#[test]
fn reservations_survive_restart_and_cannot_change_owners_generations_or_ids() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let store = Store::open(&path).unwrap();
    let mut environment = reserved();
    store.reserve_sandbox_environment(&environment).unwrap();
    assert!(store.reserve_sandbox_environment(&environment).is_err());
    environment.roles[0].agent = SandboxReadiness::Ready;
    store.save_sandbox_environment(&environment).unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    let reopened = store.sandbox_environment(environment.id).unwrap().unwrap();
    assert_eq!(reopened, environment);
    assert_eq!(
        environment.dns_mode,
        firecrab_api_types::MicroNetworkDnsMode::PrivateOnly
    );
    let mut legacy_json = serde_json::to_value(&environment).unwrap();
    legacy_json.as_object_mut().unwrap().remove("dnsMode");
    let legacy: SandboxEnvironment = serde_json::from_value(legacy_json).unwrap();
    assert_eq!(
        legacy.dns_mode,
        firecrab_api_types::MicroNetworkDnsMode::LegacyForward
    );
    assert_eq!(legacy.spec, environment.spec);
    let mut changed = environment.clone();
    changed.dns_mode = firecrab_api_types::MicroNetworkDnsMode::LegacyForward;
    assert!(store.save_sandbox_environment(&changed).is_err());
    let mut changed = environment.clone();
    changed.roles[0].vm_id = Uuid::new_v4();
    assert!(store.save_sandbox_environment(&changed).is_err());
    let mut changed = environment.clone();
    changed.generation = Uuid::new_v4();
    assert!(store.save_sandbox_environment(&changed).is_err());
    let mut changed = environment.clone();
    changed.spec.roles[0].template.version = "v2".into();
    assert!(store.save_sandbox_environment(&changed).is_err());
    let unrelated = reserved();
    assert!(
        !store
            .sandbox_environment_owns(&unrelated, "vm", environment.roles[0].vm_id)
            .unwrap()
    );
    assert_eq!(
        store
            .sandbox_environments(environment.project_id)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn deleted_environment_is_a_tombstone_and_retained_workspaces_never_expire() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state.db")).unwrap();
    let mut environment = reserved();
    store.reserve_sandbox_environment(&environment).unwrap();
    environment.state = SandboxEnvironmentState::Deleted;
    store.save_sandbox_environment(&environment).unwrap();
    environment.state = SandboxEnvironmentState::Provisioning;
    assert!(store.save_sandbox_environment(&environment).is_err());
    assert!(
        store
            .sandbox_environment_cleanup_ids(u64::MAX / 2)
            .unwrap()
            .is_empty()
    );
    let mut manifest = spec();
    manifest.roles[0].execution_mode = SandboxExecutionMode::PersistentWorkspace;
    manifest.retention.ttl_seconds = None;
    let workspace =
        reservation(Uuid::new_v4(), Uuid::new_v4(), Caller::default(), manifest).unwrap();
    assert!(workspace.expires_at.is_none());
}

#[tokio::test]
async fn partial_cleanup_keeps_unrelated_vm_and_workspace_requires_explicit_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let registry =
        crate::templates::TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
    let state = AppState::with_db_file(registry, dir.path().join("state.db"))
        .await
        .unwrap();
    let borrowed = crate::handlers::vms::test_support::record("unrelated-user-vm", Uuid::new_v4());
    crate::handlers::vms::test_support::seed_vm(&state, &borrowed);
    let shared = dir.path().join("shared-base");
    std::fs::write(&shared, "shared image").unwrap();
    let environment = reserved();
    state
        .store
        .reserve_sandbox_environment(&environment)
        .unwrap();
    // Nothing was provisioned before the simulated crash. Cleanup must not search by name or subnet.
    let deleted = cleanup(&state, environment.id, false).await.unwrap();
    assert_eq!(deleted.state, SandboxEnvironmentState::Deleted);
    assert_eq!(vm(&state, borrowed.id), Some(borrowed));
    assert_eq!(std::fs::read_to_string(shared).unwrap(), "shared image");
    let mut manifest = spec();
    manifest.roles[0].execution_mode = SandboxExecutionMode::PersistentWorkspace;
    manifest.retention.ttl_seconds = None;
    let workspace =
        reservation(Uuid::new_v4(), Uuid::new_v4(), Caller::default(), manifest).unwrap();
    state.store.reserve_sandbox_environment(&workspace).unwrap();
    assert!(cleanup(&state, workspace.id, false).await.is_err());
    assert_eq!(
        load(&state, workspace.id).unwrap().state,
        SandboxEnvironmentState::Provisioning
    );
    assert_eq!(
        cleanup(&state, workspace.id, true).await.unwrap().state,
        SandboxEnvironmentState::Deleted
    );
}

#[tokio::test]
async fn failed_export_or_changed_artifact_keeps_environment_resources() {
    let (_dir, state, mut saved) = super::super::recovery_tests::fixture().await;
    let environment = reservation(
        Uuid::new_v4(),
        saved.task.project_id,
        Caller::default(),
        spec(),
    )
    .unwrap();
    state
        .store
        .reserve_sandbox_environment(&environment)
        .unwrap();
    // Admission fixes the Environment identity. Rebuild this synthetic fixture before
    // its first Environment checkpoint instead of mutating an already admitted task.
    state
        .store
        .delete_sandbox_project(saved.task.project_id)
        .unwrap();
    saved.project.environment_id = Some(environment.id);
    let runtime = SandboxRuntimeRef {
        vm_id: environment.roles[0].vm_id,
        runtime_id: saved.task.runtime.unwrap().runtime_id,
    };
    saved.project.vm_id = Some(runtime.vm_id);
    saved.task.runtime = Some(runtime);
    saved.guest = Some(runtime);
    saved.task.environment = Some(SandboxEnvironmentRef {
        id: environment.id,
        generation: environment.generation,
        role: "debug".into(),
    });
    saved.guest_pending = false;
    saved.guest_staged = false;
    saved.task.state = SandboxTaskState::Failed;
    saved.results_exported = false;
    state.store.save_sandbox_task(&saved).unwrap();
    assert!(cleanup(&state, environment.id, false).await.is_err());
    assert_eq!(
        load(&state, environment.id).unwrap().state,
        SandboxEnvironmentState::Provisioning
    );
    saved.results_exported = true;
    state.store.save_sandbox_task(&saved).unwrap();
    assert_eq!(
        verify_export(&state, &environment).unwrap(),
        vec![saved.task.id]
    );
    saved.task.artifacts.push(SandboxArtifact {
        id: Uuid::new_v4(),
        task_id: saved.task.id,
        attempt_id: saved.task.attempt_id,
        path: "required.bin".into(),
        size: 4,
        sha256: "a".repeat(64),
    });
    state.store.save_sandbox_task(&saved).unwrap();
    assert!(cleanup(&state, environment.id, false).await.is_err());
    assert_eq!(
        load(&state, environment.id).unwrap().state,
        SandboxEnvironmentState::Provisioning
    );
}

#[tokio::test]
async fn admission_rechecks_actual_policy_address_and_runtime_after_readiness() {
    let directory = tempfile::tempdir().unwrap();
    let registry =
        crate::templates::TemplateRegistry::from_specs(directory.path(), std::iter::empty())
            .unwrap();
    let state = AppState::with_db_file(registry, directory.path().join("state.db"))
        .await
        .unwrap();
    let mut manifest = spec();
    manifest.roles.truncate(1);
    manifest.roles[0].depends_on.clear();
    manifest.allow.clear();
    let mut environment =
        reservation(Uuid::new_v4(), Uuid::new_v4(), Caller::default(), manifest).unwrap();
    let network = MicroNetworkResponse {
        dns_mode: environment.dns_mode,
        id: environment.network_id,
        name: name(&environment, "network"),
        subnet_cidr: environment.spec.subnet_cidr.clone(),
        gateway: "10.240.1.1".into(),
        internet_enabled: false,
        uplink: None,
        ipv6_cidr: None,
        ipv6_gateway: None,
        ipv6_address_mode: None,
        ipv6_egress: None,
    };
    state.store.insert_micro_network(&network).unwrap();
    let reserved = &environment.roles[0];
    let definition = &environment.spec.roles[0];
    let mut record = crate::handlers::vms::test_support::record(
        &name(&environment, &reserved.role),
        reserved.vm_id,
    );
    record.template = definition.template.name.clone();
    record.template_version = definition.template.version.clone();
    record.template_kernel_sha256 = definition.template.kernel_sha256.clone();
    record.template_rootfs_sha256 = definition.template.rootfs_sha256.clone();
    record.template_boot_args_sha256 = definition.template.boot_args_sha256.clone();
    record.cpu = definition.cpu;
    record.ram = definition.memory_mib;
    record.disk_gb = definition.disk_gb;
    record.micro_network_id = environment.network_id;
    record.security_group_id = Some(reserved.security_group_id);
    record.egress_policy = EgressPolicy::Isolated;
    record.state = VmState::Running;
    record.last_runtime_id = Some(Uuid::new_v4());
    state.store.insert(&record).unwrap();
    state.vms.lock().unwrap().insert(record.id, record.clone());
    let lease = state
        .store
        .allocate_lease(
            record.id,
            SubnetSpec::parse(network.id, &network.subnet_cidr).unwrap(),
        )
        .unwrap();
    let group = SecurityGroupResponse {
        id: reserved.security_group_id,
        name: name(&environment, &reserved.role),
        micro_network_id: network.id,
        rules: vec![],
    };
    state.store.insert_security_group(&group).unwrap();
    environment.roles[0].runtime = Some(SandboxRuntimeRef {
        vm_id: record.id,
        runtime_id: record.last_runtime_id.unwrap(),
    });
    environment.roles[0].ipv4 = Some(lease.ipv4.to_string());
    environment.roles[0].agent = SandboxReadiness::Ready;
    environment.roles[0].network = SandboxReadiness::Ready;
    environment.roles[0].application = SandboxReadiness::Ready;
    environment.state = SandboxEnvironmentState::Ready;
    state
        .store
        .reserve_sandbox_environment(&environment)
        .unwrap();
    assert!(binding(&state, environment.id, None).is_ok());
    state
        .store
        .replace_security_group_rules(
            group.id,
            &[SecurityGroupRule {
                direction: SecurityGroupDirection::Egress,
                protocol: PortProtocol::Tcp,
                port_from: 443,
                port_to: 443,
                peer_cidr: "0.0.0.0/0".into(),
            }],
        )
        .unwrap();
    assert!(
        binding(&state, environment.id, None)
            .unwrap_err()
            .contains("policy changed")
    );
    state
        .store
        .replace_security_group_rules(group.id, &[])
        .unwrap();
    let database = rusqlite::Connection::open(directory.path().join("state.db")).unwrap();
    database
        .execute(
            "UPDATE network_leases SET ipv6='fd00::2' WHERE vm_id=?1 AND released_at IS NULL",
            [record.id.to_string()],
        )
        .unwrap();
    assert!(
        binding(&state, environment.id, None)
            .unwrap_err()
            .contains("address changed")
    );
    database
        .execute(
            "UPDATE network_leases SET ipv6=NULL WHERE vm_id=?1 AND released_at IS NULL",
            [record.id.to_string()],
        )
        .unwrap();
    state
        .vms
        .lock()
        .unwrap()
        .get_mut(&record.id)
        .unwrap()
        .last_runtime_id = Some(Uuid::new_v4());
    assert!(
        binding(&state, environment.id, None)
            .unwrap_err()
            .contains("runtime or readiness changed")
    );
}

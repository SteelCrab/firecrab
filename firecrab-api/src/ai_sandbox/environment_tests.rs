use super::*;
use crate::persistence::Store;

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

//! Single-host environment reconciliation reuses Runtime VM/network/security APIs.
//! Task retries never happen here. Only recorded, owned resource IDs may be changed.
use super::{access::Caller, guest, now};
use crate::{
    handlers::{micro_networks, vms},
    ipam::SubnetSpec,
    model::{VmPurpose, VmRecord},
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension,
    extract::{Path, State},
};
use firecrab_api_types::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
use uuid::Uuid;

#[cfg(test)]
#[path = "environment_tests.rs"]
mod tests;

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 24
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Validate the full graph and policy before reserving a single host resource.
pub fn validate(spec: &SandboxEnvironmentSpec) -> Result<Vec<usize>, String> {
    if spec.version != 1 {
        return Err("Unsupported environment manifest version".into());
    }
    if spec.roles.is_empty() || spec.roles.len() > 8 {
        return Err("Environment requires 1–8 roles".into());
    }
    let subnet = SubnetSpec::parse(Uuid::nil(), &spec.subnet_cidr)
        .ok_or("Invalid environment IPv4 subnet")?;
    if subnet.prefix > 28 {
        return Err("Environment subnet must provide at least 14 guest addresses".into());
    }
    let names: BTreeSet<_> = spec.roles.iter().map(|r| r.name.as_str()).collect();
    if names.len() != spec.roles.len() {
        return Err("Environment role names must be unique".into());
    }
    for role in &spec.roles {
        if !valid_name(&role.name)
            || !(1..=32).contains(&role.cpu)
            || !(128..=65536).contains(&role.memory_mib)
            || !role.memory_mib.is_power_of_two()
            || !(1..=4096).contains(&role.disk_gb)
        {
            return Err("Invalid role name or CPU, memory or disk limit".into());
        }
        let pin = &role.template;
        if pin.name.is_empty()
            || pin.version.is_empty()
            || !hash(&pin.kernel_sha256)
            || !hash(&pin.rootfs_sha256)
            || !hash(&pin.boot_args_sha256)
            || pin.initrd_sha256.as_deref().is_some_and(|v| !hash(v))
        {
            return Err("Each role requires a complete pinned image identity".into());
        }
        if role.services.len() > 16 {
            return Err("At most 16 services per role are allowed".into());
        }
        let mut services = BTreeSet::new();
        for service in &role.services {
            if !valid_name(&service.name)
                || service.port == 0
                || !services.insert(service.name.as_str())
            {
                return Err("Invalid or duplicate declared service".into());
            }
        }
        let mut dependencies = BTreeSet::new();
        for dependency in &role.depends_on {
            if dependency == &role.name
                || !names.contains(dependency.as_str())
                || !dependencies.insert(dependency)
            {
                return Err("Invalid or duplicate readiness dependency".into());
            }
        }
        validate_mode(&role.execution_mode)?;
    }
    if spec.allow.len() > 64 {
        return Err("At most 64 environment network rules are allowed".into());
    }
    let mut rules = BTreeSet::new();
    for allow in &spec.allow {
        let target = spec
            .roles
            .iter()
            .find(|r| r.name == allow.to)
            .ok_or("Network policy names an unknown destination role")?;
        if !names.contains(allow.from.as_str())
            || allow.from == allow.to
            || !target.services.iter().any(|s| s.port == allow.port)
            || !rules.insert((&allow.from, &allow.to, allow.port))
        {
            return Err(
                "Network policy requires unique exact-role declared-service TCP rules".into(),
            );
        }
    }
    if let Some(ttl) = spec.retention.ttl_seconds {
        if !(60..=604800).contains(&ttl) {
            return Err("Environment TTL must be 60–604800 seconds".into());
        }
        if retained(spec) {
            return Err("Persistent Workspace data cannot have automatic TTL deletion".into());
        }
    }
    let mut order = Vec::new();
    while order.len() < spec.roles.len() {
        let next = (0..spec.roles.len()).find(|index| {
            !order.contains(index)
                && spec.roles[*index].depends_on.iter().all(|dependency| {
                    order
                        .iter()
                        .any(|done| spec.roles[*done].name == *dependency)
                })
        });
        let Some(index) = next else {
            return Err("Environment readiness dependencies contain a cycle".into());
        };
        order.push(index);
    }
    Ok(order)
}

/// Require an immutable guest image; no implicit pulls or mutable container tags.
pub fn validate_mode(mode: &SandboxExecutionMode) -> Result<(), String> {
    let SandboxExecutionMode::AgentSandbox { runtime, image } = mode else {
        return Ok(());
    };
    let digest = match runtime {
        SandboxContainerRuntime::Docker => {
            let Some((repository, digest)) = image.rsplit_once("@sha256:") else {
                return Err("Docker images require repository@sha256:digest".into());
            };
            if repository.is_empty()
                || repository.len() > 200
                || repository.starts_with('-')
                || !repository
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"./:_-".contains(&b))
            {
                return Err("Invalid digest-pinned Docker image".into());
            }
            digest
        }
        SandboxContainerRuntime::Nspawn => image
            .strip_prefix("sha256:")
            .ok_or("nspawn images require sha256:digest")?,
    };
    if !hash(digest) {
        return Err("Container image digest must be lowercase SHA256".into());
    }
    Ok(())
}

pub fn retained(spec: &SandboxEnvironmentSpec) -> bool {
    spec.roles.iter().any(|role| {
        matches!(
            role.execution_mode,
            SandboxExecutionMode::PersistentWorkspace
        )
    })
}

/// An immutable manifest reserves its resources before provisioning starts.
pub fn reservation(
    id: Uuid,
    project: Uuid,
    caller: Caller,
    spec: SandboxEnvironmentSpec,
) -> Result<SandboxEnvironment, String> {
    validate(&spec)?;
    let at = now();
    Ok(SandboxEnvironment {
        id,
        project_id: project,
        owner: caller.principal,
        generation: Uuid::new_v4(),
        network_id: Uuid::new_v4(),
        roles: spec
            .roles
            .iter()
            .map(|role| SandboxEnvironmentRoleState {
                role: role.name.clone(),
                vm_id: Uuid::new_v4(),
                security_group_id: Uuid::new_v4(),
                runtime: None,
                ipv4: None,
                agent: SandboxReadiness::Pending,
                network: SandboxReadiness::Pending,
                application: SandboxReadiness::Pending,
                services: vec![],
                error: None,
            })
            .collect(),
        expires_at: spec.retention.ttl_seconds.map(|ttl| at + ttl * 1000),
        spec,
        state: SandboxEnvironmentState::Provisioning,
        created_at: at,
        updated_at: at,
        exported_at: None,
        error: None,
    })
}

pub(crate) fn load(state: &AppState, id: Uuid) -> Result<SandboxEnvironment, String> {
    state
        .store
        .sandbox_environment(id)
        .map_err(|_| "Cannot read environment ownership")?
        .ok_or_else(|| "Environment does not exist".into())
}

fn save(state: &AppState, environment: &mut SandboxEnvironment) -> Result<(), String> {
    environment.updated_at = now();
    state
        .store
        .save_sandbox_environment(environment)
        .map_err(|_| "Cannot persist environment checkpoint".into())
}

fn name(environment: &SandboxEnvironment, role: &str) -> String {
    format!("ase-{}-{role}", &environment.id.simple().to_string()[..12])
}

fn owns(
    state: &AppState,
    environment: &SandboxEnvironment,
    kind: &str,
    id: Uuid,
) -> Result<(), String> {
    if !state
        .store
        .sandbox_environment_owns(environment, kind, id)
        .map_err(|_| "Cannot read environment resource ownership")?
    {
        return Err(
            "Refusing to change a resource outside this environment ownership ledger".into(),
        );
    }
    Ok(())
}

fn template(
    state: &AppState,
    role: &SandboxEnvironmentRole,
) -> Result<Arc<crate::templates::TemplateVersion>, String> {
    let pin = &role.template;
    let template = state
        .templates
        .resolve_version(&pin.name, &pin.version)
        .ok_or("Pinned environment image is unavailable")?;
    if template.kernel.sha256() != pin.kernel_sha256
        || template.rootfs.sha256() != pin.rootfs_sha256
        || template.boot_args_sha256() != pin.boot_args_sha256
        || template.initrd.as_ref().map(|a| a.sha256()) != pin.initrd_sha256.as_deref()
    {
        return Err("Pinned environment image contents changed".into());
    }
    Ok(template)
}

fn vm(state: &AppState, id: Uuid) -> Option<VmRecord> {
    state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&id)
        .cloned()
}

/// Every reconciliation compares the real VM with its immutable ownership and image pins.
fn verify_vm(
    state: &AppState,
    environment: &SandboxEnvironment,
    index: usize,
    vm: &VmRecord,
) -> Result<(), String> {
    let role = &environment.spec.roles[index];
    let reserved = &environment.roles[index];
    owns(state, environment, "vm", vm.id)?;
    if vm.id != reserved.vm_id
        || vm.name != name(environment, &role.name)
        || vm.purpose != VmPurpose::Instance
        || vm.micro_network_id != environment.network_id
        || vm.security_group_id != Some(reserved.security_group_id)
        || vm.template != role.template.name
        || vm.template_version != role.template.version
        || vm.template_kernel_sha256 != role.template.kernel_sha256
        || vm.template_rootfs_sha256 != role.template.rootfs_sha256
        || vm.template_boot_args_sha256 != role.template.boot_args_sha256
        || vm.cpu != role.cpu
        || vm.ram != role.memory_mib
        || vm.disk_gb != role.disk_gb
        || vm.egress_policy != EgressPolicy::Isolated
        || !vm.env.is_empty()
        || state
            .store
            .vm_has_service_owner(vm.id)
            .map_err(|_| "Cannot check VM service ownership")?
        || state
            .store
            .cluster_worker(vm.id)
            .map_err(|_| "Cannot check VM cluster ownership")?
            .is_some()
        || !state
            .store
            .vm_block_storages(vm.id)
            .map_err(|_| "Cannot check VM storage ownership")?
            .is_empty()
    {
        return Err("Environment VM ownership, policy or pinned resources changed".into());
    }
    Ok(())
}

async fn provision(state: &AppState, environment: &mut SandboxEnvironment) -> Result<(), String> {
    owns(state, environment, "network", environment.network_id)?;
    let network = state
        .store
        .micro_network(environment.network_id)
        .map_err(|_| "Cannot inspect environment network")?;
    if let Some(network) = network {
        if network.name != name(environment, "network")
            || network.subnet_cidr != environment.spec.subnet_cidr
            || network.internet_enabled
            || network.ipv6_cidr.is_some()
        {
            return Err(
                "Environment network differs from the reserved IPv4-only isolated manifest".into(),
            );
        }
    } else {
        let _ = micro_networks::create_micro_network_with_id(
            state.clone(),
            RequestId(Uuid::new_v4()),
            CreateMicroNetworkRequest {
                name: name(environment, "network"),
                subnet_cidr: environment.spec.subnet_cidr.clone(),
                internet_enabled: false,
                uplink: None,
                ipv6_cidr: None,
                ipv6_address_mode: None,
            },
            environment.network_id,
        )
        .await
        .map_err(|e| e.redacted().description())?;
    }
    for index in 0..environment.roles.len() {
        let reserved = &environment.roles[index];
        owns(
            state,
            environment,
            "security_group",
            reserved.security_group_id,
        )?;
        let group = state
            .store
            .security_group(reserved.security_group_id)
            .map_err(|_| "Cannot inspect environment security group")?;
        if let Some(group) = group {
            if group.name != name(environment, &reserved.role)
                || group.micro_network_id != environment.network_id
            {
                return Err("Environment security group ownership changed".into());
            }
        } else {
            state
                .store
                .insert_security_group(&SecurityGroupResponse {
                    id: reserved.security_group_id,
                    name: name(environment, &reserved.role),
                    micro_network_id: environment.network_id,
                    rules: vec![],
                })
                .map_err(|_| "Cannot reserve environment security group")?;
        }
        if let Some(current) = vm(state, reserved.vm_id) {
            verify_vm(state, environment, index, &current)?;
        } else {
            let role = &environment.spec.roles[index];
            let exact = template(state, role)?;
            let _ = vms::create_vm_from_template(
                state.clone(),
                RequestId(Uuid::new_v4()),
                CreateVmRequest {
                    name: name(environment, &role.name),
                    template: role.template.name.clone(),
                    cpu: role.cpu,
                    ram: role.memory_mib,
                    disk_gb: role.disk_gb,
                    egress_policy: EgressPolicy::Isolated,
                    micro_network_id: environment.network_id,
                    security_group_id: Some(reserved.security_group_id),
                    storage_root: None,
                    shell_ids: vec![],
                    port_forwards: vec![],
                    env: BTreeMap::new(),
                    block_storages: vec![],
                },
                reserved.vm_id,
                Some(exact),
            )
            .await
            .map_err(|e| e.redacted().description())?;
        }
        let lease = state
            .store
            .active_lease(reserved.vm_id)
            .map_err(|_| "Cannot read environment lease")?
            .ok_or("Environment VM has no reserved IPv4 address")?;
        if lease.ipv6.is_some() {
            return Err("IPv6 cannot bypass environment policy".into());
        }
        environment.roles[index].ipv4 = Some(lease.ipv4.to_string());
        save(state, environment)?;
    }
    // Leases exist before rules are installed. No role is started with a permissive policy.
    for index in 0..environment.roles.len() {
        let reserved = &environment.roles[index];
        let expected = policy(environment, &reserved.role)?;
        let current = state
            .store
            .security_group(reserved.security_group_id)
            .map_err(|_| "Cannot read environment policy")?
            .ok_or("Environment security group disappeared")?;
        if current.rules == expected {
            continue;
        }
        let record = vm(state, reserved.vm_id).ok_or("Environment VM disappeared")?;
        if crate::network_policy::vm_is_active(state, &record)
            .map_err(|_| "Cannot verify environment VM inactivity")?
        {
            return Err("Environment policy changed while its VM is active; stop and inspect before reconciliation".into());
        }
        state
            .store
            .replace_security_group_rules(reserved.security_group_id, &expected)
            .map_err(|_| "Cannot persist environment policy")?;
    }
    Ok(())
}

/// Exact /32 role policies never permit other environments or Internet destinations.
pub fn policy(
    environment: &SandboxEnvironment,
    role: &str,
) -> Result<Vec<SecurityGroupRule>, String> {
    let mut rules = Vec::new();
    for allow in &environment.spec.allow {
        let peer = if allow.from == role {
            Some((SecurityGroupDirection::Egress, &allow.to))
        } else if allow.to == role {
            Some((SecurityGroupDirection::Ingress, &allow.from))
        } else {
            None
        };
        let Some((direction, peer)) = peer else {
            continue;
        };
        let address = environment
            .roles
            .iter()
            .find(|r| r.role == *peer)
            .and_then(|r| r.ipv4.as_deref())
            .ok_or("Environment policy peer has no reserved address")?;
        address
            .parse::<std::net::Ipv4Addr>()
            .map_err(|_| "Invalid environment policy address")?;
        rules.push(SecurityGroupRule {
            direction,
            protocol: PortProtocol::Tcp,
            port_from: allow.port,
            port_to: allow.port,
            peer_cidr: format!("{address}/32"),
        });
    }
    Ok(rules)
}

fn ready(role: &SandboxEnvironmentRoleState) -> bool {
    role.agent == SandboxReadiness::Ready
        && role.network == SandboxReadiness::Ready
        && role.application == SandboxReadiness::Ready
}

/// Re-enter after partial creation/API restart using the same reserved resource IDs.
pub async fn reconcile(state: &AppState, id: Uuid) -> Result<SandboxEnvironment, String> {
    let _guard = state.lock_vm(id).await;
    let mut environment = load(state, id)?;
    if matches!(
        environment.state,
        SandboxEnvironmentState::Deleted
            | SandboxEnvironmentState::Stopped
            | SandboxEnvironmentState::CleanupPending
    ) {
        return Ok(environment);
    }
    environment.state = SandboxEnvironmentState::Provisioning;
    environment.error = None;
    save(state, &mut environment)?;
    let result = reconcile_roles(state, &mut environment).await;
    if let Err(error) = result {
        environment.state = SandboxEnvironmentState::Failed;
        environment.error = Some(error.chars().take(512).collect());
    } else if environment.roles.iter().all(ready) {
        environment.state = SandboxEnvironmentState::Ready;
    }
    save(state, &mut environment)?;
    Ok(environment)
}

async fn reconcile_roles(
    state: &AppState,
    environment: &mut SandboxEnvironment,
) -> Result<(), String> {
    let order = validate(&environment.spec)?;
    provision(state, environment).await?;
    for index in order {
        let role = &environment.spec.roles[index];
        let dependencies_ready = role.depends_on.iter().all(|dependency| {
            environment
                .roles
                .iter()
                .any(|r| r.role == *dependency && ready(r))
        });
        if !dependencies_ready {
            continue;
        }
        let mut record =
            vm(state, environment.roles[index].vm_id).ok_or("Environment VM disappeared")?;
        verify_vm(state, environment, index, &record)?;
        if matches!(
            record.state,
            VmState::Created | VmState::Stopped | VmState::Error
        ) {
            let _ = vms::start_vm(
                State(state.clone()),
                Extension(RequestId(Uuid::new_v4())),
                Path(record.id.to_string()),
            )
            .await
            .map_err(|e| e.redacted().description())?;
            record = vm(state, record.id).ok_or("Environment VM disappeared after start")?;
        }
        observe(state, environment, index, &record).await?;
        save(state, environment)?;
    }
    Ok(())
}

async fn observe(
    state: &AppState,
    environment: &mut SandboxEnvironment,
    index: usize,
    record: &VmRecord,
) -> Result<(), String> {
    let definition = &environment.spec.roles[index];
    let role = &mut environment.roles[index];
    role.agent = SandboxReadiness::Pending;
    role.network = SandboxReadiness::Pending;
    role.application = SandboxReadiness::Pending;
    role.services.clear();
    role.error = None;
    if record.state != VmState::Running {
        return Ok(());
    }
    let runtime = guest::binding(state, record.id)
        .map_err(|_| "Environment guest has no current running runtime")?;
    role.runtime = Some(runtime);
    let address = role
        .ipv4
        .as_deref()
        .ok_or("Environment guest has no IPv4 lease")?;
    let network_failed = state
        .reconciliation
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&record.id)
        .is_some_and(|r| {
            matches!(
                r.outcome,
                VmReconciliationOutcome::NetworkFailed | VmReconciliationOutcome::Mismatched
            )
        });
    role.network = if network_failed {
        SandboxReadiness::Failed
    } else {
        SandboxReadiness::Ready
    };
    let directory = state
        .sandbox
        .root
        .join("environments")
        .join(environment.id.to_string())
        .join(&role.role);
    std::fs::create_dir_all(&directory)
        .map_err(|_| "Cannot create private environment control directory")?;
    match guest::capabilities(state, &runtime, &directory).await {
        Ok(capabilities) => {
            let supported = capabilities.execution
                && match definition.execution_mode {
                    SandboxExecutionMode::PersistentWorkspace => true,
                    SandboxExecutionMode::AgentSandbox {
                        runtime: SandboxContainerRuntime::Docker,
                        ..
                    } => capabilities.docker,
                    SandboxExecutionMode::AgentSandbox {
                        runtime: SandboxContainerRuntime::Nspawn,
                        ..
                    } => capabilities.nspawn,
                };
            let supported = if supported {
                guest::mode_supported(state, &runtime, &directory, &definition.execution_mode)
                    .await
                    .unwrap_or(false)
            } else {
                false
            };
            role.agent = if supported {
                SandboxReadiness::Ready
            } else {
                SandboxReadiness::Unsupported
            };
            if !supported {
                role.error = Some(
                    capabilities
                        .unsupported
                        .unwrap_or_else(|| "Selected guest execution mode or exact pinned container image is unavailable".into()),
                );
            }
        }
        Err(_) => {
            role.agent = SandboxReadiness::Failed;
            role.error = Some("Authenticated guest capability probe failed".into());
        }
    }
    let mut applications_ready = true;
    for service in &definition.services {
        let ready = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::net::TcpStream::connect((address, service.port)),
        )
        .await
        .is_ok_and(|result| result.is_ok());
        if service.readiness && !ready {
            applications_ready = false
        }
        role.services.push(SandboxDiscoveredService {
            name: service.name.clone(),
            address: address.into(),
            port: service.port,
            ready,
        });
    }
    role.application = if applications_ready {
        SandboxReadiness::Ready
    } else {
        SandboxReadiness::Pending
    };
    Ok(())
}

/// Capture only a fully ready exact generation; callers still authorize project/resource use.
pub fn binding(
    state: &AppState,
    id: Uuid,
    role: Option<&str>,
) -> Result<
    (
        SandboxEnvironmentRef,
        super::GuestBinding,
        SandboxExecutionMode,
    ),
    String,
> {
    let environment = load(state, id)?;
    if environment.state != SandboxEnvironmentState::Ready {
        return Err("Environment is not ready; reconcile before dispatch".into());
    }
    let index = match role {
        Some(role) => environment
            .roles
            .iter()
            .position(|r| r.role == role)
            .ok_or("Environment role does not exist")?,
        None if environment.roles.len() == 1 => 0,
        None => return Err("Select an explicit environment role for this task".into()),
    };
    let selected = &environment.roles[index];
    if !ready(selected) {
        return Err("Environment agent, network and application readiness must all pass".into());
    }
    let current = vm(state, selected.vm_id).ok_or("Environment VM does not exist")?;
    verify_vm(state, &environment, index, &current)?;
    let runtime =
        guest::binding(state, selected.vm_id).map_err(|_| "Environment runtime is unavailable")?;
    if selected.runtime != Some(runtime) {
        return Err("Environment guest restarted; reconcile readiness before dispatch".into());
    }
    Ok((
        SandboxEnvironmentRef {
            id,
            generation: environment.generation,
            role: selected.role.clone(),
        },
        runtime,
        environment.spec.roles[index].execution_mode.clone(),
    ))
}

/// Recheck admission after grants, manifest lifecycle or boot identity may have changed.
pub fn verify_binding(
    state: &AppState,
    reference: &SandboxEnvironmentRef,
    runtime: SandboxRuntimeRef,
) -> Result<(), String> {
    let (current, guest, _) = binding(state, reference.id, Some(&reference.role))?;
    if current != *reference || guest != runtime {
        return Err("Environment generation or runtime changed after task admission".into());
    }
    Ok(())
}

/// Discover only explicitly reachable services of the captured environment generation.
pub fn service_environment(
    state: &AppState,
    reference: &SandboxEnvironmentRef,
) -> Result<BTreeMap<String, String>, String> {
    let environment = load(state, reference.id)?;
    if environment.generation != reference.generation {
        return Err("Environment discovery generation changed".into());
    }
    let mut values = BTreeMap::new();
    for allow in environment
        .spec
        .allow
        .iter()
        .filter(|allow| allow.from == reference.role)
    {
        let peer = environment
            .roles
            .iter()
            .find(|role| role.role == allow.to)
            .ok_or("Environment discovery role disappeared")?;
        for service in peer
            .services
            .iter()
            .filter(|service| service.port == allow.port)
        {
            service
                .address
                .parse::<std::net::Ipv4Addr>()
                .map_err(|_| "Invalid environment discovery address")?;
            let prefix = format!(
                "FIRECRAB_SERVICE_{}_{}",
                allow.to.replace('-', "_").to_ascii_uppercase(),
                service.name.replace('-', "_").to_ascii_uppercase()
            );
            values.insert(format!("{prefix}_HOST"), service.address.clone());
            values.insert(format!("{prefix}_PORT"), service.port.to_string());
        }
    }
    Ok(values)
}

/// Stop or resume retains the same dedicated disks and immutable ownership ledger.
pub async fn set_running(
    state: &AppState,
    id: Uuid,
    running: bool,
) -> Result<SandboxEnvironment, String> {
    let guard = state.lock_vm(id).await;
    let mut environment = load(state, id)?;
    if matches!(
        environment.state,
        SandboxEnvironmentState::Deleted | SandboxEnvironmentState::CleanupPending
    ) {
        return Err("Environment is being deleted".into());
    }
    if running {
        environment.state = SandboxEnvironmentState::Provisioning;
        save(state, &mut environment)?;
        drop(guard);
        return reconcile(state, id).await;
    }
    // Serialize the no-active-attempt check with task admission and persist the stop fence first.
    let admission = state.sandbox.admission.lock().await;
    if state
        .store
        .sandbox_project_busy(environment.project_id)
        .map_err(|_| "Cannot inspect active tasks")?
    {
        return Err("Reconcile or cancel active tasks before stopping their environment".into());
    }
    environment.state = SandboxEnvironmentState::Stopped;
    save(state, &mut environment)?;
    drop(admission);
    for index in (0..environment.roles.len()).rev() {
        let Some(record) = vm(state, environment.roles[index].vm_id) else {
            continue;
        };
        verify_vm(state, &environment, index, &record)?;
        stop(state, &record).await?;
        environment.roles[index].agent = SandboxReadiness::Pending;
        environment.roles[index].network = SandboxReadiness::Pending;
        environment.roles[index].application = SandboxReadiness::Pending;
        for service in &mut environment.roles[index].services {
            service.ready = false;
        }
    }
    save(state, &mut environment)?;
    Ok(environment)
}

async fn stop(state: &AppState, record: &VmRecord) -> Result<(), String> {
    if matches!(record.state, VmState::Running | VmState::Paused) {
        let _ = vms::stop_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(record.id.to_string()),
        )
        .await
        .map_err(|e| e.redacted().description())?;
    } else if matches!(
        record.state,
        VmState::Starting | VmState::Pausing | VmState::Resuming | VmState::Stopping
    ) {
        return Err("Environment VM lifecycle is still uncertain; reconcile before cleanup".into());
    }
    Ok(())
}

/// Verify durable output and artifact bytes before granting cleanup permission.
pub fn verify_export(
    state: &AppState,
    environment: &SandboxEnvironment,
) -> Result<Vec<Uuid>, String> {
    let ids = state
        .store
        .sandbox_environment_task_ids(environment.id)
        .map_err(|_| "Cannot read environment task references")?;
    for id in &ids {
        let saved = state
            .store
            .sandbox_environment_task(*id)
            .map_err(|_| "Cannot read environment result")?
            .ok_or("Environment task result disappeared")?;
        if saved.task.environment.as_ref().is_none_or(|reference| {
            reference.id != environment.id || reference.generation != environment.generation
        }) {
            return Err("Environment result generation does not match".into());
        }
        if saved.task.state.active()
            || saved.guest_pending
            || saved.guest_staged
            || !saved.results_exported
        {
            return Err(
                "Required environment result export is incomplete; all guest data is retained"
                    .into(),
            );
        }
        super::artifacts::verify_export(&saved)?;
    }
    Ok(ids)
}

pub async fn export(state: &AppState, id: Uuid) -> Result<SandboxEnvironmentExport, String> {
    let _guard = state.lock_vm(id).await;
    let mut environment = load(state, id)?;
    let tasks = verify_export(state, &environment)?;
    environment.exported_at = Some(now());
    save(state, &mut environment)?;
    Ok(SandboxEnvironmentExport {
        environment: SandboxEnvironmentRef {
            id,
            generation: environment.generation,
            role: String::new(),
        },
        task_ids: tasks,
        exported_at: environment.exported_at.unwrap_or_default(),
    })
}

/// Export first, then remove only owned VM/SG/network resources. Never remove shared images.
pub async fn cleanup(
    state: &AppState,
    id: Uuid,
    authorize_workspace_deletion: bool,
) -> Result<SandboxEnvironment, String> {
    let _guard = state.lock_vm(id).await;
    let mut environment = load(state, id)?;
    if environment.state == SandboxEnvironmentState::Deleted {
        return Ok(environment);
    }
    if retained(&environment.spec) && !authorize_workspace_deletion {
        return Err("Explicit authorization is required to delete retained Workspace data".into());
    }
    let admission = state.sandbox.admission.lock().await;
    verify_export(state, &environment)?;
    if state
        .store
        .sandbox_project_busy(environment.project_id)
        .map_err(|_| "Cannot inspect active project tasks")?
    {
        return Err("Active or unreconciled project tasks prevent environment cleanup".into());
    }
    environment.state = SandboxEnvironmentState::CleanupPending;
    environment.exported_at = Some(now());
    save(state, &mut environment)?;
    drop(admission);
    for index in (0..environment.roles.len()).rev() {
        let reserved = &environment.roles[index];
        owns(state, &environment, "vm", reserved.vm_id)?;
        if let Some(record) = vm(state, reserved.vm_id) {
            verify_vm(state, &environment, index, &record)?;
            stop(state, &record).await?;
            vms::delete_vm(
                State(state.clone()),
                Extension(RequestId(Uuid::new_v4())),
                Path(record.id.to_string()),
            )
            .await
            .map_err(|e| e.redacted().description())?;
        }
        owns(
            state,
            &environment,
            "security_group",
            reserved.security_group_id,
        )?;
        if let Some(group) = state
            .store
            .security_group(reserved.security_group_id)
            .map_err(|_| "Cannot inspect owned security group")?
        {
            if group.micro_network_id != environment.network_id
                || group.name != name(&environment, &reserved.role)
            {
                return Err("Refusing cleanup of changed security group ownership".into());
            }
            state
                .store
                .delete_security_group(group.id)
                .map_err(|_| "Cannot delete owned security group")?;
        }
    }
    owns(state, &environment, "network", environment.network_id)?;
    if let Some(network) = state
        .store
        .micro_network(environment.network_id)
        .map_err(|_| "Cannot inspect owned network")?
    {
        if network.name != name(&environment, "network")
            || network.subnet_cidr != environment.spec.subnet_cidr
            || network.internet_enabled
            || network.ipv6_cidr.is_some()
        {
            return Err("Refusing cleanup of changed network identity or policy".into());
        }
        micro_networks::delete_micro_network(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(environment.network_id.to_string()),
        )
        .await
        .map_err(|e| e.redacted().description())?;
    }
    environment.state = SandboxEnvironmentState::Deleted;
    for role in &mut environment.roles {
        role.agent = SandboxReadiness::Pending;
        role.network = SandboxReadiness::Pending;
        role.application = SandboxReadiness::Pending;
        role.services.clear();
    }
    save(state, &mut environment)?;
    Ok(environment)
}

/// TTL/restart cleanup is bounded and never gains authorization to delete a Workspace.
pub async fn reconcile_cleanup(state: AppState) {
    let Ok(ids) = state.store.sandbox_environment_cleanup_ids(now()) else {
        return;
    };
    for id in ids {
        if let Err(error) = cleanup(&state, id, false).await {
            tracing::warn!(environment_id=%id,%error,"Environment cleanup retained data");
        }
    }
}

/// Resume interrupted resource creation and TTL cleanup after API restart.
/// Failed task/guest commands remain the task engine's explicit reconciliation responsibility.
pub fn start_maintenance(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Ok(ids) = state.store.sandbox_environment_provisioning_ids() {
                for id in ids {
                    if let Err(error) = reconcile(&state, id).await {
                        tracing::warn!(environment_id=%id,%error,"Cannot resume environment provisioning");
                    }
                }
            }
            reconcile_cleanup(state.clone()).await;
        }
    });
}

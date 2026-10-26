//! Dependency-ordered execution through existing network, VM and service operations.

use std::collections::BTreeMap;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::{Extension, Json};
use firecrab_api_types::{
    CreateMicroNetworkRequest, CreateVmRequest, DeployMicroServiceRequest, DeployStrategy,
    MicroServiceSpec, ServicePhase, UpdateMicroNetworkRequest, UpdateServiceRequest,
    UpdateVmResourcesRequest, VmState,
};
use firecrab_stack::{
    Action, ApplyFailure, ApplyState, ApplyStatus, Change, Inventory, Plan, PlanInputs,
    ResourceKind, Stack, StackRecord, StepState, TemplatePin,
};
use uuid::Uuid;

use super::{database, fail_status, inventory};
use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::handlers::{micro_networks, micro_services, vms};
use crate::model::{VmPurpose, VmRecord};
use crate::server::RequestId;
use crate::state::AppState;

pub(super) async fn preflight(
    state: &AppState,
    stack: &Stack,
    plan: &Plan,
    allow_stop: bool,
    id: Uuid,
) -> Result<(), AppError> {
    // Verify bytes before any network, VM or service mutation. Registry reads are
    // metadata-only during planning; open_verified caches unchanged artifact hashes.
    let templates = state.templates.clone();
    let desired = stack.clone();
    let inputs = plan.inputs.clone();
    tokio::task::spawn_blocking(move || {
        for (key, pin) in &inputs.templates {
            let template = pinned_template(&templates, &desired.micro_vms[key].template, pin, id)?;
            for artifact in std::iter::once(&template.kernel)
                .chain(std::iter::once(&template.rootfs))
                .chain(template.initrd.iter())
            {
                templates.open_verified(artifact).map_err(|_| {
                    conflict(
                        "stack_template_changed",
                        "approved template artifacts are unavailable or changed",
                        id,
                    )
                })?;
            }
        }
        Ok::<_, AppError>(())
    })
    .await
    .map_err(|_| AppError::internal(id))??;
    let networks = network_ids(&plan.changes);
    for change in &plan.changes {
        match change.kind {
            ResourceKind::Vm if change.action == Action::Create => {
                let desired = &stack.micro_vms[&change.key];
                if desired.template == crate::microboot::MICROBOOT_ALIAS {
                    return Err(conflict(
                        "protected_stack_template",
                        "internal templates cannot be created by a stack",
                        id,
                    ));
                }
                let req = vm_create(desired, networks[desired.network.as_str()]);
                let template = pinned_template(
                    &state.templates,
                    &desired.template,
                    &plan.inputs.templates[&change.key],
                    id,
                )?;
                let mut fields = vms::validate_create_for(&req, Some(&template), state);
                if plan.changes.iter().any(|c| {
                    c.kind == ResourceKind::Network
                        && c.key == desired.network
                        && c.action == Action::Create
                }) {
                    fields.remove("microNetworkId");
                }
                if !fields.is_empty() {
                    return Err(AppError::validation(fields, id));
                }
            }
            ResourceKind::Vm if change.action == Action::Update => {
                let vm = editable_vm(state, exact_id(change, id)?, allow_stop, id)?;
                let fields =
                    vms::validate_update(&vm_update(&stack.micro_vms[&change.key]), vm.disk_gb);
                if !fields.is_empty() {
                    return Err(AppError::validation(fields, id));
                }
            }
            ResourceKind::Service
                if matches!(
                    change.action,
                    Action::Create | Action::Update | Action::Deploy
                ) =>
            {
                let desired = &stack.micro_services[&change.key];
                micro_services::validate(&desired.config, id)?;
                if let Some(policy) = &desired.config.auto_deploy {
                    crate::service_runtime::auto_deploy::validate_auth(state, policy, id).await?;
                }
                let request = DeployMicroServiceRequest {
                    micro_network_id: Some(networks[desired.network.as_str()]),
                    host_port: desired.host_port,
                };
                let fields = crate::service_runtime::deployment_fields(
                    &desired.config,
                    &request,
                    state.api_port,
                );
                if !fields.is_empty() {
                    return Err(AppError::validation(fields, id));
                }
                crate::network_policy::validate_selection(
                    state,
                    desired.config.security_group_id,
                    request.micro_network_id,
                    id,
                )?;
                if change.action != Action::Create {
                    let service = exact_id(change, id)?;
                    if !supports_live_update(
                        state,
                        service,
                        &desired.config,
                        networks[desired.network.as_str()],
                        id,
                    )
                    .await?
                    {
                        service_stop_needed(state, service, allow_stop, id).await?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) async fn run(
    state: AppState,
    stack: Stack,
    mut status: ApplyStatus,
    record: StackRecord,
    observed: Inventory,
    allow_stop: bool,
) {
    let result = run_steps(&state, &stack, &mut status, &record, &observed, allow_stop).await;
    match result {
        Ok(()) => status.state = ApplyState::Succeeded,
        Err(error) => fail_status(
            &mut status,
            ApplyFailure {
                code: error.code().into(),
                message:
                    "inspect the recorded UUIDs; completed resources and ownership were retained"
                        .into(),
                fields: error.field_names(),
            },
        ),
    }
    if save(&state, &status).await.is_err() {
        tracing::error!(operation_id=%status.operation_id, "could not record terminal stack apply status");
    }
}

async fn run_steps(
    state: &AppState,
    stack: &Stack,
    status: &mut ApplyStatus,
    record: &StackRecord,
    observed: &Inventory,
    allow_stop: bool,
) -> Result<(), AppError> {
    let id = status.operation_id;
    status.state = ApplyState::Running;
    save(state, status).await?;
    let current = observe(state, id).await?;
    if firecrab_stack::fingerprint(stack, record, observed)
        != firecrab_stack::fingerprint(stack, record, &current)
    {
        return Err(conflict(
            "stale_stack_plan",
            "host configuration changed before execution",
            id,
        ));
    }
    let changes = status
        .changes
        .iter()
        .map(|s| s.change.clone())
        .collect::<Vec<_>>();
    let networks = network_ids(&changes);
    for (index, change) in changes.iter().enumerate() {
        status.changes[index].state = StepState::Running;
        save(state, status).await?;
        if change.action != Action::Retain {
            verify_current(
                stack,
                record,
                observed,
                &observe(state, id).await?,
                change,
                id,
            )?;
        }
        execute(
            state,
            stack,
            change,
            &networks,
            &status.inputs,
            allow_stop,
            id,
        )
        .await?;
        status.changes[index].state = StepState::Succeeded;
        save(state, status).await?;
    }
    Ok(())
}

async fn execute(
    state: &AppState,
    stack: &Stack,
    change: &Change,
    networks: &BTreeMap<&str, Uuid>,
    inputs: &PlanInputs,
    allow_stop: bool,
    id: Uuid,
) -> Result<(), AppError> {
    if change.action == Action::Retain {
        return Ok(());
    }
    let resource_id = exact_id(change, id)?;
    let request_id = RequestId(id);
    match (change.kind, change.action) {
        (ResourceKind::Network, Action::Create) => {
            let desired = &stack.micro_networks[&change.key];
            let _ = micro_networks::create_micro_network_with_id(
                state.clone(),
                request_id,
                CreateMicroNetworkRequest {
                    dns_mode: firecrab_api_types::MicroNetworkDnsMode::LegacyForward,
                    name: desired.name.clone(),
                    subnet_cidr: desired.subnet_cidr.clone(),
                    internet_enabled: desired.internet_enabled,
                    uplink: None,
                    ipv6_cidr: None,
                    ipv6_address_mode: None,
                },
                resource_id,
            )
            .await?;
        }
        (ResourceKind::Network, Action::Update) => {
            let _ = micro_networks::update_micro_network(
                State(state.clone()),
                Extension(request_id),
                Path(resource_id.to_string()),
                ValidatedJson(UpdateMicroNetworkRequest {
                    internet_enabled: stack.micro_networks[&change.key].internet_enabled,
                    uplink: None,
                }),
            )
            .await?;
        }
        (ResourceKind::Vm, Action::Create) => {
            let desired = &stack.micro_vms[&change.key];
            if desired.template == crate::microboot::MICROBOOT_ALIAS {
                return Err(conflict(
                    "protected_stack_template",
                    "internal templates cannot be created by a stack",
                    id,
                ));
            }
            let pin = inputs.templates.get(&change.key).ok_or_else(|| {
                conflict(
                    "stack_template_changed",
                    "approved template identity is missing",
                    id,
                )
            })?;
            let template = pinned_template(&state.templates, &desired.template, pin, id)?;
            let _ = vms::create_vm_from_template(
                state.clone(),
                request_id,
                vm_create(desired, networks[desired.network.as_str()]),
                resource_id,
                Some(template),
            )
            .await?;
        }
        (ResourceKind::Vm, Action::Update) => {
            let pin = inputs.templates.get(&change.key).ok_or_else(|| {
                conflict(
                    "stack_template_changed",
                    "approved template identity is missing",
                    id,
                )
            })?;
            pinned_template(
                &state.templates,
                &stack.micro_vms[&change.key].template,
                pin,
                id,
            )?;
            let vm = editable_vm(state, resource_id, allow_stop, id)?;
            let restart = vm.state == VmState::Running;
            if restart {
                let _ = vms::stop_vm(
                    State(state.clone()),
                    Extension(request_id),
                    Path(resource_id.to_string()),
                )
                .await?;
            }
            let _ = vms::update_vm(
                State(state.clone()),
                Extension(request_id),
                Path(resource_id.to_string()),
                ValidatedJson(vm_update(&stack.micro_vms[&change.key])),
            )
            .await?;
            if restart {
                let Json(started) = vms::start_vm(
                    State(state.clone()),
                    Extension(request_id),
                    Path(resource_id.to_string()),
                )
                .await?;
                if started.state != VmState::Running {
                    return Err(conflict(
                        "stack_vm_start_failed",
                        "owned VM did not reach running",
                        id,
                    ));
                }
            }
        }
        (ResourceKind::Service, Action::Create | Action::Update | Action::Deploy) => {
            let declared = &stack.micro_services[&change.key];
            let store = state.store.clone();
            let current = database(id, move || store.service_deployment(resource_id)).await?;
            let mut desired = declared.clone();
            desired.config =
                firecrab_stack::effective_service_config(&declared.config, current.as_ref());
            if change.action == Action::Create {
                let _ = micro_services::create_with_id(
                    state.clone(),
                    request_id,
                    desired.config.clone(),
                    resource_id,
                )
                .await?;
            } else if supports_live_update(
                state,
                resource_id,
                &desired.config,
                networks[desired.network.as_str()],
                id,
            )
            .await?
            {
                let store = state.store.clone();
                let expected = database(id, move || store.service_deployment(resource_id))
                    .await?
                    .ok_or_else(|| {
                        conflict(
                            "stack_deployment_missing",
                            "active release disappeared before Deploy",
                            id,
                        )
                    })?
                    .id;
                let (_, Json(deployment)) = crate::service_runtime::deploy::update_from_stack(
                    state.clone(),
                    request_id,
                    resource_id,
                    UpdateServiceRequest {
                        expected_release_id: Some(expected),
                        operation_id: deploy_operation_id(id, resource_id),
                        spec: desired.config.clone(),
                        strategy: DeployStrategy::Rolling,
                    },
                    Some(id),
                )
                .await?;
                wait_service(
                    state,
                    resource_id,
                    deployment.id,
                    true,
                    Duration::from_secs(
                        u64::from(desired.config.startup_timeout)
                            * u64::from(desired.config.replicas)
                            + 1800,
                    ),
                    id,
                )
                .await?;
                return Ok(());
            } else if service_stop_needed(state, resource_id, allow_stop, id).await? {
                let (_, Json(stopping)) = crate::service_runtime::stop_from_stack(
                    state.clone(),
                    id,
                    resource_id,
                    Some(id),
                )
                .await?;
                wait_service(
                    state,
                    resource_id,
                    stopping.id,
                    false,
                    Duration::from_secs(3600),
                    id,
                )
                .await?;
            }
            if change.action == Action::Update {
                let _ = micro_services::update_from_stack(
                    state.clone(),
                    id,
                    resource_id,
                    desired.config.clone(),
                    Some(id),
                )
                .await?;
            }
            let (_, Json(deployment)) = crate::service_runtime::deploy_from_stack(
                state.clone(),
                id,
                resource_id,
                DeployMicroServiceRequest {
                    micro_network_id: Some(networks[desired.network.as_str()]),
                    host_port: desired.host_port,
                },
                Some(id),
            )
            .await?;
            wait_service(
                state,
                resource_id,
                deployment.id,
                true,
                Duration::from_secs(u64::from(desired.config.startup_timeout) + 1800),
                id,
            )
            .await?;
        }
        (ResourceKind::Service, Action::Noop) => {
            let Json(deployment) = crate::service_runtime::get(
                State(state.clone()),
                Extension(request_id),
                Path(resource_id.to_string()),
            )
            .await?;
            if deployment.is_none_or(|d| {
                d.phase != ServicePhase::Running
                    || d.instances.is_empty()
                    || d.instances
                        .iter()
                        .any(|i| !i.healthy || i.state != VmState::Running)
            }) {
                return Err(conflict(
                    "stack_service_not_healthy",
                    "configuration matches but the service is not healthy; inspect its deployment",
                    id,
                ));
            }
        }
        (_, Action::Noop) => {}
        _ => {
            return Err(conflict(
                "unsupported_stack_plan",
                "automatic replacement and pruning are not supported",
                id,
            ));
        }
    }
    Ok(())
}

fn vm_create(desired: &firecrab_stack::Vm, network: Uuid) -> CreateVmRequest {
    CreateVmRequest {
        block_storages: vec![],
        name: desired.name.clone(),
        template: desired.template.clone(),
        ram: desired.ram,
        cpu: desired.cpu,
        disk_gb: desired.disk_gb,
        egress_policy: desired.egress_policy,
        micro_network_id: network,
        security_group_id: None,
        storage_root: desired.storage_root.clone(),
        shell_ids: vec![],
        port_forwards: vec![],
        env: desired.env.clone(),
    }
}

fn vm_update(desired: &firecrab_stack::Vm) -> UpdateVmResourcesRequest {
    UpdateVmResourcesRequest {
        ram: desired.ram,
        cpu: desired.cpu,
        disk_gb: desired.disk_gb,
        egress_policy: desired.egress_policy,
        env: Some(desired.env.clone()),
        security_group_id: None,
    }
}

fn editable_vm(
    state: &AppState,
    resource: Uuid,
    allow_stop: bool,
    id: Uuid,
) -> Result<VmRecord, AppError> {
    let vm = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&resource)
        .cloned()
        .ok_or_else(|| AppError::not_found(id))?;
    if vm.purpose != VmPurpose::Instance {
        return Err(conflict(
            "protected_stack_vm",
            "only owned standalone VMs may be changed",
            id,
        ));
    }
    if vm.state == VmState::Running && !allow_stop {
        return Err(conflict(
            "stack_stop_permission_required",
            "review interruption and retry with allowStop to stop and restart the owned VM",
            id,
        ));
    }
    if vm.state != VmState::Running && !vm.state.can_edit_resources() {
        return Err(conflict(
            "stack_vm_not_editable",
            "wait for VM transitions; paused VMs must be explicitly resumed or stopped",
            id,
        ));
    }
    Ok(vm)
}

async fn service_stop_needed(
    state: &AppState,
    service: Uuid,
    allow_stop: bool,
    id: Uuid,
) -> Result<bool, AppError> {
    let store = state.store.clone();
    let deployment = database(id, move || store.service_deployment(service)).await?;
    let Some(deployment) = deployment else {
        return Ok(false);
    };
    if deployment.phase.is_busy() {
        return Err(conflict(
            "service_busy",
            "wait for the current service operation",
            id,
        ));
    }
    let needed = !deployment.instances.is_empty()
        || deployment.domain_route_owned
        || deployment.internal_route_owned
        || deployment
            .update
            .as_ref()
            .is_some_and(|update| !update.completed);
    if needed && !allow_stop {
        return Err(conflict(
            "stack_stop_permission_required",
            "review interruption and retry with allowStop to replace the owned service replicas",
            id,
        ));
    }
    Ok(needed)
}

async fn supports_live_update(
    state: &AppState,
    service: Uuid,
    desired: &MicroServiceSpec,
    network: Uuid,
    id: Uuid,
) -> Result<bool, AppError> {
    let store = state.store.clone();
    let current = database(id, move || store.service_deployment(service)).await?;
    Ok(current.is_some_and(|current| {
        matches!(
            current.phase,
            ServicePhase::Running | ServicePhase::Degraded
        ) && current.domain_route_owned
            && current.spec.domain.is_some()
            && current.spec.domain == desired.domain
            && current.spec.name == desired.name
            && current.request.micro_network_id == Some(network)
            && current
                .update
                .as_ref()
                .is_none_or(|update| update.completed)
    }))
}

fn deploy_operation_id(apply: Uuid, service: Uuid) -> Uuid {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"firecrab.stack.deploy.v1");
    hash.update(apply.as_bytes());
    hash.update(service.as_bytes());
    let digest = hash.finalize();
    let mut bytes: [u8; 16] = digest[..16].try_into().expect("SHA256 prefix");
    bytes[6] = (bytes[6] & 15) | 128;
    bytes[8] = (bytes[8] & 63) | 128;
    Uuid::from_bytes(bytes)
}

async fn wait_service(
    state: &AppState,
    service: Uuid,
    deployment_id: Uuid,
    running: bool,
    timeout: Duration,
    id: Uuid,
) -> Result<(), AppError> {
    let poll = async {
        loop {
            let Json(current) = crate::service_runtime::get(
                State(state.clone()),
                Extension(RequestId(id)),
                Path(service.to_string()),
            )
            .await?;
            let current = current.ok_or_else(|| {
                conflict(
                    "stack_deployment_missing",
                    "inspect the owned service deployment",
                    id,
                )
            })?;
            if current.id != deployment_id {
                let store = state.store.clone();
                if database(id, move || store.service_attempt(deployment_id))
                    .await?
                    .is_some_and(|attempt| {
                        attempt.service_id == service && attempt.phase == ServicePhase::Failed
                    })
                {
                    return Err(conflict(
                        "stack_deployment_failed",
                        "Deploy failed and restored the previous release; inspect the exact operation",
                        id,
                    ));
                }
                return Err(conflict(
                    "stack_deployment_changed",
                    "service deployment identity changed during apply",
                    id,
                ));
            }
            if running
                && current.phase == ServicePhase::Running
                && current.instances.len() == usize::from(current.spec.replicas)
                && current
                    .instances
                    .iter()
                    .all(|i| i.healthy && i.state == VmState::Running)
            {
                return Ok(());
            }
            if !running
                && current.phase == ServicePhase::Stopped
                && current.instances.is_empty()
                && !current.domain_route_owned
                && !current.internal_route_owned
            {
                return Ok(());
            }
            if !current.phase.is_busy() {
                return Err(conflict(
                    "stack_deployment_failed",
                    "service did not reach the requested healthy or cleaned state",
                    id,
                ));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    };
    tokio::time::timeout(timeout, poll).await.map_err(|_| {
        conflict(
            "stack_deployment_timeout",
            "inspect the owned service; its backend operation may still be completing",
            id,
        )
    })?
}

fn network_ids(changes: &[Change]) -> BTreeMap<&str, Uuid> {
    changes
        .iter()
        .filter(|c| c.kind == ResourceKind::Network && c.action != Action::Retain)
        .filter_map(|c| c.resource_id.map(|id| (c.key.as_str(), id)))
        .collect()
}

fn exact_id(change: &Change, id: Uuid) -> Result<Uuid, AppError> {
    change.resource_id.ok_or_else(|| {
        conflict(
            "stack_resource_identity_missing",
            "a step requires an exact owned UUID",
            id,
        )
    })
}

async fn observe(state: &AppState, id: Uuid) -> Result<Inventory, AppError> {
    let store = state.store.clone();
    let templates = state.templates.clone();
    database(id, move || inventory(&store, &templates)).await
}

async fn save(state: &AppState, status: &ApplyStatus) -> Result<(), AppError> {
    let store = state.store.clone();
    let record = status.clone();
    if !database(status.operation_id, move || store.save_stack_apply(&record)).await? {
        return Err(conflict(
            "stack_apply_interrupted",
            "operation is no longer active; inspect its recorded status",
            status.operation_id,
        ));
    }
    Ok(())
}

/// Recheck only this step's resource; previous successful steps may have changed inventory.
fn verify_current(
    stack: &Stack,
    record: &StackRecord,
    before: &Inventory,
    current: &Inventory,
    change: &Change,
    id: Uuid,
) -> Result<(), AppError> {
    if change.action == Action::Create {
        let collision = match change.kind {
            ResourceKind::Network => current.networks.iter().any(|n| {
                Some(n.id) == change.resource_id || n.name == stack.micro_networks[&change.key].name
            }),
            ResourceKind::Vm => current.vms.iter().any(|v| {
                Some(v.id) == change.resource_id || v.name == stack.micro_vms[&change.key].name
            }),
            ResourceKind::Service => current.services.iter().any(|s| {
                Some(s.service.id) == change.resource_id
                    || s.service.spec.name == stack.micro_services[&change.key].config.name
            }),
        };
        if collision {
            return Err(conflict(
                "stale_stack_plan",
                "resource identity or name appeared after planning",
                id,
            ));
        }
        return Ok(());
    }
    let select = |inventory: &Inventory| Inventory {
        templates: inventory.templates.clone(),
        networks: inventory
            .networks
            .iter()
            .filter(|n| change.kind == ResourceKind::Network && Some(n.id) == change.resource_id)
            .cloned()
            .collect(),
        vms: inventory
            .vms
            .iter()
            .filter(|v| change.kind == ResourceKind::Vm && Some(v.id) == change.resource_id)
            .cloned()
            .collect(),
        services: inventory
            .services
            .iter()
            .filter(|s| {
                change.kind == ResourceKind::Service && Some(s.service.id) == change.resource_id
            })
            .cloned()
            .collect(),
    };
    if firecrab_stack::fingerprint(stack, record, &select(before))
        != firecrab_stack::fingerprint(stack, record, &select(current))
    {
        return Err(conflict(
            "stale_stack_plan",
            "owned resource configuration changed before this step",
            id,
        ));
    }
    Ok(())
}

fn conflict(code: &'static str, message: &'static str, id: Uuid) -> AppError {
    AppError::conflict(code, message, id)
}

fn pinned_template(
    registry: &crate::templates::TemplateRegistry,
    alias: &str,
    pin: &TemplatePin,
    id: Uuid,
) -> Result<std::sync::Arc<crate::templates::TemplateVersion>, AppError> {
    registry
        .resolve_version(alias, &pin.version)
        .filter(|template| template.stack_pin() == *pin)
        .ok_or_else(|| {
            conflict(
                "stack_template_changed",
                "approved template identity is unavailable or changed; review a fresh plan",
                id,
            )
        })
}

#[cfg(test)]
mod tests;

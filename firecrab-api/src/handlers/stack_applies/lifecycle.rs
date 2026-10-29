//! Expand reviewed replacement decisions into journaled, UUID-specific steps.

use std::collections::BTreeSet;

use axum::{
    Extension,
    extract::{Path, State},
};
use firecrab_stack::{
    Action, ApplyStatus, ApplyStep, Binding, Inventory, Plan, ResourceKind, Stack, StepState,
    VmDiskIdentity,
};
use uuid::Uuid;

use super::database;
use crate::{
    error::AppError,
    handlers::{micro_networks, micro_services, vms},
    model::VmPurpose,
    server::RequestId,
    state::AppState,
};

pub(super) fn steps(plan: &Plan, observed: &Inventory) -> Vec<ApplyStep> {
    let mut deletes = Vec::new();
    let mut remaining = Vec::new();
    for change in &plan.changes {
        let mut step = ApplyStep {
            change: change.clone(),
            state: StepState::Pending,
            replacement_for: None,
            vm_disk: None,
        };
        if matches!(change.action, Action::Delete | Action::Replace) {
            step.change.action = Action::Delete;
            if change.kind == ResourceKind::Vm {
                step.vm_disk = observed
                    .vms
                    .iter()
                    .find(|vm| Some(vm.id) == change.resource_id)
                    .map(|vm| VmDiskIdentity {
                        storage_root: vm.storage_root.clone(),
                        generation: vm.disk_generation,
                    });
            }
            deletes.push(step);
            if change.action == Action::Delete {
                continue;
            }
            step = ApplyStep {
                change: change.clone(),
                state: StepState::Pending,
                replacement_for: change.resource_id,
                vm_disk: None,
            };
            step.change.action = Action::Create;
        }
        if step.change.action == Action::Create {
            step.change.resource_id = Some(Uuid::new_v4());
        }
        remaining.push(step);
    }
    deletes.sort_by(|a, b| (b.change.kind, &b.change.key).cmp(&(a.change.kind, &a.change.key)));
    deletes.extend(remaining);
    deletes
}

pub(super) async fn preflight(
    state: &AppState,
    stack: &Stack,
    steps: &[ApplyStep],
    allow_stop: bool,
    id: Uuid,
) -> Result<(), AppError> {
    let removed = steps
        .iter()
        .filter(|step| step.change.action == Action::Delete)
        .collect::<Vec<_>>();
    if removed.is_empty() {
        return Ok(());
    }
    let removed_networks = removed
        .iter()
        .filter(|s| s.change.kind == ResourceKind::Network)
        .filter_map(|s| s.change.resource_id)
        .collect::<BTreeSet<_>>();
    let current_networks = state
        .store
        .list_micro_networks()
        .map_err(|_| AppError::internal(id))?;
    for step in steps
        .iter()
        .filter(|s| s.change.kind == ResourceKind::Network && s.change.action == Action::Create)
    {
        let desired = &stack.micro_networks[&step.change.key];
        let subnet = crate::ipam::SubnetSpec::parse(
            step.change.resource_id.expect("reserved create UUID"),
            &desired.subnet_cidr,
        )
        .ok_or_else(|| conflict("invalid_stack_subnet", id))?;
        if current_networks
            .iter()
            .filter(|n| !removed_networks.contains(&n.id))
            .any(|n| {
                crate::ipam::SubnetSpec::parse(n.id, &n.subnet_cidr)
                    .is_some_and(|other| subnet.overlaps(&other))
            })
        {
            return Err(conflict("stack_external_dependency", id));
        }
    }
    let store = state.store.clone();
    let (vms, services, leases) = database(id, move || {
        let vms = store.load_all()?;
        let services = store
            .list_micro_services()?
            .into_iter()
            .map(|service| store.service_deployment(service.id))
            .collect::<Result<Vec<_>, _>>()?;
        let leases = store.stack_lifecycle_leases()?;
        Ok((
            vms,
            services.into_iter().flatten().collect::<Vec<_>>(),
            leases,
        ))
    })
    .await?;
    let mut removed_vms = removed
        .iter()
        .filter(|s| s.change.kind == ResourceKind::Vm)
        .filter_map(|s| s.change.resource_id)
        .collect::<BTreeSet<_>>();
    for step in &removed {
        let resource = step
            .change
            .resource_id
            .ok_or_else(|| conflict("stack_resource_identity_missing", id))?;
        if step.change.kind == ResourceKind::Service
            && let Some(deployment) = services.iter().find(|d| d.service_id == resource)
        {
            if deployment.phase.is_busy()
                || deployment.update.as_ref().is_some_and(|u| !u.completed)
                || deployment
                    .recovery
                    .as_ref()
                    .is_some_and(|health| health.repair.is_some())
            {
                return Err(conflict("stack_service_recovery_required", id));
            }
            if (!deployment.instances.is_empty()
                || deployment.domain_route_owned
                || deployment.internal_route_owned)
                && !allow_stop
            {
                return Err(conflict("stack_stop_permission_required", id));
            }
            removed_vms.extend(deployment.instances.iter().map(|instance| instance.vm_id));
        }
        if step.change.kind == ResourceKind::Vm
            && let Some(vm) = vms.get(&resource)
        {
            if vm.purpose != VmPurpose::Instance
                || services
                    .iter()
                    .any(|d| d.owned_vm_ids().any(|id| id == resource))
            {
                return Err(conflict("protected_stack_vm", id));
            }
            if vm.state == firecrab_api_types::VmState::Running && !allow_stop {
                return Err(conflict("stack_stop_permission_required", id));
            }
            if vm.state != firecrab_api_types::VmState::Running && !vm.state.can_delete() {
                return Err(conflict("stack_vm_not_editable", id));
            }
            if vm.state != firecrab_api_types::VmState::Running
                && crate::network_policy::vm_is_active(state, vm)
                    .map_err(|_| AppError::internal(id))?
            {
                return Err(conflict("stack_vm_in_use", id));
            }
            if !state
                .store
                .vm_block_storages(resource)
                .map_err(|_| AppError::internal(id))?
                .is_empty()
            {
                return Err(conflict("stack_external_volume_attached", id));
            }
        }
    }
    for step in removed
        .iter()
        .filter(|s| s.change.kind == ResourceKind::Network)
    {
        let resource = step.change.resource_id.expect("checked exact identities");
        if state
            .store
            .network_has_security_groups(resource)
            .map_err(|_| AppError::internal(id))?
            || vms
                .values()
                .any(|vm| vm.micro_network_id == resource && !removed_vms.contains(&vm.id))
            || leases.iter().any(|lease| {
                lease.1.is_none_or(|network| network == resource) && !removed_vms.contains(&lease.0)
            })
            || services.iter().any(|d| {
                d.request.micro_network_id == Some(resource)
                    && !removed.iter().any(|s| {
                        s.change.kind == ResourceKind::Service
                            && s.change.resource_id == Some(d.service_id)
                    })
            })
        {
            return Err(conflict("stack_external_dependency", id));
        }
    }
    Ok(())
}

pub(super) async fn delete(
    state: &AppState,
    status: &ApplyStatus,
    index: usize,
    allow_stop: bool,
) -> Result<ApplyStatus, AppError> {
    let id = status.operation_id;
    let step = &status.changes[index];
    let resource = step
        .change
        .resource_id
        .ok_or_else(|| conflict("stack_resource_identity_missing", id))?;
    let binding = Binding {
        kind: step.change.kind,
        key: step.change.key.clone(),
        resource_id: resource,
    };
    let store = state.store.clone();
    database(id, move || store.guard_stack_delete(id, index, &binding)).await?;
    match step.change.kind {
        ResourceKind::Service => {
            let store = state.store.clone();
            let deployment = database(id, move || store.service_deployment(resource)).await?;
            if deployment.as_ref().is_some_and(|d| {
                !d.instances.is_empty() || d.domain_route_owned || d.internal_route_owned
            }) {
                if !allow_stop {
                    return Err(conflict("stack_stop_permission_required", id));
                }
                let (_, axum::Json(stopping)) =
                    crate::service_runtime::stop_from_stack(state.clone(), id, resource, Some(id))
                        .await?;
                super::execute::wait_service(
                    state,
                    resource,
                    stopping.id,
                    false,
                    std::time::Duration::from_secs(3600),
                    id,
                )
                .await?;
            }
            let result =
                micro_services::delete_from_stack(state.clone(), id, resource, Some(id)).await;
            if let Err(error) = result
                && error.code() != "not_found"
            {
                return Err(error);
            }
        }
        ResourceKind::Vm => {
            let vm = state
                .vms
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&resource)
                .cloned();
            if let Some(vm) = vm {
                if vm.purpose != VmPurpose::Instance {
                    return Err(conflict("protected_stack_vm", id));
                }
                if vm.state == firecrab_api_types::VmState::Running {
                    if !allow_stop {
                        return Err(conflict("stack_stop_permission_required", id));
                    }
                    let _ = vms::stop_vm(
                        State(state.clone()),
                        Extension(RequestId(id)),
                        Path(resource.to_string()),
                    )
                    .await?;
                }
                vms::delete_vm_with_disk_policy(
                    state.clone(),
                    RequestId(id),
                    resource,
                    !status.lifecycle.delete_vm_disks,
                    Some(id),
                )
                .await?;
            } else {
                match state.store.release_lease(resource) {
                    Ok(()) | Err(crate::ipam::IpamError::NotLeased { .. }) => {}
                    Err(_) => return Err(AppError::internal(id)),
                }
            }
        }
        ResourceKind::Network => {
            let result =
                micro_networks::delete_from_stack(state.clone(), id, resource, Some(id)).await;
            if let Err(error) = result
                && error.code() != "not_found"
            {
                return Err(error);
            }
        }
    }
    let store = state.store.clone();
    database(id, move || store.finish_stack_delete(id, index)).await
}

fn conflict(code: &'static str, id: Uuid) -> AppError {
    AppError::conflict(
        code,
        "inspect exact ownership, workload interruption and external dependencies before destructive stack changes",
        id,
    )
}

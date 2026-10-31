//! Metadata-only stack ownership and read-only planning against the selected host.

use std::collections::{BTreeMap, BTreeSet};

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use firecrab_stack::{
    Binding, DeleteStackRequest, Inventory, Plan, RegisterStackRequest, ServiceSnapshot, Stack,
    StackRecord, VmSnapshot, logical_name,
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::model::VmPurpose;
use crate::persistence::{PersistenceError, Store};
use crate::server::RequestId;
use crate::state::AppState;

mod guard;
pub(crate) use guard::protect_resources;

/// Idempotently register a stack name without storing or applying its manifest.
pub async fn register(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    ValidatedJson(request): ValidatedJson<RegisterStackRequest>,
) -> Result<Json<StackRecord>, AppError> {
    validate_name(&request.name, request_id.0)?;
    database(request_id.0, move || {
        state.store.register_stack(&request.name)
    })
    .await
    .map(Json)
}

/// List identities and ownership bindings without returning resource configuration.
pub async fn list(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
) -> Result<Json<Vec<StackRecord>>, AppError> {
    database(request_id.0, move || state.store.list_stacks())
        .await
        .map(Json)
}

/// Read one registered stack's current ownership revision.
pub async fn get(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(name): Path<String>,
) -> Result<Json<StackRecord>, AppError> {
    validate_name(&name, request_id.0)?;
    database(request_id.0, move || {
        state
            .store
            .stack(&name)?
            .ok_or(PersistenceError::MissingStack)
    })
    .await
    .map(Json)
}

/// Delete only empty metadata after matching the exact registration UUID.
pub async fn delete(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(name): Path<String>,
    Query(request): Query<DeleteStackRequest>,
) -> Result<StatusCode, AppError> {
    validate_name(&name, request_id.0)?;
    database(request_id.0, move || {
        state.store.delete_empty_stack(&name, request.id)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Claim an existing resource UUID explicitly. This does not edit the resource.
pub async fn bind(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(name): Path<String>,
    ValidatedJson(binding): ValidatedJson<Binding>,
) -> Result<Json<StackRecord>, AppError> {
    let _admission = state.stack_apply_admission.lock().await;
    let _claim = state.service_mutations.lock().await;
    change_binding(state.store.clone(), request_id.0, name, binding, false)
        .await
        .map(Json)
}

/// Release an exact binding without stopping or deleting its resource or disks.
pub async fn unbind(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(name): Path<String>,
    ValidatedJson(binding): ValidatedJson<Binding>,
) -> Result<Json<StackRecord>, AppError> {
    let _admission = state.stack_apply_admission.lock().await;
    let _claim = state.service_mutations.lock().await;
    change_binding(state.store.clone(), request_id.0, name, binding, true)
        .await
        .map(Json)
}

async fn change_binding(
    store: Store,
    request_id: Uuid,
    name: String,
    binding: Binding,
    remove: bool,
) -> Result<StackRecord, AppError> {
    validate_name(&name, request_id)?;
    if !logical_name(&binding.key) || binding.resource_id.is_nil() {
        return Err(AppError::validation(
            BTreeMap::from([(
                "binding".to_owned(),
                "requires a logical DNS key and a non-nil resource UUID".to_owned(),
            )]),
            request_id,
        ));
    }
    database(request_id, move || {
        if remove {
            store.unbind_stack_resource(&name, &binding)
        } else {
            store.bind_stack_resource(&name, &binding)
        }
    })
    .await
}

/// Compare a validated declaration to explicit UUID ownership; never mutate host resources.
pub async fn plan(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(name): Path<String>,
    ValidatedJson(stack): ValidatedJson<Stack>,
) -> Result<Json<Plan>, AppError> {
    validate_name(&name, request_id.0)?;
    if name != stack.name {
        return Err(AppError::validation(
            BTreeMap::from([(
                "name".to_owned(),
                "must match the stack name in the URL".to_owned(),
            )]),
            request_id.0,
        ));
    }
    firecrab_stack::validate(&stack).map_err(|errors| validation_errors(errors, request_id.0))?;
    let (record, inventory) = database(request_id.0, move || {
        let record = state
            .store
            .stack(&name)?
            .ok_or(PersistenceError::MissingStack)?;
        let inventory = inventory(&state.store, &state.templates)?;
        Ok((record, inventory))
    })
    .await?;
    firecrab_stack::plan(&stack, &record, &inventory)
        .map(Json)
        .map_err(|errors| validation_errors(errors, request_id.0))
}

pub(crate) fn inventory(
    store: &Store,
    templates: &crate::templates::TemplateRegistry,
) -> Result<Inventory, PersistenceError> {
    let mut services = Vec::new();
    let mut service_vms = BTreeSet::new();
    for service in store.list_micro_services()? {
        let deployment = store.service_deployment(service.id)?;
        if let Some(deployment) = &deployment {
            service_vms.extend(deployment.owned_vm_ids());
        }
        services.push(ServiceSnapshot {
            service,
            deployment,
        });
    }
    let vms = store
        .load_all()?
        .into_values()
        .filter(|vm| vm.purpose == VmPurpose::Instance && !service_vms.contains(&vm.id))
        .map(|vm| {
            Ok(VmSnapshot {
                id: vm.id,
                name: vm.name,
                template: vm.template,
                template_version: vm.template_version,
                template_kernel_sha256: vm.template_kernel_sha256,
                template_rootfs_sha256: vm.template_rootfs_sha256,
                template_boot_args_sha256: vm.template_boot_args_sha256,
                template_pin: store.stack_vm_pin(vm.id, false)?,
                disk_generation: vm.disk_generation,
                cpu: vm.cpu,
                ram: vm.ram,
                disk_gb: vm.disk_gb,
                egress_policy: vm.egress_policy,
                micro_network_id: vm.micro_network_id,
                storage_root: vm.storage_root,
                env: vm.env,
            })
        })
        .collect::<Result<_, PersistenceError>>()?;
    Ok(Inventory {
        templates: templates.stack_snapshot(),
        networks: store.list_micro_networks()?,
        vms,
        services,
    })
}

pub(super) fn validate_name(name: &str, request_id: Uuid) -> Result<(), AppError> {
    if logical_name(name) {
        return Ok(());
    }
    Err(AppError::validation(
        BTreeMap::from([(
            "name".to_owned(),
            "must be a lowercase DNS label of 1-63 bytes".to_owned(),
        )]),
        request_id,
    ))
}

pub(super) fn validation_errors(
    errors: Vec<firecrab_stack::Diagnostic>,
    request_id: Uuid,
) -> AppError {
    AppError::validation(
        errors
            .into_iter()
            .map(|e| (e.path, e.message.to_owned()))
            .collect(),
        request_id,
    )
}

/// Keep SQLite off the async executor and never log submitted configuration values.
pub(crate) async fn database<T: Send + 'static>(
    request_id: Uuid,
    operation: impl FnOnce() -> Result<T, PersistenceError> + Send + 'static,
) -> Result<T, AppError> {
    tokio::task::spawn_blocking(operation).await.map_err(|_| AppError::internal(request_id))?.map_err(|error| match error {
        PersistenceError::IacResourceBusy => AppError::conflict("iac_resource_busy", "wait for the current resource operation before transferring ownership",request_id),
        PersistenceError::IacParentManaged => AppError::conflict("iac_parent_managed", "resource is generated by a parent controller; manage its parent instead",request_id),
        PersistenceError::IacVaultUnavailable => AppError::unavailable("credential store is unavailable; ownership was not changed", request_id),
        PersistenceError::IacEndpointConflict => AppError::conflict("iac_endpoint_mismatch", "ownership request belongs to a different API endpoint", request_id),
        PersistenceError::InvalidIacOwner => AppError::new(StatusCode::BAD_REQUEST, "invalid_iac_owner", "provide valid resource, owner and operation identities", request_id),
        PersistenceError::UnsupportedIacResource => AppError::new(StatusCode::BAD_REQUEST, "unsupported_iac_resource", "this resource adapter does not yet support safe ownership transfer", request_id),
        PersistenceError::IacOwnershipConflict => AppError::conflict("iac_ownership_conflict", "resource ownership conflicts; inspect the exact current owner", request_id),
        PersistenceError::StaleIacOwner => AppError::conflict("stale_iac_owner", "read the current owner and generation before changing ownership", request_id),
        PersistenceError::IacOperationIdentity => AppError::conflict("iac_operation_identity_conflict", "operation UUID requires the same normalized ownership request", request_id),
        PersistenceError::MissingStack | PersistenceError::MissingStackSource => AppError::not_found(request_id),
        PersistenceError::StaleStackSource => AppError::conflict("stale_stack_source", "read the current source generation before updating or checking it", request_id),
        PersistenceError::StackSourceBusy => AppError::conflict("stack_source_busy", "wait for the active Git check before changing or deleting its configuration", request_id),
        PersistenceError::StackSyncRecoveryRequired => AppError::conflict("stack_sync_recovery_required", "inspect the failed apply and acknowledge its operation ID before requesting a fresh sync", request_id),
        PersistenceError::InvalidStackSource => AppError::new(StatusCode::BAD_REQUEST, "invalid_stack_source", "provide a valid Git source configuration", request_id),
        PersistenceError::MissingStackApply => AppError::not_found(request_id),
        PersistenceError::StackApplyBusy => AppError::conflict("stack_apply_busy", "wait for the active stack apply to finish", request_id),
        PersistenceError::StaleStackPlan => AppError::conflict("stale_stack_plan", "host configuration or ownership changed; review a fresh plan", request_id),
        PersistenceError::StackApplyIdentity => AppError::conflict("stack_apply_identity_conflict", "operation ID belongs to a different registration, declaration or stop permission", request_id),
        PersistenceError::StackSourceManaged => AppError::conflict("stack_source_managed", "use source sync for a Git-managed stack; explicitly remove the source to return to manual apply", request_id),
        PersistenceError::StackNotEmpty => AppError::new(StatusCode::CONFLICT, "stack_not_empty", "unbind every exact resource UUID before deleting stack registration", request_id),
        PersistenceError::StackConflict => AppError::new(StatusCode::CONFLICT, "stack_ownership_conflict", "binding conflicts with existing ownership; unbind the exact UUID explicitly before changing it", request_id),
        PersistenceError::InvalidStackResource => AppError::new(StatusCode::BAD_REQUEST, "invalid_stack_resource", "resource is missing, is a protected VM, or a service operation is in progress", request_id),
        PersistenceError::StackBindingLimit => AppError::new(StatusCode::BAD_REQUEST, "stack_binding_limit", "a stack can own at most 256 resources", request_id),
        _ => { tracing::error!(%request_id, "stack database operation failed"); AppError::internal(request_id) }
    })
}

#[cfg(test)]
mod tests;

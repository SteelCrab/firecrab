//! A configured controller owns each receipt and its immutable local VM UUID.
use super::{image, inventory, stored, validate_spec};
use crate::{
    error::AppError, extract::ValidatedJson, model::VmRecord, persistence::ClusterError,
    server::RequestId, state::AppState, tls::AuthenticatedHost,
};
use axum::{
    Extension, Json,
    extract::{ConnectInfo, Path, Request, State},
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use firecrab_api_types::*;
use uuid::Uuid;

pub(crate) type Peer = Option<Extension<ConnectInfo<AuthenticatedHost>>>;

pub(crate) fn peer_id(peer: Peer, request: RequestId) -> Result<Uuid, AppError> {
    peer.map(|Extension(ConnectInfo(peer))| peer.host_id)
        .ok_or_else(|| {
            AppError::new(
                StatusCode::FORBIDDEN,
                "host_authentication_required",
                "this operation requires an authenticated TLS host",
                request.0,
            )
        })
}

pub(super) fn controller(
    state: &AppState,
    peer: Peer,
    request: RequestId,
) -> Result<Uuid, AppError> {
    let id = peer_id(peer, request)?;
    if state
        .cluster
        .worker
        .as_ref()
        .is_none_or(|w| w.controller_id != id)
    {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "wrong_controller",
            "host is not enrolled with this controller",
            request.0,
        ));
    }
    Ok(id)
}

/// Direct local mutations cannot bypass the controller's durable lifecycle journal.
pub(crate) async fn protect_vm(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) {
        return next.run(request).await;
    }
    let (service, vm) = {
        let path_id = |prefix| {
            request
                .uri()
                .path()
                .strip_prefix(prefix)
                .and_then(|s| s.split('/').next())
                .and_then(|s| Uuid::parse_str(s).ok())
        };
        (path_id("/api/micro-services/"), path_id("/api/vms/"))
    };
    if service.is_none() && vm.is_none() {
        return next.run(request).await;
    }
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(|r| r.0)
        .unwrap_or_else(Uuid::new_v4);
    let store = state.store.clone();
    let owner = stored(move || {
        if let Some(vm) = vm
            && store.cluster_worker(vm)?.is_some()
        {
            return Ok(Some((
                "controller_owned_vm",
                "use the owning controller placement API for this VM",
            )));
        }
        let service = match (service, vm) {
            (Some(id), _) => Some(id),
            (_, Some(vm)) => store.service_deployment_for_vm(vm)?.map(|d| d.service_id),
            _ => None,
        };
        if let Some(service) = service
            && store.cluster_service_worker(service)?.is_some()
        {
            return Ok(Some((
                "controller_owned_service",
                "use the owning controller service API",
            )));
        }
        Ok(None)
    })
    .await;
    match owner {
        Ok(Some((code, message))) => {
            return AppError::conflict(code, message, request_id).into_response();
        }
        Err(_) => return AppError::internal(request_id).into_response(),
        _ => {}
    }
    next.run(request).await
}

/// Also used by ordinary local VM creation on enrolled workers.
pub(crate) async fn check_capacity(
    state: &AppState,
    id: Uuid,
    cpu: u8,
    ram: u32,
    disk: u16,
    storage: &str,
) -> Result<(), ClusterError> {
    let report = inventory(state).await?;
    let store = state.store.clone();
    let receipts = stored(move || store.cluster_workers()).await?;
    let mut used_cpu = u64::from(report.unmanaged.cpu);
    let mut used_ram = report.unmanaged.ram_mib;
    let store = state.store.clone();
    let service_vms = stored(move || store.cluster_service_vm_ids()).await?;
    {
        let vms = state.vms.lock().unwrap_or_else(|e| e.into_inner());
        for vm in vms
            .values()
            .filter(|vm| service_vms.contains(&vm.id) && vm.id != id)
        {
            used_cpu += u64::from(vm.cpu);
            used_ram += u64::from(vm.ram);
        }
    }
    let root = report
        .storages
        .iter()
        .find(|s| s.id == storage)
        .ok_or_else(|| ClusterError::Invalid("unknown host-local storage".into()))?;
    let mut used_disk = root.unmanaged_gib;
    if !receipts.iter().any(|r| r.id == id) {
        let current = state
            .vms
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .cloned();
        if let Some(vm) = current {
            if !service_vms.contains(&id) {
                used_cpu = used_cpu.saturating_sub(u64::from(vm.cpu));
                used_ram = used_ram.saturating_sub(u64::from(vm.ram));
            }
            if vm.storage_root == storage {
                used_disk = used_disk.saturating_sub(u64::from(vm.disk_gb));
            }
        }
    }
    for r in receipts
        .iter()
        .filter(|r| r.id != id && r.phase.reserves_capacity())
    {
        used_cpu += u64::from(r.intent.spec.cpu);
        used_ram += u64::from(r.intent.spec.ram);
        if r.intent.target.storage.resource_id == storage {
            used_disk += u64::from(r.intent.spec.disk_gb);
        }
    }
    if used_cpu + u64::from(cpu) > u64::from(report.capacity.cpu)
        || used_ram + u64::from(ram) > report.capacity.ram_mib
        || used_disk + u64::from(disk) > root.capacity_gib
    {
        return Err(ClusterError::Capacity);
    }
    Ok(())
}

async fn current(state: &AppState, id: Uuid) -> Result<Option<VmRecord>, ClusterError> {
    if let Some(vm) = state
        .vms
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&id)
        .cloned()
    {
        return Ok(Some(vm));
    }
    // A crash after the VM row commit can precede its cache insertion or final receipt.
    let store = state.store.clone();
    let vm = stored(move || Ok(store.load_all()?.remove(&id))).await?;
    if let Some(vm) = &vm {
        state
            .vms
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, vm.clone());
    }
    Ok(vm)
}

async fn observe(state: &AppState, r: &mut WorkerReceipt) -> Result<(), ClusterError> {
    r.observed = match current(state, r.id).await? {
        Some(vm) => {
            let lease = crate::handlers::vms::lease_for(state, r.id).await;
            Some(crate::handlers::vms::vm_response(
                state,
                &vm,
                lease.as_ref(),
            ))
        }
        None => None,
    };
    Ok(())
}

pub(crate) async fn get(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    peer: Peer,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkerReceipt>, AppError> {
    let controller = controller(&state, peer, request)?;
    let store = state.store.clone();
    let mut r = stored(move || store.cluster_worker(id))
        .await
        .map_err(|e| super::handlers::error(e, request))?
        .ok_or_else(|| AppError::not_found(request.0))?;
    if r.controller_id != controller {
        return Err(AppError::not_found(request.0));
    }
    observe(&state, &mut r)
        .await
        .map_err(|e| super::handlers::error(e, request))?;
    Ok(Json(r))
}

pub(crate) async fn put(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    peer: Peer,
    Path(id): Path<Uuid>,
    ValidatedJson(mut intent): ValidatedJson<WorkerIntent>,
) -> Result<Json<WorkerReceipt>, AppError> {
    let owner = controller(&state, peer, request)?;
    validate_spec(&mut intent.spec).map_err(|e| super::handlers::error(e, request))?;
    let local = state
        .cluster
        .identity
        .ok_or_else(|| AppError::internal(request.0))?;
    if id.is_nil()
        || intent.generation > i64::MAX as u64
        || intent.target.network.host_id != local
        || intent.target.storage.host_id != local
        || !intent.spec.targets.contains(&intent.target)
    {
        return Err(AppError::conflict(
            "foreign_resource",
            "worker intent must reference this host's own resources",
            request.0,
        ));
    }
    // Keep the journal, local mutation and acknowledgement alive after HTTP cancellation.
    let permit = state
        .cluster
        .worker_permits
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(16)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::too_many_requests(request.0))?;
    tokio::spawn(async move {
        let _permit = permit;
        execute(state, id, owner, intent, request).await
    })
    .await
    .map_err(|_| AppError::internal(request.0))?
    .map(Json)
}

fn matches_spec(vm: &VmRecord, r: &WorkerReceipt) -> bool {
    let spec = &r.intent.spec;
    let image = &spec.image;
    vm.id == r.id
        && vm.name == spec.name
        && vm.template == image.alias
        && vm.template_version == image.version
        && vm.template_kernel_sha256 == image.kernel_sha256
        && vm.template_rootfs_sha256 == image.rootfs_sha256
        && vm.template_boot_args_sha256 == image.boot_args_sha256
        && vm.cpu == spec.cpu
        && vm.ram == spec.ram
        && vm.disk_gb == spec.disk_gb
        && vm.env == spec.env
        && vm.egress_policy == spec.egress_policy
        && vm.micro_network_id.to_string() == r.intent.target.network.resource_id
        && vm.storage_root == r.intent.target.storage.resource_id
}

async fn execute(
    state: AppState,
    id: Uuid,
    owner: Uuid,
    intent: WorkerIntent,
    request: RequestId,
) -> Result<WorkerReceipt, AppError> {
    let _lock = state.cluster.lock_worker(id).await;
    let admission = state.cluster.admission.lock().await;
    let store = state.store.clone();
    let mut r = stored(move || store.cluster_worker_admit(id, owner, intent))
        .await
        .map_err(|e| super::handlers::error(e, request))?;
    drop(admission);
    if matches!(
        r.phase,
        PlacementPhase::Confirmed | PlacementPhase::Deleted | PlacementPhase::Rejected
    ) {
        observe(&state, &mut r)
            .await
            .map_err(|e| super::handlers::error(e, request))?;
        return Ok(r);
    }
    let result = apply(&state, &r, request).await;
    observe(&state, &mut r)
        .await
        .map_err(|e| super::handlers::error(e, request))?;
    match result {
        Ok(()) => {
            r.phase = if r.intent.action == PlacementAction::Delete {
                PlacementPhase::Deleted
            } else {
                PlacementPhase::Confirmed
            };
            r.error = None;
        }
        Err(e) => {
            r.phase = if r.intent.action == PlacementAction::Create
                && r.observed.is_none()
                && e.code() != "internal_error"
            {
                PlacementPhase::Rejected
            } else if matches!(
                r.intent.action,
                PlacementAction::Start | PlacementAction::Stop
            ) && r.observed.is_some()
            {
                PlacementPhase::Confirmed
            } else {
                PlacementPhase::Uncertain
            };
            r.error = Some(e.redacted().description());
        }
    }
    let store = state.store.clone();
    let receipt = r.clone();
    stored(move || store.cluster_worker_finish(&receipt))
        .await
        .map_err(|e| super::handlers::error(e, request))?;
    Ok(r)
}

async fn apply(state: &AppState, r: &WorkerReceipt, request: RequestId) -> Result<(), AppError> {
    let vm = current(state, r.id)
        .await
        .map_err(|e| super::handlers::error(e, request))?;
    let spec = &r.intent.spec;
    let id = r.id.to_string();
    if matches!(
        r.intent.action,
        PlacementAction::Create | PlacementAction::Start
    ) && state
        .templates
        .resolve_version(&spec.image.alias, &spec.image.version)
        .is_none_or(|version| image(&version) != spec.image)
    {
        return Err(AppError::conflict(
            "image_changed",
            "pinned boot image components are unavailable or changed",
            request.0,
        ));
    }
    let state_arg = || State(state.clone());
    let request_arg = || Extension(request);
    let path = || Path(id.clone());
    match r.intent.action {
        PlacementAction::Create => {
            if let Some(vm) = vm {
                if !matches_spec(&vm, r) {
                    return Err(AppError::conflict(
                        "placement_drift",
                        "owned VM differs from its placement intent",
                        request.0,
                    ));
                }
                crate::handlers::vms::resume_cluster_create(state, r.id, request).await?;
                return Ok(());
            }
            let Some(template) = state.templates.resolve_alias(&spec.image.alias) else {
                return Err(AppError::conflict(
                    "image_missing",
                    "pinned image is not installed",
                    request.0,
                ));
            };
            if image(&template) != spec.image {
                return Err(AppError::conflict(
                    "image_changed",
                    "image no longer matches the reserved fingerprint",
                    request.0,
                ));
            }
            let network = Uuid::parse_str(&r.intent.target.network.resource_id)
                .map_err(|_| AppError::internal(request.0))?;
            let req = CreateVmRequest {
                name: spec.name.clone(),
                template: spec.image.alias.clone(),
                cpu: spec.cpu,
                ram: spec.ram,
                disk_gb: spec.disk_gb,
                egress_policy: spec.egress_policy,
                env: spec.env.clone(),
                micro_network_id: network,
                storage_root: Some(r.intent.target.storage.resource_id.clone()),
                security_group_id: None,
                block_storages: Vec::new(),
                shell_ids: Vec::new(),
                port_forwards: Vec::new(),
            };
            let _ =
                crate::handlers::vms::create_vm_with_id(state.clone(), request, req, r.id).await?;
        }
        PlacementAction::Start => {
            let vm = vm.ok_or_else(|| AppError::not_found(request.0))?;
            if !matches_spec(&vm, r) {
                return Err(AppError::conflict(
                    "placement_drift",
                    "owned VM differs from its placement intent",
                    request.0,
                ));
            }
            if !matches!(vm.state, VmState::Running | VmState::Starting) {
                let _ = crate::handlers::vms::start_vm_request(state_arg(), request_arg(), path())
                    .await?;
            }
        }
        PlacementAction::Stop => {
            let vm = vm.ok_or_else(|| AppError::not_found(request.0))?;
            if !matches!(vm.state, VmState::Created | VmState::Stopped) {
                let _ = crate::handlers::vms::stop_vm(state_arg(), request_arg(), path()).await?;
            }
        }
        PlacementAction::Delete => {
            if let Some(vm) = vm {
                if !matches!(
                    vm.state,
                    VmState::Created | VmState::Stopped | VmState::Error
                ) {
                    let _ =
                        crate::handlers::vms::stop_vm(state_arg(), request_arg(), path()).await?;
                }
                crate::handlers::vms::delete_vm(state_arg(), request_arg(), path()).await?;
            }
            crate::handlers::vms::resume_cluster_delete(
                state,
                r.id,
                &r.intent.target.storage.resource_id,
                request,
            )
            .await?;
        }
    }
    Ok(())
}

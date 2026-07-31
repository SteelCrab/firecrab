use std::collections::{BTreeMap, HashMap};
use std::io::Read;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use firecrab_api_types::{VmLogResponse, VmResponse};
use firecrab_helper_protocol::network::{DhcpLeaseEntry, MicroNetworkSpec};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::firecracker::{self, FirecrackerProcess, VmNetwork, VmProcess};
use crate::ipam::{IpamError, SubnetSpec};
use crate::model::{
    CreateVmRequest, Lease, StartupStep, StartupStepOutcome, StartupStepRun,
    UpdateVmResourcesRequest, VmRecord, VmState,
};
use crate::network_policy::EgressPolicy;
use crate::persistence::PersistenceError;
use crate::rootfs;
use crate::server::RequestId;
use crate::state::AppState;

pub async fn list_vms(State(state): State<AppState>) -> Json<Vec<VmResponse>> {
    let vms = {
        let guard = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.clone()
    };
    // Best-effort, same reasoning as lease_for: a failure here just means
    // every VM's ipv4/mac comes back None for this one poll, not a failed
    // list.
    let store = state.store.clone();
    let leases: HashMap<Uuid, Lease> = tokio::task::spawn_blocking(move || store.active_leases())
        .await
        .ok()
        .and_then(Result::ok)
        .map(|leases| {
            leases
                .into_iter()
                .map(|lease| (lease.vm_id, lease))
                .collect()
        })
        .unwrap_or_default();
    Json(sorted_responses(&vms, &leases))
}

pub async fn get_vm(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<VmResponse>, AppError> {
    let id = parse_id(&id, request_id.0)?;
    let record = {
        let vms = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        vms.get(&id).cloned()
    };
    let Some(record) = record else {
        return Err(AppError::not_found(request_id.0));
    };
    let lease = lease_for(&state, id).await;
    Ok(Json(vm_response(&record, lease.as_ref())))
}

/// Bytes of the on-disk console log returned to the dashboard's VM detail
/// view; generous enough for a full boot without risking an unbounded
/// response for a long-lived VM's ever-growing log file.
const MAX_LOG_BYTES: u64 = 256 * 1024;

/// The VM's captured serial console output (see
/// `firecracker::console_log_path`) — empty, not an error, before the VM has
/// ever produced any output.
pub async fn get_vm_log(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<VmLogResponse>, AppError> {
    let id = parse_id(&id, request_id.0)?;
    let (storage_root, last_runtime_id) = {
        let vms = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match vms.get(&id) {
            Some(vm) => (vm.storage_root.clone(), vm.last_runtime_id),
            None => return Err(AppError::not_found(request_id.0)),
        }
    };

    let vms_dir = state.vms_dir_for(&storage_root);
    tokio::task::spawn_blocking(move || read_console_log(&vms_dir, id, last_runtime_id))
        .await
        .map(Json)
        .map_err(|_| AppError::internal(request_id.0))
}

fn read_console_log(
    vms_dir: &std::path::Path,
    id: Uuid,
    last_runtime_id: Option<Uuid>,
) -> VmLogResponse {
    let Some(runtime_id) = last_runtime_id else {
        return VmLogResponse {
            console_log: String::new(),
            truncated: false,
        };
    };
    let runtime = crate::artifacts::VmArtifactPaths::for_vm(vms_dir, id).runtime(runtime_id);
    let path = firecracker::console_log_path(&runtime);
    let Ok(file) = std::fs::File::open(&path) else {
        return VmLogResponse {
            console_log: String::new(),
            truncated: false,
        };
    };
    let truncated = file
        .metadata()
        .map(|metadata| metadata.len() > MAX_LOG_BYTES)
        .unwrap_or(false);
    let mut buffer = Vec::new();
    let _ = file.take(MAX_LOG_BYTES).read_to_end(&mut buffer);
    VmLogResponse {
        console_log: String::from_utf8_lossy(&buffer).into_owned(),
        truncated,
    }
}

pub async fn create_vm(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    ValidatedJson(req): ValidatedJson<CreateVmRequest>,
) -> Result<(StatusCode, Json<VmResponse>), AppError> {
    let fields = validate_create(&req, &state);
    if !fields.is_empty() {
        return Err(AppError::validation(fields, request_id.0));
    }
    // Not verified against disk here — that's a real (2GB rootfs) I/O cost,
    // and run_start already re-verifies right before the template is
    // actually used, which is the point that matters for catching a
    // tampered/corrupted artifact.
    let template = state
        .templates
        .resolve_alias(&req.template)
        .ok_or_else(|| AppError::internal(request_id.0))?;
    let storage_root = req
        .storage_root
        .clone()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| state.storage.default_id().to_owned());
    let vm = VmRecord {
        id: Uuid::new_v4(),
        name: req.name,
        template: template.name.clone(),
        template_version: template.version.clone(),
        template_kernel_sha256: template.kernel.sha256().to_owned(),
        template_rootfs_sha256: template.rootfs.sha256().to_owned(),
        template_boot_args_sha256: template.boot_args_sha256(),
        ram: req.ram,
        cpu: req.cpu,
        disk_gb: req.disk_gb,
        egress_policy: req.egress_policy,
        micro_network_id: req.micro_network_id,
        storage_root,
        disk_generation: None,
        last_runtime_id: None,
        state: VmState::Created,
        startup_step: None,
        startup_timeline: Vec::new(),
        package_update: None,
    };

    let store = state.store.clone();
    let record = vm.clone();
    tokio::task::spawn_blocking(move || store.insert(&record))
        .await
        .map_err(|_| AppError::internal(request_id.0))?
        .map_err(|error| {
            tracing::error!(request_id = %request_id.0, %error, "failed to persist VM state");
            AppError::internal(request_id.0)
        })?;

    // Allocated up front (not on first start) so it persists across every
    // stop/start of this VM and is only ever freed by a successful delete —
    // see Store::active_lease's doc comment. The subnet it comes from is the
    // VM's MicroNetwork, so a VM can never hold an address its own bridge
    // doesn't route.
    let subnet = match resolve_subnet(&state, vm.micro_network_id, request_id.0).await {
        Ok(subnet) => subnet,
        Err(error) => {
            let store = state.store.clone();
            let vm_id = vm.id;
            let _ = tokio::task::spawn_blocking(move || store.delete(vm_id)).await;
            return Err(error);
        }
    };
    let store = state.store.clone();
    let vm_id = vm.id;
    let lease = match tokio::task::spawn_blocking(move || store.allocate_lease(vm_id, subnet))
        .await
        .map_err(|_| AppError::internal(request_id.0))?
    {
        Ok(lease) => lease,
        Err(error) => {
            tracing::error!(request_id = %request_id.0, vm_id = %vm_id, %error, "failed to allocate vm lease");
            let store = state.store.clone();
            let _ = tokio::task::spawn_blocking(move || store.delete(vm_id)).await;
            return Err(AppError::internal(request_id.0));
        }
    };
    // Best-effort: a failure here just means the guest won't get its lease
    // via DHCP until the next sync — setup_vm_network re-pushes the full
    // snapshot on every start regardless, and the console-sentinel
    // readiness check at that point is what actually surfaces a genuinely
    // missing reservation, not this one.
    if let Err(error) = sync_dhcp_leases(&state).await {
        tracing::warn!(request_id = %request_id.0, vm_id = %vm.id, error, "dhcp sync failed after create");
    }

    tracing::info!(
        request_id = %request_id.0,
        vm_id = %vm.id,
        name = vm.name,
        template = vm.template,
        cpu = vm.cpu,
        ram = vm.ram,
        "vm created"
    );
    let response = vm_response(&vm, Some(&lease));
    state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(vm.id, vm);

    Ok((StatusCode::CREATED, Json(response)))
}

/// Replaces cpu/ram/disk for a VM that has no live process. Applies on the
/// *next* `start`, not to a running Firecracker instance.
pub async fn update_vm(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(req): ValidatedJson<UpdateVmResourcesRequest>,
) -> Result<Json<VmResponse>, AppError> {
    let id = parse_id(&id, request_id.0)?;
    let (updated, previous) = claim_resource_update(&state, id, &req, request_id.0)?;
    if let Err(error) = persist_update(&state, &updated, request_id.0).await {
        restore_resources(&state, id, previous);
        return Err(error);
    }
    tracing::info!(
        request_id = %request_id.0,
        vm_id = %id,
        cpu = updated.cpu,
        ram = updated.ram,
        disk_gb = updated.disk_gb,
        "vm resources updated"
    );
    let lease = lease_for(&state, id).await;
    Ok(Json(vm_response(&updated, lease.as_ref())))
}

type PreviousResources = (u8, u32, u16, EgressPolicy);

fn claim_resource_update(
    state: &AppState,
    id: Uuid,
    req: &UpdateVmResourcesRequest,
    request_id: Uuid,
) -> Result<(VmRecord, PreviousResources), AppError> {
    let mut vms = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let vm = vms
        .get_mut(&id)
        .ok_or_else(|| AppError::not_found(request_id))?;
    if !vm.state.can_edit_resources() {
        return Err(AppError::invalid_state(vm.state, request_id));
    }
    let fields = validate_update(req, vm.disk_gb);
    if !fields.is_empty() {
        return Err(AppError::validation(fields, request_id));
    }
    let previous = (vm.cpu, vm.ram, vm.disk_gb, vm.egress_policy);
    vm.cpu = req.cpu;
    vm.ram = req.ram;
    vm.disk_gb = req.disk_gb;
    vm.egress_policy = req.egress_policy;
    Ok((vm.clone(), previous))
}

fn restore_resources(state: &AppState, id: Uuid, previous: PreviousResources) {
    let mut vms = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(vm) = vms.get_mut(&id) {
        (vm.cpu, vm.ram, vm.disk_gb, vm.egress_policy) = previous;
    }
}

fn validate_update(
    req: &UpdateVmResourcesRequest,
    current_disk_gb: u16,
) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    if !(1..=32).contains(&req.cpu) {
        fields.insert("cpu".to_owned(), "must be between 1 and 32".to_owned());
    }
    if !is_valid_ram_mib(req.ram) {
        fields.insert(
            "ram".to_owned(),
            "must be a power of two between 128 and 32768 MiB".to_owned(),
        );
    }
    if req.disk_gb < current_disk_gb {
        fields.insert(
            "diskGb".to_owned(),
            format!(
                "must be at least the current size ({current_disk_gb} GiB) — shrinking is not supported"
            ),
        );
    } else if req.disk_gb > MAX_DISK_GB {
        fields.insert(
            "diskGb".to_owned(),
            format!("must be at most {MAX_DISK_GB} GiB"),
        );
    }
    fields
}

/// Starts the VM synchronously: claim `starting`, prepare the disk and
/// config, spawn Firecracker, wait for its API, then record `running`.
/// Any failure lands the record on `error` with no process left behind.
pub async fn start_vm(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<VmResponse>, AppError> {
    let id = parse_id(&id, request_id.0)?;
    let (claimed, previous) = claim_transition(&state, id, VmState::Starting, request_id.0)?;
    if let Err(error) = persist_update(&state, &claimed, request_id.0).await {
        set_memory_state(&state, id, previous);
        return Err(error);
    }

    // Detached: a slow disk copy (queued behind `disk_prep_permits` when
    // many VMs start at once) can outlive the per-request timeout in
    // `enforce_limits`. A plain nested `.await` chain would be dropped
    // right along with the timed-out request, orphaning the VM in
    // `starting` forever — nothing would be left to call `spawn_vm` or
    // record the final state once the disk work finished. `tokio::spawn`
    // runs independently of whoever's awaiting it, so the VM still reaches
    // `running`/`error` even if this response times out; the dashboard's
    // existing polling picks up the result either way.
    let state_for_task = state.clone();
    tokio::spawn(async move { finish_start(&state_for_task, id, claimed, request_id).await })
        .await
        .map_err(|_| AppError::internal(request_id.0))?
}

async fn finish_start(
    state: &AppState,
    id: Uuid,
    claimed: VmRecord,
    request_id: RequestId,
) -> Result<Json<VmResponse>, AppError> {
    let started = std::time::Instant::now();
    let process = match run_start(state, &claimed).await {
        Ok(process) => process,
        Err(reason) => {
            tracing::error!(request_id = %request_id.0, vm_id = %id, reason, "vm start failed");
            return Err(fail_start_with(state, id, request_id.0, Some(reason)).await);
        }
    };
    let pid = process.pid();

    firecracker::register_and_watch(state, id, process);

    finish_startup_timeline(state, id, StartupStepOutcome::Succeeded, None);

    match transition_if(state, id, VmState::Starting, VmState::Running) {
        Some(running) => {
            if persist_update(state, &running, request_id.0).await.is_err() {
                return Err(fail_start(state, id, request_id.0).await);
            }
            tracing::info!(
                request_id = %request_id.0,
                vm_id = %id,
                pid,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "vm running"
            );
            let lease = lease_for(state, id).await;
            Ok(Json(vm_response(&running, lease.as_ref())))
        }
        // The guest exited before we could record running; the exit monitor
        // already landed the record on its terminal state.
        None => {
            let record = state
                .vms
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&id)
                .cloned();
            let Some(record) = record else {
                return Err(AppError::not_found(request_id.0));
            };
            let lease = lease_for(state, id).await;
            Ok(Json(vm_response(&record, lease.as_ref())))
        }
    }
}

/// Stops the VM synchronously: claim `stopping`, SIGTERM the process,
/// escalate to SIGKILL after the grace period, then record `stopped`.
pub async fn stop_vm(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<VmResponse>, AppError> {
    let id = parse_id(&id, request_id.0)?;
    let (claimed, previous) = claim_transition(&state, id, VmState::Stopping, request_id.0)?;
    if let Err(error) = persist_update(&state, &claimed, request_id.0).await {
        set_memory_state(&state, id, previous);
        return Err(error);
    }

    let entry = state
        .processes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&id)
        .cloned();
    if let Some(VmProcess {
        pid, mut exited, ..
    }) = entry
    {
        firecracker::sigterm(pid);
        if tokio::time::timeout(state.runtime.stop_grace, exited.changed())
            .await
            .is_err()
        {
            firecracker::sigkill(pid);
            let _ = tokio::time::timeout(state.runtime.stop_grace, exited.changed()).await;
        }
    }

    // Only a running VM can reach Stopping (see VmState::can_transition), so
    // setup_vm_network always ran for it — tear its TAP/policy back down.
    // The underlying lease is untouched; it's freed only by a successful
    // delete, so a later start reuses the same IP/MAC.
    teardown_vm_network(&state, id).await;

    // The exit monitor normally lands the record on stopped; cover the
    // process-less and raced cases by finishing the transition ourselves.
    let stopped = {
        let mut vms = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let vm = vms
            .get_mut(&id)
            .ok_or_else(|| AppError::not_found(request_id.0))?;
        if vm.state == VmState::Stopping {
            vm.state = VmState::Stopped;
        }
        vm.clone()
    };
    persist_update(&state, &stopped, request_id.0).await?;
    tracing::info!(request_id = %request_id.0, vm_id = %id, "vm stopped");
    let lease = lease_for(&state, id).await;
    Ok(Json(vm_response(&stopped, lease.as_ref())))
}

/// Hard-deletes the VM: refuse while a process could be alive, then remove
/// the VM directory and the record.
pub async fn delete_vm(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let id = parse_id(&id, request_id.0)?;
    let removed = {
        let mut vms = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let vm = vms
            .get(&id)
            .ok_or_else(|| AppError::not_found(request_id.0))?;
        if !vm.state.can_delete() {
            return Err(AppError::invalid_state(vm.state, request_id.0));
        }
        vms.remove(&id).expect("record checked under the same lock")
    };

    let store = state.store.clone();
    let artifact_paths = crate::artifacts::VmArtifactPaths::for_vm(
        &state.vms_dir_for(&removed.storage_root),
        id,
    );
    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        // Runtime dirs first conceptually (owned by this VM only), then the
        // whole tree including disks — one remove_dir_all covers both.
        if let Err(error) = crate::artifacts::remove_vm_artifacts(&artifact_paths) {
            return Err(format!(
                "failed to remove {}: {error}",
                artifact_paths.dir.display()
            ));
        }
        match store.delete(id) {
            Ok(()) | Err(PersistenceError::MissingVm { .. }) => {}
            Err(error) => return Err(error.to_string()),
        }
        // Only ever freed here, after everything else about the VM is gone
        // — see Store::active_lease's doc comment. NotLeased shouldn't
        // happen (every VM gets one at create_vm), but isn't worth failing
        // an otherwise-successful delete over.
        match store.release_lease(id) {
            Ok(()) | Err(IpamError::NotLeased { .. }) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|inner| inner);

    if let Err(reason) = result {
        tracing::error!(request_id = %request_id.0, vm_id = %id, reason, "vm delete failed");
        state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id, removed);
        return Err(AppError::internal(request_id.0));
    }
    // Best-effort, same reasoning as create_vm's sync call: this VM's
    // reservation just needs to eventually disappear from dnsmasq, not
    // synchronously with this response.
    if let Err(error) = sync_dhcp_leases(&state).await {
        tracing::warn!(request_id = %request_id.0, vm_id = %id, error, "dhcp sync failed after delete");
    }
    tracing::info!(request_id = %request_id.0, vm_id = %id, "vm deleted");
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) fn parse_id(id: &str, request_id: Uuid) -> Result<Uuid, AppError> {
    Uuid::parse_str(id).map_err(|_| {
        let mut fields = BTreeMap::new();
        fields.insert("id".to_owned(), "must be a UUID".to_owned());
        AppError::validation(fields, request_id)
    })
}

/// Atomically checks the transition table and moves the in-memory record to
/// `to`, so concurrent lifecycle calls on the same VM cannot both proceed.
fn claim_transition(
    state: &AppState,
    id: Uuid,
    to: VmState,
    request_id: Uuid,
) -> Result<(VmRecord, VmState), AppError> {
    let mut vms = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let vm = vms
        .get_mut(&id)
        .ok_or_else(|| AppError::not_found(request_id))?;
    let previous = vm.state;
    if !previous.can_transition(to) {
        return Err(AppError::invalid_state(previous, request_id));
    }
    vm.state = to;
    // Reset here rather than skip-if-Starting: `run_start` sets the first
    // real step moments later, so a stale step from a prior start never
    // shows through even for a single poll.
    vm.startup_step = None;
    if to == VmState::Starting {
        vm.startup_timeline.clear();
    }
    Ok((vm.clone(), previous))
}

fn set_memory_state(state: &AppState, id: Uuid, to: VmState) -> Option<VmRecord> {
    let mut vms = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    vms.get_mut(&id).map(|vm| {
        vm.state = to;
        vm.startup_step = None;
        vm.clone()
    })
}

fn transition_if(state: &AppState, id: Uuid, from: VmState, to: VmState) -> Option<VmRecord> {
    let mut vms = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match vms.get_mut(&id) {
        Some(vm) if vm.state == from => {
            vm.state = to;
            vm.startup_step = None;
            Some(vm.clone())
        }
        _ => None,
    }
}

async fn persist_update(
    state: &AppState,
    record: &VmRecord,
    request_id: Uuid,
) -> Result<(), AppError> {
    let store = state.store.clone();
    let record = record.clone();
    tokio::task::spawn_blocking(move || store.update(&record))
        .await
        .map_err(|_| AppError::internal(request_id))?
        .map_err(|error| {
            tracing::error!(request_id = %request_id, %error, "failed to persist VM state");
            AppError::internal(request_id)
        })
}

async fn run_start(state: &AppState, vm: &VmRecord) -> Result<FirecrackerProcess, String> {
    let template = state
        .templates
        .resolve_version(&vm.template, &vm.template_version)
        .ok_or_else(|| {
            format!(
                "template {}/{} is not registered",
                vm.template, vm.template_version
            )
        })?;

    set_startup_step(state, vm.id, StartupStep::PreparingDisk);

    // Set up before the disk copy so a network failure fails fast, without
    // paying for a multi-GB copy first. Any failure *after* this succeeds
    // must tear the TAP + policy back down — see the loop below.
    let network = setup_vm_network(state, vm.id, vm.egress_policy, vm.micro_network_id).await?;

    let result = finish_run_start(state, vm, template, &network).await;
    if result.is_err() {
        teardown_vm_network(state, vm.id).await;
    }
    result
}

async fn finish_run_start(
    state: &AppState,
    vm: &VmRecord,
    template: std::sync::Arc<crate::templates::TemplateVersion>,
    network: &VmNetwork,
) -> Result<FirecrackerProcess, String> {
    // Bounds how many VMs copy/grow a rootfs disk at once — see
    // `DISK_PREP_CONCURRENCY`'s doc comment. Held across the blocking task
    // below, released once that VM's disk+config are ready.
    let permit = state
        .disk_prep_permits
        .clone()
        .acquire_owned()
        .await
        .expect("disk_prep_permits semaphore is never closed");

    let templates = state.templates.clone();
    let record = vm.clone();
    let vms_dir = state.vms_dir_for(&vm.storage_root);
    let state_for_blocking = state.clone();
    let network = network.clone();
    // One durable disk generation per VM; allocated on first prepare and
    // reused for every later start so guest data survives stop/start.
    let generation = record.disk_generation.unwrap_or_else(Uuid::new_v4);
    let runtime_id = Uuid::new_v4();
    let prep = tokio::task::spawn_blocking(move || -> Result<(Uuid, Uuid), String> {
        let _permit = permit;
        let paths = crate::artifacts::VmArtifactPaths::for_vm(&vms_dir, record.id);
        let mut source = templates
            .open_verified(&template.rootfs)
            .map_err(|error| format!("rootfs verification failed: {error}"))?;
        let target_bytes = u64::from(record.disk_gb) * 1024 * 1024 * 1024;
        let rootfs = rootfs::prepare_rootfs(&paths, generation, &mut source, target_bytes)
            .map_err(|error| format!("rootfs preparation failed: {error}"))?;
        rootfs::specialize_guest(&rootfs, record.id)
            .map_err(|error| format!("guest specialization failed: {error}"))?;

        set_startup_step(
            &state_for_blocking,
            record.id,
            StartupStep::GeneratingConfig,
        );
        let runtime = paths
            .create_runtime(runtime_id)
            .map_err(|error| format!("runtime directory failed: {error}"))?;
        let kernel = templates.artifact_path(&template.kernel);
        let initrd = template
            .initrd
            .as_ref()
            .map(|artifact| templates.artifact_path(artifact));
        firecracker::write_config(
            &runtime,
            &rootfs,
            &record,
            &kernel,
            initrd.as_deref(),
            &template.boot_args,
            Some(&network),
        )
        .map_err(|error| format!("config generation failed: {error}"))?;
        Ok((generation, runtime_id))
    })
    .await
    .map_err(|error| format!("start preparation task failed: {error}"))??;

    let (generation, runtime_id) = prep;
    // Persist generation + last runtime so log/delete resolve host paths
    // without storing absolute paths.
    {
        let mut vms = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(live) = vms.get_mut(&vm.id) {
            live.disk_generation = Some(generation);
            live.last_runtime_id = Some(runtime_id);
        }
    }
    let store = state.store.clone();
    let mut to_persist = vm.clone();
    to_persist.disk_generation = Some(generation);
    to_persist.last_runtime_id = Some(runtime_id);
    let _ = tokio::task::spawn_blocking(move || store.update(&to_persist)).await;

    set_startup_step(state, vm.id, StartupStep::StartingProcess);

    let vms_dir = state.vms_dir_for(&vm.storage_root);
    let paths = crate::artifacts::VmArtifactPaths::for_vm(&vms_dir, vm.id);
    let runtime = paths.runtime(runtime_id);
    let process = firecracker::spawn_vm(
        &state.runtime.firecracker_binary,
        &runtime,
        vm.id,
        state.runtime.ready_timeout,
    )
    .await
    .map_err(|error| error.to_string())?;

    set_startup_step(state, vm.id, StartupStep::ConfiguringNetwork);
    // Not registered with register_and_watch yet, so `process` dropping on
    // an early return here still kills it (spawn_vm's Command sets
    // kill_on_drop) — no separate cleanup needed on this path.
    wait_for_network_ready(process.console(), state.runtime.network_ready_timeout)
        .await
        .map_err(|error| format!("network readiness check failed: {error}"))?;

    Ok(process)
}

/// Bytes of console output re-scanned for the network-readiness sentinel
/// on every new chunk; bounded so a guest that never prints one doesn't
/// grow this without limit over a long timeout.
const NETWORK_SENTINEL_SCAN_CAP: usize = 4096;

/// Waits for the guest's boot-time network-readiness unit to report over
/// its serial console (`docs/30-tasks/task-guest-network-configuration.md` — the
/// substitute this project uses for a guest-agent event, which is out of
/// scope). `FIRECRAB_NETWORK_READY` succeeds; `FIRECRAB_NETWORK_FAILED`,
/// the console closing, or the timeout all fail the start outright — a
/// silent DNS failure at this stage must not be treated as a successful
/// boot.
async fn wait_for_network_ready(
    console: &crate::console::ConsoleBroker,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let (backlog, mut receiver) = console.subscribe();
    let mut buffer = backlog;
    if let Some(outcome) = find_network_sentinel(&buffer) {
        return outcome;
    }

    let wait = async {
        loop {
            match receiver.recv().await {
                Ok(chunk) => {
                    buffer.extend_from_slice(&chunk);
                    if buffer.len() > NETWORK_SENTINEL_SCAN_CAP {
                        let excess = buffer.len() - NETWORK_SENTINEL_SCAN_CAP;
                        buffer.drain(..excess);
                    }
                    if let Some(outcome) = find_network_sentinel(&buffer) {
                        return outcome;
                    }
                }
                // Lagged means the broadcast fan-out dropped some buffered
                // output because this receiver fell behind (e.g. many VMs
                // booting at once starve the consumer task of CPU) — the
                // guest and its console are still perfectly alive, so this
                // must not be treated the same as the channel actually
                // closing. Losing a chunk just means a sentinel inside it
                // could be missed; resubscribing from scratch would lose the
                // backlog needed to detect a sentinel that already scrolled
                // by, so simply keep reading forward.
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(
                        skipped,
                        "console broadcast lagged; still waiting for network readiness"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err("console closed before network became ready".to_owned());
                }
            }
        }
    };

    tokio::time::timeout(timeout, wait)
        .await
        .unwrap_or_else(|_| Err("timed out waiting for network readiness".to_owned()))
}

/// Scans already-received console bytes for the sentinel line, re-run
/// against the whole rolling buffer (not just the newest chunk) since the
/// line can straddle two separate console reads.
fn find_network_sentinel(buffer: &[u8]) -> Option<Result<(), String>> {
    let text = String::from_utf8_lossy(buffer);
    for line in text.lines() {
        if line.contains("FIRECRAB_NETWORK_READY") {
            return Some(Ok(()));
        }
        if let Some((_, reason)) = line.split_once("FIRECRAB_NETWORK_FAILED") {
            return Some(Err(format!("guest reported network failure:{reason}")));
        }
    }
    None
}

/// Ensures the bridge/firewall are up, creates `vm_id`'s TAP, and applies its
/// isolation policy for its (already-allocated, see `create_vm`) lease. Any
/// failure after the TAP is created tears the TAP itself back down before
/// returning, so a partial failure here never leaves an unprotected TAP
/// attached to the bridge.
async fn setup_vm_network(
    state: &AppState,
    vm_id: Uuid,
    egress_policy: EgressPolicy,
    micro_network_id: Uuid,
) -> Result<VmNetwork, String> {
    let store = state.store.clone();
    let lease = tokio::task::spawn_blocking(move || store.active_lease(vm_id))
        .await
        .map_err(|error| format!("lease lookup task failed: {error}"))?
        .map_err(|error| format!("lease lookup failed: {error}"))?
        .ok_or_else(|| format!("vm {vm_id} has no allocated lease"))?;

    // Re-applied on every start, not just at create: bridges, nftables rules
    // and dnsmasq's config do not survive a host reboot (or a net-helper
    // restart), and a VM start is the one moment the network it needs has to
    // genuinely exist. Unlike the same call at daemon startup, a failure here
    // fails the start — this VM would have nowhere to attach.
    crate::handlers::micro_networks::ensure_all_networks(state).await?;

    let tap_name = state
        .network
        .create_tap(vm_id, micro_network_id)
        .await
        .map_err(|error| format!("tap creation failed: {error}"))?;

    if let Err(error) = state
        .network
        .apply_vm_policy(vm_id, lease.ipv4, lease.mac, egress_policy, false)
        .await
    {
        // A failed apply_vm_policy leaves nothing installed (nft applies a
        // ruleset as one atomic transaction), so remove_vm_policy is a
        // guaranteed no-op today — called anyway so cleanup here doesn't
        // silently rely on that nft implementation detail staying true, and
        // so this matches teardown_vm_network's own order everywhere else.
        let _ = state.network.remove_vm_policy(vm_id).await;
        let _ = state.network.delete_tap(vm_id).await;
        return Err(format!("firewall policy application failed: {error}"));
    }

    Ok(VmNetwork {
        tap_name,
        guest_mac: lease.mac,
    })
}

/// Reverses [`setup_vm_network`]: removes the firewall policy then the TAP.
/// Best-effort and always called from an already-failing or already-stopping
/// path, so failures here are logged rather than propagated — the VM's own
/// lifecycle state still needs to move forward either way. `pub(crate)` so
/// the exit monitor (`firecracker::register_and_watch`) can call it too —
/// a guest-initiated poweroff or crash never goes through `stop_vm`, so it
/// has to run from there instead.
pub(crate) async fn teardown_vm_network(state: &AppState, vm_id: Uuid) {
    if let Err(error) = state.network.remove_vm_policy(vm_id).await {
        tracing::warn!(vm_id = %vm_id, %error, "failed to remove vm firewall policy");
    }
    if let Err(error) = state.network.delete_tap(vm_id).await {
        tracing::warn!(vm_id = %vm_id, %error, "failed to delete vm tap device");
    }
}

/// Pushes the current full set of active leases to the DHCP helper, tagged
/// with the current lease generation so a snapshot that arrives out of
/// order relative to a newer one is ignored instead of clobbering it (see
/// `Store::lease_revision`). Called after any change to the active-lease
/// set (`create_vm`/`delete_vm`) and defensively on every VM start (see
/// `setup_vm_network`) in case the helper's dnsmasq process itself needs
/// restarting.
pub(crate) async fn sync_dhcp_leases(state: &AppState) -> Result<(), String> {
    let store = state.store.clone();
    let (revision, leases) = tokio::task::spawn_blocking(move || {
        let revision = store.lease_revision()?;
        let leases = store.active_leases()?;
        Ok::<_, IpamError>((revision, leases))
    })
    .await
    .map_err(|error| format!("dhcp lease snapshot task failed: {error}"))?
    .map_err(|error| format!("dhcp lease snapshot failed: {error}"))?;

    let entries = leases
        .into_iter()
        .map(|lease| DhcpLeaseEntry {
            vm_id: lease.vm_id,
            ipv4: lease.ipv4,
            mac: lease.mac,
        })
        .collect();

    state
        .network
        .sync_dhcp_leases(revision, entries, micro_network_specs(state).await?)
        .await
        .map_err(|error| format!("dhcp sync failed: {error}"))
}

/// Reinstalls the per-VM policy of every VM that is currently running.
///
/// A global firewall (re)apply flushes the owned tables, which takes every
/// per-VM chain and verdict-map entry with it — the helper says as much by
/// clearing its `applied_vms`. Without this, a VM that was running when a
/// network was created, deleted or switched on/off would keep its TAP and
/// its address but silently lose all egress until someone restarted it.
pub(crate) async fn reapply_running_vm_policies(state: &AppState) -> Result<(), String> {
    let store = state.store.clone();
    let running = tokio::task::spawn_blocking(move || {
        let vms = store.load_all()?;
        let mut running = Vec::new();
        for vm in vms.into_values().filter(|vm| vm.state == VmState::Running) {
            // A running VM always has one; skipped rather than failed if not,
            // since there is nothing to install for it either way.
            if let Some(lease) = store.active_lease(vm.id).ok().flatten() {
                running.push((vm.id, vm.egress_policy, lease));
            }
        }
        Ok::<_, crate::persistence::PersistenceError>(running)
    })
    .await
    .map_err(|error| format!("running vm lookup task failed: {error}"))?
    .map_err(|error| format!("running vm lookup failed: {error}"))?;

    for (vm_id, egress_policy, lease) in running {
        state
            .network
            .apply_vm_policy(vm_id, lease.ipv4, lease.mac, egress_policy, false)
            .await
            .map_err(|error| format!("policy reapply failed for vm {vm_id}: {error}"))?;
    }
    Ok(())
}

/// The current set of MicroNetworks in the shape the privileged helper takes
/// them. Sent with every firewall/DHCP call because both render one rule (or
/// dnsmasq interface) per network and need the complete set, not a delta.
pub(crate) async fn micro_network_specs(state: &AppState) -> Result<Vec<MicroNetworkSpec>, String> {
    let store = state.store.clone();
    let networks = tokio::task::spawn_blocking(move || store.list_micro_networks())
        .await
        .map_err(|error| format!("micro network lookup task failed: {error}"))?
        .map_err(|error| format!("micro network lookup failed: {error}"))?;

    networks
        .into_iter()
        .map(|network| {
            SubnetSpec::parse(network.id, &network.subnet_cidr)
                .map(|subnet| subnet.helper_spec(network.internet_enabled))
                .ok_or_else(|| {
                    format!(
                        "micro network {} has an unparseable subnet {:?}",
                        network.id, network.subnet_cidr
                    )
                })
        })
        .collect()
}

/// The subnet a new VM's lease comes from. A MicroNetwork id that no longer
/// exists is a client error — it usually means the network was deleted
/// between the form loading and the VM being created.
async fn resolve_subnet(
    state: &AppState,
    micro_network_id: Uuid,
    request_id: Uuid,
) -> Result<SubnetSpec, AppError> {
    let store = state.store.clone();
    let network = tokio::task::spawn_blocking(move || store.micro_network(micro_network_id))
        .await
        .map_err(|_| AppError::internal(request_id))?
        .map_err(|error| {
            tracing::error!(request_id = %request_id, %error, "failed to look up micro network");
            AppError::internal(request_id)
        })?
        .ok_or_else(|| {
            AppError::validation(
                BTreeMap::from([(
                    "microNetworkId".to_owned(),
                    "no MicroNetwork with this id exists".to_owned(),
                )]),
                request_id,
            )
        })?;

    SubnetSpec::parse(network.id, &network.subnet_cidr).ok_or_else(|| {
        tracing::error!(
            request_id = %request_id,
            micro_network_id = %network.id,
            subnet_cidr = network.subnet_cidr,
            "stored micro network subnet does not parse"
        );
        AppError::internal(request_id)
    })
}

/// Records the current phase of an in-flight start so pollers can show *why*
/// a VM hasn't reached `running` yet. Silently a no-op once the record has
/// moved on (e.g. a concurrent failure already landed it on `error`).
fn set_startup_step(state: &AppState, id: Uuid, step: StartupStep) {
    let now = epoch_millis();
    let mut vms = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(vm) = vms.get_mut(&id) {
        close_open_step(vm, now, StartupStepOutcome::Succeeded, None);
        vm.startup_step = Some(step);
        vm.startup_timeline.push(StartupStepRun {
            step,
            started_at_ms: now,
            ended_at_ms: None,
            outcome: StartupStepOutcome::Running,
            detail: None,
        });
    }
}

/// Wall-clock now, in milliseconds since the Unix epoch. The dashboard needs
/// a real clock (it prints the time each step began), not the monotonic one
/// used elsewhere for measuring durations.
fn epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// Closes whichever step is still open, if any. Idempotent, so the success
/// and failure paths can both call it without checking first.
fn close_open_step(
    vm: &mut VmRecord,
    now: u64,
    outcome: StartupStepOutcome,
    detail: Option<String>,
) {
    if let Some(open) = vm
        .startup_timeline
        .iter_mut()
        .find(|run| run.outcome == StartupStepOutcome::Running)
    {
        open.ended_at_ms = Some(now);
        open.outcome = outcome;
        open.detail = detail;
    }
}

/// Ends the current start attempt's timeline. The entries stay on the record
/// afterwards — a finished timeline is exactly what the dashboard shows, so
/// clearing it here would blank the screen at the moment it becomes useful.
/// A *new* start clears it (see `claim_transition`).
fn finish_startup_timeline(
    state: &AppState,
    id: Uuid,
    outcome: StartupStepOutcome,
    detail: Option<String>,
) {
    let now = epoch_millis();
    let mut vms = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(vm) = vms.get_mut(&id) {
        close_open_step(vm, now, outcome, detail);
    }
}

/// Failure tail of the start flow: record `error`, then make sure no process
/// survives (the monitor sees `error` and leaves the record alone).
async fn fail_start(state: &AppState, id: Uuid, request_id: Uuid) -> AppError {
    fail_start_with(state, id, request_id, None).await
}

/// `fail_start`, plus the reason recorded on whichever startup step was open
/// — that is what tells the dashboard *where* the start died.
async fn fail_start_with(
    state: &AppState,
    id: Uuid,
    request_id: Uuid,
    reason: Option<String>,
) -> AppError {
    finish_startup_timeline(state, id, StartupStepOutcome::Failed, reason);
    let pid = state
        .processes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&id)
        .map(|process| process.pid);
    if let Some(record) = set_memory_state(state, id, VmState::Error) {
        let _ = persist_update(state, &record, request_id).await;
    }
    if let Some(pid) = pid {
        firecracker::sigkill(pid);
    }
    AppError::internal(request_id)
}

/// `leases` is a full snapshot (see `Store::active_leases`), looked up once
/// for the whole list rather than once per VM.
fn sorted_responses(
    vms: &HashMap<Uuid, VmRecord>,
    leases: &HashMap<Uuid, Lease>,
) -> Vec<VmResponse> {
    let mut records: Vec<&VmRecord> = vms.values().collect();
    records.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    records
        .into_iter()
        .map(|vm| vm_response(vm, leases.get(&vm.id)))
        .collect()
}

/// Looks up `vm_id`'s active lease, if any. Best-effort: a lookup failure
/// (task panic or DB error) just means the response's ipv4/mac come back
/// `None` for this one call, not a hard failure — the lease itself is the
/// source of truth and is unaffected either way.
pub(crate) async fn lease_for(state: &AppState, vm_id: Uuid) -> Option<Lease> {
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || store.active_lease(vm_id))
        .await
        .ok()?
        .ok()?
}

pub(crate) fn vm_response(vm: &VmRecord, lease: Option<&Lease>) -> VmResponse {
    VmResponse {
        id: vm.id,
        name: vm.name.clone(),
        state: vm.state,
        template: vm.template.clone(),
        template_version: vm.template_version.clone(),
        cpu: vm.cpu,
        ram: vm.ram,
        disk_gb: vm.disk_gb,
        startup_step: vm.startup_step,
        startup_timeline: vm.startup_timeline.clone(),
        egress_policy: vm.egress_policy,
        package_update: vm.package_update.clone(),
        ipv4: lease.map(|lease| lease.ipv4.to_string()),
        mac: lease.map(|lease| lease.mac.to_string()),
        hostname: firecrab_helper_protocol::network::guest_hostname(vm.id),
        micro_network_id: vm.micro_network_id,
        storage_root: vm.storage_root.clone(),
    }
}

/// Highest disk size the create form accepts; keeps a mistyped value from
/// filling the host disk (`copy` + `resize2fs` both happen synchronously in
/// the start pipeline, so an absurd size would just hang a start).
const MAX_DISK_GB: u16 = 500;

const MIN_RAM_MIB: u32 = 128;
const MAX_RAM_MIB: u32 = 32_768;

/// RAM is restricted to powers of two (128, 256, 512, ... 32768 MiB),
/// matching how cloud instance sizes are usually picked rather than an
/// arbitrary MiB value.
fn is_valid_ram_mib(ram: u32) -> bool {
    (MIN_RAM_MIB..=MAX_RAM_MIB).contains(&ram) && ram.is_power_of_two()
}

fn validate_create(req: &CreateVmRequest, state: &AppState) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    if !valid_vm_name(&req.name) {
        fields.insert(
            "name".to_owned(),
            "must be 1-64 ASCII letters, numbers, '.', '_' or '-'".to_owned(),
        );
    }
    let template = state.templates.resolve_alias(&req.template);
    if template.is_none() {
        fields.insert("template".to_owned(), "is not supported".to_owned());
    }
    if !(1..=32).contains(&req.cpu) {
        fields.insert("cpu".to_owned(), "must be between 1 and 32".to_owned());
    }
    if !is_valid_ram_mib(req.ram) {
        fields.insert(
            "ram".to_owned(),
            "must be a power of two between 128 and 32768 MiB".to_owned(),
        );
    }
    if let Some(template) = &template {
        let min_disk_gb = min_disk_gb_for(template.rootfs.length());
        if !(min_disk_gb..=MAX_DISK_GB).contains(&req.disk_gb) {
            fields.insert(
                "diskGb".to_owned(),
                format!("must be between {min_disk_gb} and {MAX_DISK_GB} GiB"),
            );
        }
    }

    let storage_id = req
        .storage_root
        .as_deref()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| state.storage.default_id());
    if !state.storage_root_known(storage_id) {
        fields.insert(
            "storageRoot".to_owned(),
            "is not a registered storage root".to_owned(),
        );
    } else if fields.get("diskGb").is_none() {
        // Only probe free space once diskGb itself is in range — otherwise
        // the capacity message would hide a simpler validation error.
        let need_bytes = u64::from(req.disk_gb) * 1024 * 1024 * 1024;
        if let Err(error) = state.ensure_storage_capacity(storage_id, need_bytes) {
            fields.insert(
                "storageRoot".to_owned(),
                match error {
                    crate::storage::StorageError::UnknownRoot(_) => {
                        "is not a registered storage root".to_owned()
                    }
                    crate::storage::StorageError::FreeSpace { detail, .. } => {
                        format!("not enough free space ({detail})")
                    }
                },
            );
        }
    }
    fields
}

/// `PUT /api/vms/{id}/storage` — manually reassign a VM to another storage
/// root. Allowed only while the VM is inactive; refused if a rootfs already
/// exists at the old path (no silent disk move).
pub async fn assign_vm_storage(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(req): ValidatedJson<firecrab_api_types::AssignVmStorageRequest>,
) -> Result<Json<VmResponse>, AppError> {
    let id = parse_id(&id, request_id.0)?;
    let target = req.storage_root.trim();
    if target.is_empty() {
        let mut fields = BTreeMap::new();
        fields.insert("storageRoot".to_owned(), "must not be empty".to_owned());
        return Err(AppError::validation(fields, request_id.0));
    }
    if !state.storage_root_known(target) {
        let mut fields = BTreeMap::new();
        fields.insert(
            "storageRoot".to_owned(),
            "is not a registered storage root".to_owned(),
        );
        return Err(AppError::validation(fields, request_id.0));
    }

    let (previous_root, disk_gb, can_edit) = {
        let vms = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let vm = vms
            .get(&id)
            .ok_or_else(|| AppError::not_found(request_id.0))?;
        (
            vm.storage_root.clone(),
            vm.disk_gb,
            vm.state.can_edit_resources(),
        )
    };
    if !can_edit {
        let state_now = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .map(|vm| vm.state)
            .unwrap_or(VmState::Error);
        return Err(AppError::invalid_state(state_now, request_id.0));
    }
    if previous_root == target {
        let lease = lease_for(&state, id).await;
        let vm = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .cloned()
            .ok_or_else(|| AppError::not_found(request_id.0))?;
        return Ok(Json(vm_response(&vm, lease.as_ref())));
    }

    // Refuse if a disk already exists at the old location — moving multi-GB
    // rootfs is out of scope; operator deletes/recreates or keeps the VM.
    let old_paths =
        crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for(&previous_root), id);
    let has_disk = match state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&id)
        .and_then(|vm| vm.disk_generation)
    {
        Some(generation) => old_paths.rootfs(generation).exists(),
        None => old_paths.disks.exists()
            && std::fs::read_dir(&old_paths.disks)
                .map(|entries| entries.filter_map(Result::ok).any(|_| true))
                .unwrap_or(false),
    };
    if has_disk {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            "storage_has_disk",
            "VM already has a disk at the current storage root; delete and recreate to move",
            request_id.0,
        ));
    }

    let need_bytes = u64::from(disk_gb) * 1024 * 1024 * 1024;
    if let Err(error) = state.ensure_storage_capacity(target, need_bytes) {
        let mut fields = BTreeMap::new();
        fields.insert(
            "storageRoot".to_owned(),
            match error {
                crate::storage::StorageError::UnknownRoot(_) => {
                    "is not a registered storage root".to_owned()
                }
                crate::storage::StorageError::FreeSpace { detail, .. } => {
                    format!("not enough free space ({detail})")
                }
            },
        );
        return Err(AppError::validation(fields, request_id.0));
    }

    let updated = {
        let mut vms = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let vm = vms
            .get_mut(&id)
            .ok_or_else(|| AppError::not_found(request_id.0))?;
        vm.storage_root = target.to_owned();
        vm.clone()
    };
    if let Err(error) = persist_update(&state, &updated, request_id.0).await {
        // Roll back memory.
        if let Some(vm) = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(&id)
        {
            vm.storage_root = previous_root;
        }
        return Err(error);
    }
    tracing::info!(
        request_id = %request_id.0,
        vm_id = %id,
        storage_root = %target,
        "vm storage reassigned"
    );
    let lease = lease_for(&state, id).await;
    Ok(Json(vm_response(&updated, lease.as_ref())))
}

/// Smallest disk size that can hold the template's rootfs, rounded up to a
/// whole GiB (ext4 shrink isn't supported, so this is a hard floor).
fn min_disk_gb_for(rootfs_bytes: u64) -> u16 {
    const GIB: u64 = 1024 * 1024 * 1024;
    rootfs_bytes.div_ceil(GIB).try_into().unwrap_or(u16::MAX)
}

fn valid_vm_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Test-only fixtures shared with sibling handler modules (e.g.
/// `handlers::packages`) that need a running-VM-with-a-process to exercise
/// their own handlers, without duplicating this module's DB/rootfs setup.
#[cfg(test)]
pub(crate) mod test_support {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use uuid::Uuid;

    use super::{AppState, VmRecord, VmState};
    use crate::ipam::SubnetSpec;
    use crate::state::RuntimeConfig;
    use crate::templates::{TemplateRegistry, TemplateSpec};

    pub(crate) fn record(name: &str, id: Uuid) -> VmRecord {
        VmRecord {
            id,
            name: name.to_owned(),
            state: VmState::Created,
            template: "ubuntu-rootfs-26.04".to_owned(),
            template_version: "v1".to_owned(),
            template_kernel_sha256: String::new(),
            template_rootfs_sha256: String::new(),
            template_boot_args_sha256: String::new(),
            cpu: 1,
            ram: 128,
            // 0 keeps `run_start`'s disk grow step a no-op against this
            // fixture's fake (non-ext4) rootfs bytes; real growth is
            // covered by `rootfs::tests::grows_a_real_ext4_filesystem_to_the_requested_size`.
            disk_gb: 0,
            egress_policy: Default::default(),
            micro_network_id: Uuid::from_u128(1),
            storage_root: "default".to_owned(),
            disk_generation: None,
            last_runtime_id: None,
            startup_step: None,
            startup_timeline: Vec::new(),
            package_update: None,
        }
    }

    pub(crate) async fn test_state(root: &Path) -> AppState {
        test_state_with_binary(root, PathBuf::from("/nonexistent-firecracker")).await
    }

    pub(crate) async fn test_state_with_binary(root: &Path, binary: PathBuf) -> AppState {
        fs::write(root.join("kernel"), b"kernel").unwrap();
        // A real (if tiny) ext4 image, not just an arbitrary byte blob —
        // `specialize_guest` runs `debugfs` against every VM's own rootfs
        // copy as part of a normal start, so the template itself needs to
        // be a filesystem debugfs can actually open.
        let status = std::process::Command::new("mkfs.ext4")
            .args(["-q", "-F"])
            .arg(root.join("rootfs"))
            .arg("8M")
            .status()
            .expect("mkfs.ext4 must be installed for this test");
        assert!(status.success(), "mkfs.ext4 failed");
        // A blank mkfs.ext4 image has no directories at all — real golden
        // images always have /etc, but this fake template needs it made
        // explicitly for specialize_guest's /etc/hostname write to land.
        std::process::Command::new("debugfs")
            .arg("-w")
            .arg("-R")
            .arg("mkdir /etc")
            .arg(root.join("rootfs"))
            .output()
            .expect("debugfs must be installed for this test");
        let templates = TemplateRegistry::from_specs(
            root,
            [TemplateSpec {
                alias: "ubuntu-rootfs-26.04".to_owned(),
                version: "v1".to_owned(),
                kernel: PathBuf::from("kernel"),
                initrd: None,
                rootfs: PathBuf::from("rootfs"),
                boot_args: "console=ttyS0".to_owned(),
            }],
        )
        .unwrap();
        // Unique per call (not just per `root`): some tests reopen state
        // against the same directory to simulate a restart, and a stale
        // listener would still hold the previous socket path.
        let socket_path = root.join(format!("net-helper-{}.sock", Uuid::new_v4()));
        crate::network::test_support::spawn_always_ok_helper(&socket_path);

        AppState::with_db_file(templates, root.join("data/firecrab.db"))
            .await
            .unwrap()
            .with_test_runtime(RuntimeConfig {
                vms_dir: root.join("vms"),
                firecracker_binary: binary,
                ready_timeout: Duration::from_secs(5),
                stop_grace: Duration::from_millis(500),
                network_ready_timeout: Duration::from_millis(300),
            })
            .with_test_network(crate::network::NetworkClient::with_socket_path(socket_path))
    }

    pub(crate) fn seed_vm(state: &AppState, vm: &VmRecord) {
        state.store.insert(vm).unwrap();
        state.vms.lock().unwrap().insert(vm.id, vm.clone());
        state
            .store
            .allocate_lease(vm.id, SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)))
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use crate::ipam::SubnetSpec;
    use std::fs;
    use std::time::Duration;

    use axum::response::IntoResponse;
    use tempfile::tempdir;

    use super::test_support::{record, seed_vm, test_state, test_state_with_binary};
    use super::*;
    use crate::firecracker::test_support::{
        SERVE_LOOP, SERVE_ONCE_THEN_EXIT, fake_firecracker, process_alive, short_tempdir,
    };

    #[test]
    fn validates_vm_names() {
        assert!(valid_vm_name("vm-01.example"));
        assert!(!valid_vm_name(""));
        assert!(!valid_vm_name("-vm"));
        assert!(!valid_vm_name("vm space"));
        assert!(!valid_vm_name(&"a".repeat(65)));
    }

    #[tokio::test]
    async fn validates_disk_gb_against_the_template_floor_and_fixed_ceiling() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        // The fixture template's rootfs is 6 bytes (`test_state_with_binary`),
        // so `min_disk_gb_for` rounds it up to 1 GiB.
        let base = CreateVmRequest {
            name: "test-vm".to_owned(),
            template: "ubuntu-rootfs-26.04".to_owned(),
            ram: 512,
            cpu: 1,
            disk_gb: 0,
            egress_policy: Default::default(),
            micro_network_id: Uuid::from_u128(1),
            storage_root: None,
        };

        let too_small = validate_create(&base, &state);
        assert!(too_small.contains_key("diskGb"), "{too_small:?}");

        let at_floor = CreateVmRequest {
            disk_gb: 1,
            ..base.clone()
        };
        assert!(!validate_create(&at_floor, &state).contains_key("diskGb"));

        let too_large = CreateVmRequest {
            disk_gb: MAX_DISK_GB + 1,
            ..base.clone()
        };
        assert!(validate_create(&too_large, &state).contains_key("diskGb"));

        let at_ceiling = CreateVmRequest {
            disk_gb: MAX_DISK_GB,
            ..base
        };
        assert!(!validate_create(&at_ceiling, &state).contains_key("diskGb"));
    }

    #[test]
    fn lists_vms_sorted_by_name_then_id() {
        let low = Uuid::from_u128(1);
        let high = Uuid::from_u128(2);
        let vms = HashMap::from([
            (high, record("beta", high)),
            (low, record("beta", low)),
            (Uuid::from_u128(3), record("alpha", Uuid::from_u128(3))),
        ]);

        let responses = sorted_responses(&vms, &HashMap::new());
        let order: Vec<(String, Uuid)> = responses
            .into_iter()
            .map(|response| (response.name, response.id))
            .collect();
        assert_eq!(
            order,
            vec![
                ("alpha".to_owned(), Uuid::from_u128(3)),
                ("beta".to_owned(), low),
                ("beta".to_owned(), high),
            ]
        );
    }

    #[test]
    fn lists_empty_map_as_empty_vec() {
        assert!(sorted_responses(&HashMap::new(), &HashMap::new()).is_empty());
    }

    fn memory_state(state: &AppState, id: Uuid) -> Option<VmState> {
        state.vms.lock().unwrap().get(&id).map(|vm| vm.state)
    }

    fn db_state(state: &AppState, id: Uuid) -> Option<VmState> {
        state.store.load_all().unwrap().get(&id).map(|vm| vm.state)
    }

    async fn wait_for_state(state: &AppState, id: Uuid, want: VmState) {
        for _ in 0..100 {
            if memory_state(state, id) == Some(want) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        panic!("vm {id} never reached {want:?}");
    }

    #[tokio::test]
    async fn list_vms_returns_all_known_vms() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = record("test-vm", Uuid::new_v4());
        state.vms.lock().unwrap().insert(vm.id, vm.clone());

        let Json(body) = list_vms(State(state)).await;

        assert_eq!(body.len(), 1);
        assert_eq!(body[0].id, vm.id);
    }

    #[tokio::test]
    async fn list_vms_includes_the_leased_ipv4_and_mac() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = record("leased-vm", Uuid::new_v4());
        seed_vm(&state, &vm);
        let lease = state.store.active_lease(vm.id).unwrap().unwrap();

        let Json(body) = list_vms(State(state)).await;

        assert_eq!(body.len(), 1);
        assert_eq!(body[0].ipv4, Some(lease.ipv4.to_string()));
        assert_eq!(body[0].mac, Some(lease.mac.to_string()));
    }

    #[tokio::test]
    async fn get_vm_returns_a_known_vm() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = record("test-vm", Uuid::new_v4());
        state.vms.lock().unwrap().insert(vm.id, vm.clone());

        let Json(body) = get_vm(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap();

        assert_eq!(body.id, vm.id);
        assert_eq!(body.name, "test-vm");
    }

    #[tokio::test]
    async fn get_vm_unknown_id_returns_not_found() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let error = get_vm(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(Uuid::new_v4().to_string()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_vm_rejects_a_malformed_uuid() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let error = get_vm(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path("not-a-uuid".to_owned()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn console_log_is_empty_before_any_output_exists() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = record("quiet", Uuid::new_v4());
        seed_vm(&state, &vm);

        let Json(log) = get_vm_log(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap();

        assert_eq!(log.console_log, "");
        assert!(!log.truncated);
    }

    fn seed_console_log(state: &AppState, vm: &mut VmRecord, contents: impl AsRef<[u8]>) {
        let runtime_id = Uuid::new_v4();
        let paths = crate::artifacts::VmArtifactPaths::for_vm(&state.runtime.vms_dir, vm.id);
        let runtime = paths.create_runtime(runtime_id).unwrap();
        fs::write(&runtime.console_log, contents).unwrap();
        vm.last_runtime_id = Some(runtime_id);
        state.store.update(vm).unwrap();
        state.vms.lock().unwrap().insert(vm.id, vm.clone());
    }

    #[tokio::test]
    async fn console_log_returns_file_contents_once_written() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let mut vm = record("chatty", Uuid::new_v4());
        seed_vm(&state, &vm);
        seed_console_log(&state, &mut vm, "booted\nlogin: ");

        let Json(log) = get_vm_log(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap();

        assert_eq!(log.console_log, "booted\nlogin: ");
        assert!(!log.truncated);
    }

    #[tokio::test]
    async fn console_log_truncates_oversized_output() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let mut vm = record("verbose", Uuid::new_v4());
        seed_vm(&state, &vm);
        seed_console_log(
            &state,
            &mut vm,
            "x".repeat(MAX_LOG_BYTES as usize + 1000),
        );

        let Json(log) = get_vm_log(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap();

        assert_eq!(log.console_log.len() as u64, MAX_LOG_BYTES);
        assert!(log.truncated);
    }

    #[tokio::test]
    async fn console_log_unknown_vm_returns_not_found() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let error = get_vm_log(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(Uuid::new_v4().to_string()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn console_log_handles_non_utf8_bytes_without_erroring() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let mut vm = record("binary", Uuid::new_v4());
        seed_vm(&state, &vm);
        seed_console_log(&state, &mut vm, [b'o', b'k', 0xFF, 0xFE, b'\n']);

        let Json(log) = get_vm_log(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap();

        assert!(log.console_log.starts_with("ok"));
        assert!(!log.truncated);
    }

    #[tokio::test]
    async fn a_successful_start_records_every_step_with_its_own_span() {
        let directory = short_tempdir();
        let root = directory.path();
        let binary = fake_firecracker(
            root,
            &format!("signal.signal(signal.SIGTERM, lambda *_: sys.exit(0)){SERVE_LOOP}"),
        );
        let state = test_state_with_binary(root, binary).await;
        let vm = record("timeline", Uuid::new_v4());
        seed_vm(&state, &vm);

        let Json(started) = start_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap();

        // Every step of the pipeline, in the order run_start walks them.
        let steps: Vec<StartupStep> = started
            .startup_timeline
            .iter()
            .map(|run| run.step)
            .collect();
        assert_eq!(
            steps,
            vec![
                StartupStep::PreparingDisk,
                StartupStep::GeneratingConfig,
                StartupStep::StartingProcess,
                StartupStep::ConfiguringNetwork,
            ]
        );

        for run in &started.startup_timeline {
            assert_eq!(
                run.outcome,
                StartupStepOutcome::Succeeded,
                "{:?} should be closed once the VM is running",
                run.step
            );
            let ended = run.ended_at_ms.expect("a closed step has an end");
            assert!(
                ended >= run.started_at_ms,
                "{:?} ended before it started",
                run.step
            );
            assert!(run.detail.is_none(), "only a failed step carries a reason");
        }

        // Steps run back to back, so each one starts where the last ended.
        for pair in started.startup_timeline.windows(2) {
            assert!(
                pair[1].started_at_ms >= pair[0].ended_at_ms.unwrap(),
                "steps must not overlap"
            );
        }
    }

    #[tokio::test]
    async fn a_failed_start_marks_the_step_it_died_on() {
        let directory = short_tempdir();
        let root = directory.path();
        // Exits immediately, so the readiness wait in StartingProcess fails.
        let binary = fake_firecracker(root, "sys.exit(1)\n");
        let state = test_state_with_binary(root, binary).await;
        let vm = record("doomed", Uuid::new_v4());
        seed_vm(&state, &vm);

        let error = start_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );

        let vms = state.vms.lock().unwrap();
        let timeline = &vms.get(&vm.id).unwrap().startup_timeline;
        let failed = timeline
            .iter()
            .find(|run| run.outcome == StartupStepOutcome::Failed)
            .expect("the step that died must be recorded as failed");
        assert!(
            failed.detail.is_some(),
            "a failed step carries why it failed"
        );
        assert!(
            failed.ended_at_ms.is_some(),
            "a failed step is closed, not left open"
        );
        // Nothing ran after it.
        let failed_index = timeline
            .iter()
            .position(|run| run.outcome == StartupStepOutcome::Failed)
            .unwrap();
        assert_eq!(failed_index, timeline.len() - 1);
    }

    #[tokio::test]
    async fn start_then_stop_runs_the_full_lifecycle() {
        let directory = short_tempdir();
        let root = directory.path();
        let binary = fake_firecracker(
            root,
            &format!("signal.signal(signal.SIGTERM, lambda *_: sys.exit(0)){SERVE_LOOP}"),
        );
        let state = test_state_with_binary(root, binary).await;
        let vm = record("lifecycle", Uuid::new_v4());
        seed_vm(&state, &vm);

        let Json(started) = start_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap();

        assert_eq!(started.state, VmState::Running);
        assert_eq!(db_state(&state, vm.id), Some(VmState::Running));
        assert_eq!(
            started.startup_step, None,
            "a running VM must not still be reporting a startup step"
        );
        let pid = state.processes.lock().unwrap().get(&vm.id).unwrap().pid as i32;
        assert!(process_alive(pid));
        let generation = state
            .vms
            .lock()
            .unwrap()
            .get(&vm.id)
            .and_then(|live| live.disk_generation)
            .expect("start must assign a disk generation");
        let disk = crate::artifacts::VmArtifactPaths::for_vm(&state.runtime.vms_dir, vm.id)
            .rootfs(generation);
        assert!(disk.exists(), "start must prepare the VM disk at {disk:?}");

        let Json(stopped) = stop_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap();

        assert_eq!(stopped.state, VmState::Stopped);
        assert_eq!(db_state(&state, vm.id), Some(VmState::Stopped));
        assert!(!process_alive(pid));
        assert!(state.processes.lock().unwrap().is_empty());
    }

    async fn stop_tolerates_a_failed_teardown_step(fail_operation: &'static str) {
        let directory = short_tempdir();
        let root = directory.path();
        let binary = fake_firecracker(
            root,
            &format!("signal.signal(signal.SIGTERM, lambda *_: sys.exit(0)){SERVE_LOOP}"),
        );
        let state = test_state_with_binary(root, binary).await;

        let socket_path = root.join("net-helper-teardown-fail.sock");
        let (_helper, log) = crate::network::test_support::spawn_recording_helper(
            &socket_path,
            Some(fail_operation),
        );
        let state =
            state.with_test_network(crate::network::NetworkClient::with_socket_path(socket_path));

        let vm = record("teardown-partial-fail", Uuid::new_v4());
        seed_vm(&state, &vm);

        let _ = start_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap();

        let Json(stopped) = stop_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap();

        assert_eq!(
            stopped.state,
            VmState::Stopped,
            "a failed {fail_operation} must not block landing on stopped"
        );
        let calls = log.lock().unwrap().clone();
        assert!(
            calls.contains(&"remove_vm_policy") && calls.contains(&"delete_tap"),
            "both teardown steps must still run even when {fail_operation} fails, got {calls:?}"
        );
    }

    #[tokio::test]
    async fn stop_vm_tolerates_a_failed_remove_vm_policy() {
        stop_tolerates_a_failed_teardown_step("remove_vm_policy").await;
    }

    #[tokio::test]
    async fn stop_vm_tolerates_a_failed_delete_tap() {
        stop_tolerates_a_failed_teardown_step("delete_tap").await;
    }

    #[tokio::test]
    async fn start_rejects_disallowed_states_with_conflict() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let mut vm = record("busy", Uuid::new_v4());
        vm.state = VmState::Running;
        seed_vm(&state, &vm);

        let error = start_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
        assert_eq!(memory_state(&state, vm.id), Some(VmState::Running));
    }

    #[tokio::test]
    async fn start_unknown_vm_returns_not_found() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let error = start_vm(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(Uuid::new_v4().to_string()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn start_failure_records_error_state_without_leftovers() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = record("doomed", Uuid::new_v4());
        seed_vm(&state, &vm);

        let error = start_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap_err();

        assert_eq!(
            error.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(memory_state(&state, vm.id), Some(VmState::Error));
        assert_eq!(db_state(&state, vm.id), Some(VmState::Error));
        assert_eq!(
            state.vms.lock().unwrap().get(&vm.id).unwrap().startup_step,
            None,
            "a failed start must not leave a stale startup step behind"
        );
        assert!(state.processes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn start_fails_when_guest_never_reports_network_ready() {
        let directory = short_tempdir();
        let root = directory.path();
        // Boots and serves the readiness probe, like a real guest, but
        // never prints the network sentinel — simulates a guest whose
        // DHCP/DNS check hangs or was never wired up.
        let binary = fake_firecracker(
            root,
            r#"
srv = socket.socket(socket.AF_UNIX)
srv.bind(sock_path)
srv.listen(1)
while True:
    conn, _ = srv.accept()
    conn.recv(1024)
    conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
    conn.close()
"#,
        );
        let state = test_state_with_binary(root, binary).await;
        let vm = record("network-never-ready", Uuid::new_v4());
        seed_vm(&state, &vm);

        let error = start_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap_err();

        assert_eq!(
            error.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(db_state(&state, vm.id), Some(VmState::Error));
    }

    #[tokio::test]
    async fn start_fails_when_guest_reports_network_failure() {
        let directory = short_tempdir();
        let root = directory.path();
        let binary = fake_firecracker(
            root,
            r#"
print("FIRECRAB_NETWORK_FAILED dns unreachable", flush=True)
srv = socket.socket(socket.AF_UNIX)
srv.bind(sock_path)
srv.listen(1)
while True:
    conn, _ = srv.accept()
    conn.recv(1024)
    conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
    conn.close()
"#,
        );
        let state = test_state_with_binary(root, binary).await;
        let vm = record("network-failed", Uuid::new_v4());
        seed_vm(&state, &vm);

        let error = start_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap_err();

        assert_eq!(
            error.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(db_state(&state, vm.id), Some(VmState::Error));
    }

    #[tokio::test]
    async fn apply_vm_policy_failure_still_removes_policy_and_tap() {
        let directory = short_tempdir();
        let root = directory.path();
        let state = test_state(root).await;

        let socket_path = root.join("net-helper-fail.sock");
        let (_helper, log) = crate::network::test_support::spawn_recording_helper(
            &socket_path,
            Some("apply_vm_policy"),
        );
        let state =
            state.with_test_network(crate::network::NetworkClient::with_socket_path(socket_path));

        let vm = record("policy-fail", Uuid::new_v4());
        seed_vm(&state, &vm);

        let error = start_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap_err();

        assert_eq!(
            error.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(db_state(&state, vm.id), Some(VmState::Error));

        let calls = log.lock().unwrap().clone();
        let apply_at = calls
            .iter()
            .position(|&op| op == "apply_vm_policy")
            .expect("apply_vm_policy must have been called");
        assert!(
            calls[apply_at + 1..].contains(&"remove_vm_policy"),
            "expected remove_vm_policy after a failed apply_vm_policy, got {calls:?}"
        );
        assert!(
            calls[apply_at + 1..].contains(&"delete_tap"),
            "expected delete_tap after a failed apply_vm_policy, got {calls:?}"
        );
    }

    #[tokio::test]
    async fn set_startup_step_updates_the_live_record_and_ignores_unknown_ids() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = record("stepping", Uuid::new_v4());
        seed_vm(&state, &vm);

        set_startup_step(&state, vm.id, StartupStep::GeneratingConfig);
        assert_eq!(
            state.vms.lock().unwrap().get(&vm.id).unwrap().startup_step,
            Some(StartupStep::GeneratingConfig)
        );

        // No matching VM: must not panic, and no unrelated record is touched.
        set_startup_step(&state, Uuid::new_v4(), StartupStep::PreparingDisk);
        assert_eq!(
            state.vms.lock().unwrap().get(&vm.id).unwrap().startup_step,
            Some(StartupStep::GeneratingConfig)
        );
    }

    #[tokio::test]
    async fn stop_rejects_non_running_vm() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = record("idle", Uuid::new_v4());
        seed_vm(&state, &vm);

        let error = stop_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
        assert_eq!(memory_state(&state, vm.id), Some(VmState::Created));
    }

    #[tokio::test]
    async fn delete_removes_record_and_directory() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = record("condemned", Uuid::new_v4());
        seed_vm(&state, &vm);
        let paths = crate::artifacts::VmArtifactPaths::for_vm(&state.runtime.vms_dir, vm.id);
        paths.ensure_directories().unwrap();
        let generation = Uuid::new_v4();
        fs::write(paths.rootfs(generation), b"disk").unwrap();

        let status = delete_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap();

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(state.vms.lock().unwrap().is_empty());
        assert!(state.store.load_all().unwrap().is_empty());
        assert!(!paths.dir.exists());

        let error = get_vm(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_rejects_active_vm() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let mut vm = record("active", Uuid::new_v4());
        vm.state = VmState::Running;
        seed_vm(&state, &vm);

        let error = delete_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
        assert_eq!(memory_state(&state, vm.id), Some(VmState::Running));
    }

    #[tokio::test]
    async fn guest_poweroff_lands_on_stopped() {
        let directory = short_tempdir();
        let root = directory.path();
        let binary = fake_firecracker(root, SERVE_ONCE_THEN_EXIT);
        let state = test_state_with_binary(root, binary).await;
        let vm = record("poweroff", Uuid::new_v4());
        seed_vm(&state, &vm);

        let Json(_) = start_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap();

        wait_for_state(&state, vm.id, VmState::Stopped).await;
        assert_eq!(db_state(&state, vm.id), Some(VmState::Stopped));
        assert!(state.processes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn killed_process_lands_on_error() {
        let directory = short_tempdir();
        let root = directory.path();
        let binary = fake_firecracker(root, SERVE_LOOP);
        let state = test_state_with_binary(root, binary).await;

        // Swap in a recording helper so this test can confirm the exit
        // monitor (not stop_vm) is the one tearing the network down when
        // the process dies without ever going through the stop API.
        let socket_path = root.join("net-helper-recording.sock");
        let (_helper, log) =
            crate::network::test_support::spawn_recording_helper(&socket_path, None);
        let state =
            state.with_test_network(crate::network::NetworkClient::with_socket_path(socket_path));

        let vm = record("crashy", Uuid::new_v4());
        seed_vm(&state, &vm);

        let Json(_) = start_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
        )
        .await
        .unwrap();
        let pid = state.processes.lock().unwrap().get(&vm.id).unwrap().pid;

        firecracker::sigkill(pid);

        wait_for_state(&state, vm.id, VmState::Error).await;
        assert_eq!(db_state(&state, vm.id), Some(VmState::Error));
        assert!(state.processes.lock().unwrap().is_empty());

        let calls = log.lock().unwrap().clone();
        assert!(
            calls.contains(&"remove_vm_policy"),
            "exit monitor must tear down the firewall policy on an unclean exit, got {calls:?}"
        );
        assert!(
            calls.contains(&"delete_tap"),
            "exit monitor must delete the TAP on an unclean exit, got {calls:?}"
        );
    }

    #[tokio::test]
    async fn restart_demotes_active_states_to_stopped() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let state = test_state(root).await;
        let mut vm = record("ghost", Uuid::new_v4());
        vm.state = VmState::Running;
        state.store.insert(&vm).unwrap();
        drop(state);

        let reopened = test_state(root).await;

        assert_eq!(memory_state(&reopened, vm.id), Some(VmState::Stopped));
        assert_eq!(db_state(&reopened, vm.id), Some(VmState::Stopped));
    }

    #[tokio::test]
    async fn persistence_failure_does_not_publish_vm_in_memory() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let net = seed_network(&state).await;
        state.store.break_for_tests();

        let result = create_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(create_request_on("test-vm", net)),
        )
        .await;

        assert!(result.is_err());
        assert!(state.vms.lock().unwrap().is_empty());
    }

    fn create_request(name: &str) -> CreateVmRequest {
        create_request_on(name, Uuid::from_u128(1))
    }

    fn create_request_on(name: &str, micro_network_id: Uuid) -> CreateVmRequest {
        CreateVmRequest {
            name: name.to_owned(),
            template: "ubuntu-rootfs-26.04".to_owned(),
            ram: 512,
            cpu: 1,
            disk_gb: 2,
            egress_policy: Default::default(),
            micro_network_id,
            storage_root: None,
        }
    }

    async fn seed_network(state: &AppState) -> Uuid {
        let (_, Json(network)) = crate::handlers::micro_networks::create_micro_network(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(firecrab_api_types::CreateMicroNetworkRequest {
                name: "test-net".to_owned(),
                subnet_cidr: "172.30.0.0/24".to_owned(),
                internet_enabled: true,
            }),
        )
        .await
        .expect("seed micro network");
        network.id
    }

    #[tokio::test]
    async fn assign_vm_storage_updates_root_before_disk_exists() {
        let directory = tempdir().unwrap();
        let pool = directory.path().join("assigned-pool");
        let state = test_state(directory.path()).await;
        let net = seed_network(&state).await;
        let (_, Json(ms)) = crate::handlers::micro_storages::create_micro_storage(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(firecrab_api_types::CreateMicroStorageRequest {
                name: "assigned-pool".to_owned(),
                path: pool.display().to_string(),
            }),
        )
        .await
        .unwrap();

        let (_, Json(created)) = create_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(create_request_on("move-me", net)),
        )
        .await
        .unwrap();
        assert_eq!(created.storage_root, "default");

        let Json(reassigned) = assign_vm_storage(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(created.id.to_string()),
            ValidatedJson(firecrab_api_types::AssignVmStorageRequest {
                storage_root: ms.id.to_string(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(reassigned.storage_root, ms.id.to_string());
        assert_eq!(
            state.vms_dir_for(&reassigned.storage_root),
            pool.join("vms")
        );
    }

    #[tokio::test]
    async fn create_vm_uses_selected_storage_root_and_rejects_unknown_or_full_roots() {
        let directory = tempdir().unwrap();
        let disk_a = directory.path().join("disk-a");
        let disk_b = directory.path().join("disk-b");
        std::fs::create_dir_all(&disk_a).unwrap();
        std::fs::create_dir_all(&disk_b).unwrap();
        let state = test_state(directory.path())
            .await
            .with_test_storage(
                crate::storage::StorageRegistry::parse(&format!(
                    "disk-a={}:disk-b={}",
                    disk_a.display(),
                    disk_b.display()
                ))
                .unwrap(),
            );
        let net = seed_network(&state).await;

        let unknown = validate_create(
            &CreateVmRequest {
                storage_root: Some("nope".to_owned()),
                ..create_request_on("x", net)
            },
            &state,
        );
        assert!(unknown.contains_key("storageRoot"), "{unknown:?}");

        let (_, Json(created)) = create_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(CreateVmRequest {
                storage_root: Some("disk-b".to_owned()),
                ..create_request_on("on-b", net)
            }),
        )
        .await
        .expect("create on disk-b");
        assert_eq!(created.storage_root, "disk-b");
        assert_eq!(
            state.vms.lock().unwrap().get(&created.id).unwrap().storage_root,
            "disk-b"
        );

        let ok = validate_create(
            &CreateVmRequest {
                storage_root: Some("disk-a".to_owned()),
                ..create_request_on("ok", net)
            },
            &state,
        );
        assert!(!ok.contains_key("storageRoot"), "{ok:?}");

        // Free-space rejection at create time (before any disk copy).
        let free = crate::storage::available_bytes(&disk_a).unwrap();
        let need_gib = (free / (1024 * 1024 * 1024)).saturating_add(100);
        if need_gib <= u64::from(MAX_DISK_GB) {
            let full = validate_create(
                &CreateVmRequest {
                    disk_gb: need_gib as u16,
                    storage_root: Some("disk-a".to_owned()),
                    ..create_request_on("huge", net)
                },
                &state,
            );
            assert!(
                full.get("storageRoot")
                    .is_some_and(|msg| msg.contains("free space")),
                "{full:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_vm_in_a_micro_network_leases_from_that_networks_subnet() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let (_, Json(network)) = crate::handlers::micro_networks::create_micro_network(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(firecrab_api_types::CreateMicroNetworkRequest {
                name: "prod".to_owned(),
                subnet_cidr: "172.31.0.0/24".to_owned(),
                internet_enabled: true,
            }),
        )
        .await
        .expect("create micro network");

        let (_, Json(created)) = create_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(CreateVmRequest {
                micro_network_id: network.id,
                ..create_request_on("in-prod", network.id)
            }),
        )
        .await
        .expect("create vm");

        assert_eq!(created.micro_network_id, network.id);
        assert_eq!(
            created
                .ipv4
                .as_deref()
                .map(|ip| ip.starts_with("172.31.0.")),
            Some(true),
            "lease must come from the MicroNetwork's subnet: {:?}",
            created.ipv4
        );
    }

    #[tokio::test]
    async fn creating_a_vm_in_an_unknown_micro_network_is_rejected_without_a_record() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let net = seed_network(&state).await;

        let error = create_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(CreateVmRequest {
                micro_network_id: Uuid::new_v4(),
                ..create_request_on("orphan", net)
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
        assert!(
            state.store.load_all().unwrap().is_empty(),
            "a rejected create must not leave the VM record behind"
        );
    }

    #[tokio::test]
    async fn create_vm_succeeds_and_allocates_a_lease() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let net = seed_network(&state).await;

        let (status, Json(created)) = create_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(create_request_on("fresh-vm", net)),
        )
        .await
        .unwrap();

        assert_eq!(status, StatusCode::CREATED);
        assert!(state.vms.lock().unwrap().contains_key(&created.id));
        let lease = state
            .store
            .active_lease(created.id)
            .unwrap()
            .expect("create_vm must allocate a lease up front, not on first start");
        assert_eq!(created.ipv4, Some(lease.ipv4.to_string()));
        assert_eq!(created.mac, Some(lease.mac.to_string()));
        assert_eq!(
            created.hostname,
            firecrab_helper_protocol::network::guest_hostname(created.id)
        );
        assert_eq!(created.egress_policy, EgressPolicy::Internet);
    }

    #[tokio::test]
    async fn create_vm_rolls_back_the_record_when_lease_allocation_fails() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let net = seed_network(&state).await;

        // Exhaust the 253-address pool so the lease allocation inside
        // create_vm fails after the VM record has already been inserted.
        for _ in 0..253 {
            state
                .store
                .allocate_lease(Uuid::new_v4(), SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)))
                .unwrap();
        }

        let result = create_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(create_request_on("doomed-vm", net)),
        )
        .await;

        assert!(result.is_err());
        assert!(
            state.vms.lock().unwrap().is_empty(),
            "a failed lease allocation must roll back the just-inserted VM record"
        );
        assert!(state.store.load_all().unwrap().is_empty());
    }

    fn update_request(cpu: u8, ram: u32, disk_gb: u16) -> UpdateVmResourcesRequest {
        UpdateVmResourcesRequest {
            cpu,
            ram,
            disk_gb,
            egress_policy: Default::default(),
        }
    }

    #[tokio::test]
    async fn update_applies_new_resources_for_an_editable_vm() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = record("resizable", Uuid::new_v4());
        seed_vm(&state, &vm);

        let Json(updated) = update_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
            ValidatedJson(update_request(4, 1024, 3)),
        )
        .await
        .unwrap();

        assert_eq!(updated.cpu, 4);
        assert_eq!(updated.ram, 1024);
        assert_eq!(updated.disk_gb, 3);
        let reopened = test_state(directory.path()).await;
        assert_eq!(db_state(&reopened, vm.id), Some(VmState::Created));
        let persisted = reopened.store.load_all().unwrap();
        let persisted = persisted.get(&vm.id).unwrap();
        assert_eq!(
            (persisted.cpu, persisted.ram, persisted.disk_gb),
            (4, 1024, 3)
        );
    }

    #[tokio::test]
    async fn update_vm_can_change_the_egress_policy() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = record("policy-editable", Uuid::new_v4());
        seed_vm(&state, &vm);
        assert_eq!(vm.egress_policy, EgressPolicy::Internet);

        let request = UpdateVmResourcesRequest {
            egress_policy: EgressPolicy::Isolated,
            ..update_request(vm.cpu, vm.ram, vm.disk_gb)
        };
        let Json(updated) = update_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
            ValidatedJson(request),
        )
        .await
        .unwrap();

        assert_eq!(updated.egress_policy, EgressPolicy::Isolated);
        let persisted = state.store.load_all().unwrap();
        assert_eq!(
            persisted.get(&vm.id).unwrap().egress_policy,
            EgressPolicy::Isolated
        );
    }

    #[tokio::test]
    async fn update_rejects_a_running_vm_with_conflict() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let mut vm = record("busy-resize", Uuid::new_v4());
        vm.state = VmState::Running;
        seed_vm(&state, &vm);

        let error = update_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
            ValidatedJson(update_request(4, 1024, 3)),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
        let vms = state.vms.lock().unwrap();
        let unchanged = vms.get(&vm.id).unwrap();
        assert_eq!(
            (unchanged.cpu, unchanged.ram, unchanged.disk_gb),
            (1, 128, 0)
        );
    }

    #[tokio::test]
    async fn update_rejects_shrinking_the_disk() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let mut vm = record("no-shrink", Uuid::new_v4());
        vm.disk_gb = 5;
        seed_vm(&state, &vm);

        let error = update_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(vm.id.to_string()),
            ValidatedJson(update_request(1, 128, 4)),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
        assert_eq!(memory_state(&state, vm.id), Some(VmState::Created));
        let vms = state.vms.lock().unwrap();
        assert_eq!(vms.get(&vm.id).unwrap().disk_gb, 5);
    }

    #[tokio::test]
    async fn update_unknown_vm_returns_not_found() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let error = update_vm(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            axum::extract::Path(Uuid::new_v4().to_string()),
            ValidatedJson(update_request(1, 128, 2)),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn find_network_sentinel_recognizes_ready_and_failed_lines() {
        assert_eq!(
            find_network_sentinel(b"boot log\nFIRECRAB_NETWORK_READY 172.30.0.5\nmore\n"),
            Some(Ok(()))
        );
        assert!(matches!(
            find_network_sentinel(b"FIRECRAB_NETWORK_FAILED dns unreachable\n"),
            Some(Err(reason)) if reason.contains("dns unreachable")
        ));
        assert_eq!(find_network_sentinel(b"just a normal boot line\n"), None);
    }

    #[test]
    fn find_network_sentinel_matches_across_a_reassembled_buffer() {
        // wait_for_network_ready re-scans the whole rolling buffer on every
        // new chunk specifically so a sentinel line split across two
        // separate console reads is still found once both have arrived.
        let mut buffer = b"boot log FIRECRAB_NETWORK".to_vec();
        assert_eq!(find_network_sentinel(&buffer), None);
        buffer.extend_from_slice(b"_READY 172.30.0.5\n");
        assert_eq!(find_network_sentinel(&buffer), Some(Ok(())));
    }

    #[tokio::test]
    async fn wait_for_network_ready_survives_a_lagged_broadcast_receiver() {
        // Regression test for the concurrent-boot bug: a receiver that falls
        // behind the console broadcast (many VMs booting at once starve the
        // consumer task of CPU) must not be treated as the console having
        // closed. Flood well past the broadcast capacity before the sentinel
        // arrives, so the waiter is guaranteed to observe `Lagged` first.
        let console = std::sync::Arc::new(crate::console::ConsoleBroker::new());
        let waiter = tokio::spawn({
            let console = console.clone();
            async move { wait_for_network_ready(&console, Duration::from_secs(5)).await }
        });

        // Let the spawned task reach its subscribe() call before the flood,
        // so the flood lags its receiver instead of only filling backlog.
        tokio::task::yield_now().await;
        for _ in 0..300 {
            console.push_output(b"boot noise\n");
        }
        console.push_output(b"FIRECRAB_NETWORK_READY 172.30.0.5\n");

        assert_eq!(waiter.await.expect("waiter task panicked"), Ok(()));
    }

    #[test]
    fn ram_must_be_a_power_of_two_within_range() {
        for valid in [128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768] {
            assert!(is_valid_ram_mib(valid), "{valid} should be valid");
        }
        for invalid in [0, 64, 100, 127, 512 + 1, 3000, 32768 + 1, 65536] {
            assert!(!is_valid_ram_mib(invalid), "{invalid} should be invalid");
        }
    }

    #[test]
    fn validates_update_cpu_ram_and_disk_bounds() {
        let valid = update_request(2, 512, 5);
        assert!(validate_update(&valid, 5).is_empty());

        let bad_cpu = update_request(0, 512, 5);
        assert!(validate_update(&bad_cpu, 5).contains_key("cpu"));

        let bad_ram = update_request(2, 64, 5);
        assert!(validate_update(&bad_ram, 5).contains_key("ram"));

        let shrink = update_request(2, 512, 4);
        assert!(validate_update(&shrink, 5).contains_key("diskGb"));

        let too_big = update_request(2, 512, MAX_DISK_GB + 1);
        assert!(validate_update(&too_big, 5).contains_key("diskGb"));
    }
}

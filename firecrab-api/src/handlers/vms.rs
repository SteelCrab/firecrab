use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::PathBuf;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use firecrab_api_types::{VmLogResponse, VmResponse};
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::firecracker::{self, FirecrackerProcess, VmNetwork, VmProcess};
use crate::ipam::IpamError;
use crate::model::{CreateVmRequest, StartupStep, UpdateVmResourcesRequest, VmRecord, VmState};
use crate::network_policy::EgressPolicy;
use crate::persistence::PersistenceError;
use crate::rootfs;
use crate::server::RequestId;
use crate::state::AppState;

pub async fn list_vms(State(state): State<AppState>) -> Json<Vec<VmResponse>> {
    let vms = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Json(sorted_responses(&vms))
}

pub async fn get_vm(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<VmResponse>, AppError> {
    let id = parse_id(&id, request_id.0)?;
    let vms = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    vms.get(&id)
        .map(vm_response)
        .map(Json)
        .ok_or_else(|| AppError::not_found(request_id.0))
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
    {
        let vms = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !vms.contains_key(&id) {
            return Err(AppError::not_found(request_id.0));
        }
    }

    let vms_dir = state.runtime.vms_dir.clone();
    tokio::task::spawn_blocking(move || read_console_log(&vms_dir, id))
        .await
        .map(Json)
        .map_err(|_| AppError::internal(request_id.0))
}

fn read_console_log(vms_dir: &std::path::Path, id: Uuid) -> VmLogResponse {
    let path = firecracker::console_log_path(vms_dir, id);
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
        state: VmState::Created,
        startup_step: None,
    };

    let response = vm_response(&vm);
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
    // see Store::active_lease's doc comment.
    let store = state.store.clone();
    let vm_id = vm.id;
    if let Err(error) = tokio::task::spawn_blocking(move || store.allocate_lease(vm_id))
        .await
        .map_err(|_| AppError::internal(request_id.0))?
    {
        tracing::error!(request_id = %request_id.0, vm_id = %vm_id, %error, "failed to allocate vm lease");
        let store = state.store.clone();
        let _ = tokio::task::spawn_blocking(move || store.delete(vm_id)).await;
        return Err(AppError::internal(request_id.0));
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
    Ok(Json(vm_response(&updated)))
}

type PreviousResources = (u8, u32, u16);

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
    let previous = (vm.cpu, vm.ram, vm.disk_gb);
    vm.cpu = req.cpu;
    vm.ram = req.ram;
    vm.disk_gb = req.disk_gb;
    Ok((vm.clone(), previous))
}

fn restore_resources(state: &AppState, id: Uuid, previous: PreviousResources) {
    let mut vms = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(vm) = vms.get_mut(&id) {
        (vm.cpu, vm.ram, vm.disk_gb) = previous;
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
            return Err(fail_start(state, id, request_id.0).await);
        }
    };
    let pid = process.pid();

    firecracker::register_and_watch(state, id, process);

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
            Ok(Json(vm_response(&running)))
        }
        // The guest exited before we could record running; the exit monitor
        // already landed the record on its terminal state.
        None => state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .map(vm_response)
            .map(Json)
            .ok_or_else(|| AppError::not_found(request_id.0)),
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
    Ok(Json(vm_response(&stopped)))
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
    let vm_dir = state.runtime.vms_dir.join(id.to_string());
    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        match std::fs::remove_dir_all(&vm_dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("failed to remove {}: {error}", vm_dir.display()));
            }
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
    tracing::info!(request_id = %request_id.0, vm_id = %id, "vm deleted");
    Ok(StatusCode::NO_CONTENT)
}

fn parse_id(id: &str, request_id: Uuid) -> Result<Uuid, AppError> {
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
    let network = setup_vm_network(state, vm.id).await?;

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
    let runtime = state.runtime.clone();
    let record = vm.clone();
    let state_for_blocking = state.clone();
    let network = network.clone();
    let config_path = tokio::task::spawn_blocking(move || -> Result<PathBuf, String> {
        let _permit = permit;
        let mut source = templates
            .open_verified(&template.rootfs)
            .map_err(|error| format!("rootfs verification failed: {error}"))?;
        let target_bytes = u64::from(record.disk_gb) * 1024 * 1024 * 1024;
        rootfs::prepare_rootfs(&runtime.vms_dir, record.id, &mut source, target_bytes)
            .map_err(|error| format!("rootfs preparation failed: {error}"))?;

        set_startup_step(
            &state_for_blocking,
            record.id,
            StartupStep::GeneratingConfig,
        );
        let kernel = templates.artifact_path(&template.kernel);
        firecracker::write_config(
            &runtime.vms_dir,
            &record,
            &kernel,
            &template.boot_args,
            Some(&network),
        )
        .map_err(|error| format!("config generation failed: {error}"))
    })
    .await
    .map_err(|error| format!("start preparation task failed: {error}"))??;

    set_startup_step(state, vm.id, StartupStep::StartingProcess);

    firecracker::spawn_vm(
        &state.runtime.firecracker_binary,
        &state.runtime.vms_dir,
        vm.id,
        &config_path,
        state.runtime.ready_timeout,
    )
    .await
    .map_err(|error| error.to_string())
}

/// Ensures the bridge/firewall are up, creates `vm_id`'s TAP, and applies its
/// isolation policy for its (already-allocated, see `create_vm`) lease. Any
/// failure after the TAP is created tears the TAP itself back down before
/// returning, so a partial failure here never leaves an unprotected TAP
/// attached to the bridge.
async fn setup_vm_network(state: &AppState, vm_id: Uuid) -> Result<VmNetwork, String> {
    let store = state.store.clone();
    let lease = tokio::task::spawn_blocking(move || store.active_lease(vm_id))
        .await
        .map_err(|error| format!("lease lookup task failed: {error}"))?
        .map_err(|error| format!("lease lookup failed: {error}"))?
        .ok_or_else(|| format!("vm {vm_id} has no allocated lease"))?;

    state
        .network
        .ensure_bridge()
        .await
        .map_err(|error| format!("ensure_bridge failed: {error}"))?;
    state
        .network
        .ensure_firewall()
        .await
        .map_err(|error| format!("ensure_firewall failed: {error}"))?;

    let tap_name = state
        .network
        .create_tap(vm_id)
        .await
        .map_err(|error| format!("tap creation failed: {error}"))?;

    if let Err(error) = state
        .network
        .apply_vm_policy(vm_id, lease.ipv4, lease.mac, EgressPolicy::default(), false)
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

/// Records the current phase of an in-flight start so pollers can show *why*
/// a VM hasn't reached `running` yet. Silently a no-op once the record has
/// moved on (e.g. a concurrent failure already landed it on `error`).
fn set_startup_step(state: &AppState, id: Uuid, step: StartupStep) {
    let mut vms = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(vm) = vms.get_mut(&id) {
        vm.startup_step = Some(step);
    }
}

/// Failure tail of the start flow: record `error`, then make sure no process
/// survives (the monitor sees `error` and leaves the record alone).
async fn fail_start(state: &AppState, id: Uuid, request_id: Uuid) -> AppError {
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

fn sorted_responses(vms: &HashMap<Uuid, VmRecord>) -> Vec<VmResponse> {
    let mut records: Vec<&VmRecord> = vms.values().collect();
    records.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    records.into_iter().map(vm_response).collect()
}

fn vm_response(vm: &VmRecord) -> VmResponse {
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
    fields
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use axum::response::IntoResponse;
    use tempfile::tempdir;

    use super::*;
    use crate::firecracker::test_support::{
        SERVE_LOOP, SERVE_ONCE_THEN_EXIT, fake_firecracker, process_alive, short_tempdir,
    };
    use crate::state::RuntimeConfig;
    use crate::templates::{TemplateRegistry, TemplateSpec};

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

    fn record(name: &str, id: Uuid) -> VmRecord {
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
            startup_step: None,
        }
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

        let responses = sorted_responses(&vms);
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
        assert!(sorted_responses(&HashMap::new()).is_empty());
    }

    async fn test_state(root: &Path) -> AppState {
        test_state_with_binary(root, PathBuf::from("/nonexistent-firecracker")).await
    }

    async fn test_state_with_binary(root: &Path, binary: PathBuf) -> AppState {
        fs::write(root.join("kernel"), b"kernel").unwrap();
        fs::write(root.join("rootfs"), b"rootfs").unwrap();
        let templates = TemplateRegistry::from_specs(
            root,
            [TemplateSpec {
                alias: "ubuntu-rootfs-26.04".to_owned(),
                version: "v1".to_owned(),
                kernel: PathBuf::from("kernel"),
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
            })
            .with_test_network(crate::network::NetworkClient::with_socket_path(socket_path))
    }

    fn seed_vm(state: &AppState, vm: &VmRecord) {
        state.store.insert(vm).unwrap();
        state.vms.lock().unwrap().insert(vm.id, vm.clone());
        state.store.allocate_lease(vm.id).unwrap();
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

    #[tokio::test]
    async fn console_log_returns_file_contents_once_written() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = record("chatty", Uuid::new_v4());
        seed_vm(&state, &vm);
        let path = firecracker::console_log_path(&state.runtime.vms_dir, vm.id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "booted\nlogin: ").unwrap();

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
        let vm = record("verbose", Uuid::new_v4());
        seed_vm(&state, &vm);
        let path = firecracker::console_log_path(&state.runtime.vms_dir, vm.id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "x".repeat(MAX_LOG_BYTES as usize + 1000)).unwrap();

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
        let vm = record("binary", Uuid::new_v4());
        seed_vm(&state, &vm);
        let path = firecracker::console_log_path(&state.runtime.vms_dir, vm.id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, [b'o', b'k', 0xFF, 0xFE, b'\n']).unwrap();

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
        assert!(
            rootfs::rootfs_path(&state.runtime.vms_dir, vm.id).exists(),
            "start must prepare the VM disk"
        );

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

        start_vm(
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
        let vm_dir = state.runtime.vms_dir.join(vm.id.to_string());
        fs::create_dir_all(&vm_dir).unwrap();
        fs::write(vm_dir.join("rootfs.ext4"), b"disk").unwrap();

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
        assert!(!vm_dir.exists());

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
        state.store.break_for_tests();

        let result = create_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(create_request("test-vm")),
        )
        .await;

        assert!(result.is_err());
        assert!(state.vms.lock().unwrap().is_empty());
    }

    fn create_request(name: &str) -> CreateVmRequest {
        CreateVmRequest {
            name: name.to_owned(),
            template: "ubuntu-rootfs-26.04".to_owned(),
            ram: 512,
            cpu: 1,
            disk_gb: 2,
        }
    }

    #[tokio::test]
    async fn create_vm_succeeds_and_allocates_a_lease() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let (status, Json(created)) = create_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(create_request("fresh-vm")),
        )
        .await
        .unwrap();

        assert_eq!(status, StatusCode::CREATED);
        assert!(state.vms.lock().unwrap().contains_key(&created.id));
        assert!(
            state.store.active_lease(created.id).unwrap().is_some(),
            "create_vm must allocate a lease up front, not on first start"
        );
    }

    #[tokio::test]
    async fn create_vm_rolls_back_the_record_when_lease_allocation_fails() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        // Exhaust the 253-address pool so the lease allocation inside
        // create_vm fails after the VM record has already been inserted.
        for _ in 0..253 {
            state.store.allocate_lease(Uuid::new_v4()).unwrap();
        }

        let result = create_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(create_request("doomed-vm")),
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
        UpdateVmResourcesRequest { cpu, ram, disk_gb }
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
        let reopened = {
            let state = test_state(directory.path()).await;
            state
        };
        assert_eq!(db_state(&reopened, vm.id), Some(VmState::Created));
        let persisted = reopened.store.load_all().unwrap();
        let persisted = persisted.get(&vm.id).unwrap();
        assert_eq!(
            (persisted.cpu, persisted.ram, persisted.disk_gb),
            (4, 1024, 3)
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

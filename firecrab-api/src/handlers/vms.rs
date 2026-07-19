use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use firecrab_api_types::VmResponse;
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::firecracker::{self, FirecrackerProcess, VmProcess};
use crate::model::{CreateVmRequest, VmRecord, VmState};
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

pub async fn create_vm(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    ValidatedJson(req): ValidatedJson<CreateVmRequest>,
) -> Result<(StatusCode, Json<VmResponse>), AppError> {
    let fields = validate_create(&req, &state);
    if !fields.is_empty() {
        return Err(AppError::validation(fields, request_id.0));
    }
    let template = state
        .templates
        .resolve_alias(&req.template)
        .ok_or_else(|| AppError::internal(request_id.0))?;
    let verify_templates = state.templates.clone();
    let verify_template = template.clone();
    tokio::task::spawn_blocking(move || {
        verify_templates.open_verified(&verify_template.kernel)?;
        verify_templates.open_verified(&verify_template.rootfs)
    })
    .await
    .map_err(|_| AppError::internal(request_id.0))?
    .map_err(|_| AppError::internal(request_id.0))?;
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
        state: VmState::Created,
    };

    let response = vm_response(&vm);
    let store = state.store.clone();
    let record = vm.clone();
    tokio::task::spawn_blocking(move || store.insert(&record))
        .await
        .map_err(|_| AppError::internal(request_id.0))?
        .map_err(|error| {
            eprintln!(
                "[ERROR] request_id={} failed to persist VM state: {error}",
                request_id.0
            );
            AppError::internal(request_id.0)
        })?;
    state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(vm.id, vm);

    Ok((StatusCode::CREATED, Json(response)))
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

    let process = match run_start(&state, &claimed).await {
        Ok(process) => process,
        Err(reason) => {
            eprintln!(
                "[ERROR] request_id={} vm {id} start failed: {reason}",
                request_id.0
            );
            return Err(fail_start(&state, id, request_id.0).await);
        }
    };

    firecracker::register_and_watch(&state, id, process);

    match transition_if(&state, id, VmState::Starting, VmState::Running) {
        Some(running) => {
            if persist_update(&state, &running, request_id.0).await.is_err() {
                return Err(fail_start(&state, id, request_id.0).await);
            }
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
    if let Some(VmProcess { pid, mut exited }) = entry {
        firecracker::sigterm(pid);
        if tokio::time::timeout(state.runtime.stop_grace, exited.changed())
            .await
            .is_err()
        {
            firecracker::sigkill(pid);
            let _ = tokio::time::timeout(state.runtime.stop_grace, exited.changed()).await;
        }
    }

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
            Ok(()) | Err(PersistenceError::MissingVm { .. }) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|inner| inner);

    if let Err(reason) = result {
        eprintln!(
            "[ERROR] request_id={} vm {id} delete failed: {reason}",
            request_id.0
        );
        state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id, removed);
        return Err(AppError::internal(request_id.0));
    }
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
    Ok((vm.clone(), previous))
}

fn set_memory_state(state: &AppState, id: Uuid, to: VmState) -> Option<VmRecord> {
    let mut vms = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    vms.get_mut(&id).map(|vm| {
        vm.state = to;
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
            eprintln!("[ERROR] request_id={request_id} failed to persist VM state: {error}");
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

    let templates = state.templates.clone();
    let runtime = state.runtime.clone();
    let record = vm.clone();
    let config_path = tokio::task::spawn_blocking(move || -> Result<PathBuf, String> {
        let mut source = templates
            .open_verified(&template.rootfs)
            .map_err(|error| format!("rootfs verification failed: {error}"))?;
        rootfs::prepare_rootfs(&runtime.vms_dir, record.id, &mut source)
            .map_err(|error| format!("rootfs preparation failed: {error}"))?;
        let kernel = templates.artifact_path(&template.kernel);
        firecracker::write_config(&runtime.vms_dir, &record, &kernel, &template.boot_args)
            .map_err(|error| format!("config generation failed: {error}"))
    })
    .await
    .map_err(|error| format!("start preparation task failed: {error}"))??;

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
    }
}

fn validate_create(req: &CreateVmRequest, state: &AppState) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    if !valid_vm_name(&req.name) {
        fields.insert(
            "name".to_owned(),
            "must be 1-64 ASCII letters, numbers, '.', '_' or '-'".to_owned(),
        );
    }
    if state.templates.resolve_alias(&req.template).is_none() {
        fields.insert("template".to_owned(), "is not supported".to_owned());
    }
    if !(1..=32).contains(&req.cpu) {
        fields.insert("cpu".to_owned(), "must be between 1 and 32".to_owned());
    }
    if !(128..=32_768).contains(&req.ram) {
        fields.insert(
            "ram".to_owned(),
            "must be between 128 and 32768 MiB".to_owned(),
        );
    }
    fields
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
        AppState::with_db_file(templates, root.join("data/firecrab.db"))
            .await
            .unwrap()
            .with_test_runtime(RuntimeConfig {
                vms_dir: root.join("vms"),
                firecracker_binary: binary,
                ready_timeout: Duration::from_secs(5),
                stop_grace: Duration::from_millis(500),
            })
    }

    fn seed_vm(state: &AppState, vm: &VmRecord) {
        state.store.insert(vm).unwrap();
        state.vms.lock().unwrap().insert(vm.id, vm.clone());
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
        assert!(state.processes.lock().unwrap().is_empty());
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

        let request = CreateVmRequest {
            name: "test-vm".to_owned(),
            template: "ubuntu-rootfs-26.04".to_owned(),
            ram: 512,
            cpu: 1,
        };
        let result = create_vm(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(request),
        )
        .await;

        assert!(result.is_err());
        assert!(state.vms.lock().unwrap().is_empty());
    }
}

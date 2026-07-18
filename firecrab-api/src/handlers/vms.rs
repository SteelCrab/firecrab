use std::collections::{BTreeMap, HashMap};

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use firecrab_api_types::VmResponse;
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::model::{CreateVmRequest, VmRecord, VmState};
use crate::persistence;
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
    let id = Uuid::parse_str(&id).map_err(|_| {
        let mut fields = BTreeMap::new();
        fields.insert("id".to_owned(), "must be a UUID".to_owned());
        AppError::validation(fields, request_id.0)
    })?;
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
    let _writer = state.persistence_writer.lock().await;
    let mut snapshot = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    snapshot.insert(vm.id, vm.clone());
    persistence::save(&state.data_file, &snapshot)
        .await
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

    use axum::response::IntoResponse;
    use tempfile::tempdir;

    use super::*;
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
        AppState::with_data_file(templates, root.join("data/vms.json"))
            .await
            .unwrap()
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
    async fn persistence_failure_does_not_publish_vm_in_memory() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        fs::write(directory.path().join("data"), b"not a directory").unwrap();

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

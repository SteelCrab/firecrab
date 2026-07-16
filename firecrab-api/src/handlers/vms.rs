use std::collections::{BTreeMap, HashMap};

use axum::Json;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use firecrab_api_types::VmResponse;
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::model::{CreateVmRequest, VmRecord, VmState};
use crate::persistence;
use crate::server::RequestId;
use crate::state::AppState;

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
    let template = state
        .templates
        .resolve_version(&template.name, &template.version)
        .ok_or_else(|| AppError::internal(request_id.0))?;
    state
        .templates
        .open_verified(&template.kernel)
        .map_err(|_| AppError::internal(request_id.0))?;
    state
        .templates
        .open_verified(&template.rootfs)
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
    let mut vms = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    vms.insert(vm.id, vm);
    persistence::save(&vms);

    Ok((StatusCode::CREATED, Json(response)))
}

pub async fn list_vms(State(state): State<AppState>) -> Json<Vec<VmResponse>> {
    let vms = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Json(sorted_responses(&vms))
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
    if !state.templates.contains(&req.template) {
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
    use super::*;

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
            template_version: "ubuntu-26.04-v1".to_owned(),
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
}

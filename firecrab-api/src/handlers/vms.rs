use std::collections::BTreeMap;

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

    let mut vms = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    vms.insert(vm.id, vm.clone());
    persistence::save(&vms);

    let response = VmResponse {
        id: vm.id,
        name: vm.name,
        state: vm.state,
        template: vm.template,
        template_version: vm.template_version,
        cpu: vm.cpu,
        ram: vm.ram,
    };
    Ok((StatusCode::CREATED, Json(response)))
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
    use super::valid_vm_name;

    #[test]
    fn validates_vm_names() {
        assert!(valid_vm_name("vm-01.example"));
        assert!(!valid_vm_name(""));
        assert!(!valid_vm_name("-vm"));
        assert!(!valid_vm_name("vm space"));
        assert!(!valid_vm_name(&"a".repeat(65)));
    }
}

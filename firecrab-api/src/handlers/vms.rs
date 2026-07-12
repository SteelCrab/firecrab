use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use uuid::Uuid;

use crate::model::{CreateVmRequest, VmRecord, VmState};
use crate::persistence;
use crate::state::AppState;

pub async fn create_vm(
    State(state): State<AppState>,
    Json(req): Json<CreateVmRequest>,
) -> Result<(StatusCode, Json<VmRecord>), StatusCode> {
    if !state.templates.contains(&req.template) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let template = state
        .templates
        .resolve_alias(&req.template)
        .ok_or(StatusCode::BAD_REQUEST)?;
    let template = state
        .templates
        .resolve_version(&template.name, &template.version)
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .templates
        .open_verified(&template.kernel)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state
        .templates
        .open_verified(&template.rootfs)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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

    Ok((StatusCode::CREATED, Json(vm)))
}

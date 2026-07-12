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
) -> (StatusCode, Json<VmRecord>) {
    let vm = VmRecord {
        id: Uuid::new_v4(),
        name: req.name,
        template: req.template,
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

    (StatusCode::CREATED, Json(vm))
}

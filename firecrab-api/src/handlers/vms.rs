use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use uuid::Uuid;

use crate::error::AppError;
use crate::model::{CreateVmRequest, VmRecord, VmState};
use crate::persistence;
use crate::state::AppState;

pub async fn list_vms(State(state): State<AppState>) -> Result<Json<Vec<VmRecord>>, AppError> {
    let vms = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .values()
        .cloned()
        .collect::<Vec<_>>();

    Ok(Json(vms))
}

pub async fn create_vm(
    State(state): State<AppState>,
    payload: Result<Json<CreateVmRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<VmRecord>), AppError> {
    let Json(req) = payload.map_err(AppError::InvalidJson)?;
    let template = state
        .templates
        .resolve_alias(&req.template)
        .ok_or(AppError::InvalidTemplate)?;
    state.templates.open_verified(&template.kernel)?;
    state.templates.open_verified(&template.rootfs)?;
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

    let _writer = state.persistence_writer.lock().await;
    let mut snapshot = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    snapshot.insert(vm.id, vm.clone());
    persistence::save(&state.data_file, &snapshot).await?;
    state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(vm.id, vm.clone());

    Ok((StatusCode::CREATED, Json(vm)))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::*;
    use crate::templates::{TemplateRegistry, TemplateSpec};

    #[tokio::test]
    async fn list_vms_returns_all_known_vms() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("kernel"), b"kernel").unwrap();
        fs::write(directory.path().join("rootfs"), b"rootfs").unwrap();
        let templates = TemplateRegistry::from_specs(
            directory.path(),
            [TemplateSpec {
                alias: "ubuntu-rootfs-26.04".to_owned(),
                version: "v1".to_owned(),
                kernel: PathBuf::from("kernel"),
                rootfs: PathBuf::from("rootfs"),
                boot_args: "console=ttyS0".to_owned(),
            }],
        )
        .unwrap();
        let data_file = directory.path().join("data/vms.json");
        let state = AppState::with_data_file(templates, data_file)
            .await
            .unwrap();

        let vm = VmRecord {
            id: Uuid::new_v4(),
            name: "test-vm".to_owned(),
            state: VmState::Created,
            template: "ubuntu-rootfs-26.04".to_owned(),
            template_version: "v1".to_owned(),
            template_kernel_sha256: "kernel".to_owned(),
            template_rootfs_sha256: "rootfs".to_owned(),
            template_boot_args_sha256: "args".to_owned(),
            ram: 512,
            cpu: 1.0,
        };
        state.vms.lock().unwrap().insert(vm.id, vm.clone());

        let response = list_vms(State(state.clone())).await.unwrap();
        let body = response.0;

        assert_eq!(body.len(), 1);
        assert_eq!(body[0].id, vm.id);
    }

    #[tokio::test]
    async fn persistence_failure_does_not_publish_vm_in_memory() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("kernel"), b"kernel").unwrap();
        fs::write(directory.path().join("rootfs"), b"rootfs").unwrap();
        let templates = TemplateRegistry::from_specs(
            directory.path(),
            [TemplateSpec {
                alias: "ubuntu-rootfs-26.04".to_owned(),
                version: "v1".to_owned(),
                kernel: PathBuf::from("kernel"),
                rootfs: PathBuf::from("rootfs"),
                boot_args: "console=ttyS0".to_owned(),
            }],
        )
        .unwrap();
        let data_file = directory.path().join("data/vms.json");
        let state = AppState::with_data_file(templates, data_file)
            .await
            .unwrap();
        fs::write(directory.path().join("data"), b"not a directory").unwrap();

        let result = create_vm(
            State(state.clone()),
            Ok(Json(CreateVmRequest {
                name: "test-vm".to_owned(),
                template: "ubuntu-rootfs-26.04".to_owned(),
                ram: 512,
                cpu: 1.0,
            })),
        )
        .await;

        assert!(matches!(result, Err(AppError::Persistence(_))));
        assert!(
            state
                .vms
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_empty()
        );
    }
}

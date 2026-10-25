//! Named persistent volumes, attached exclusively while their VM is inactive.

use crate::block_storage;
use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::handlers::vms::parse_id;
use crate::persistence::PersistenceError;
use crate::server::RequestId;
use crate::state::AppState;
use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use firecrab_api_types::{
    AttachMicroBlockStorageRequest, CreateMicroBlockStorageRequest, MicroBlockStorageResponse,
    VmBlockStorageMount,
};
use std::collections::BTreeMap;
use uuid::Uuid;

pub(crate) fn persistence_error(error: PersistenceError, request: Uuid) -> AppError {
    match error {
        PersistenceError::MissingBlockStorage { .. } => AppError::not_found(request),
        PersistenceError::BlockStorageConflict { message } => {
            AppError::conflict("volume_conflict", message, request)
        }
        other => {
            tracing::error!(%other, request_id = %request, "volume persistence failed");
            AppError::internal(request)
        }
    }
}

pub async fn list(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
) -> Result<Json<Vec<MicroBlockStorageResponse>>, AppError> {
    state
        .store
        .block_storages()
        .map(Json)
        .map_err(|error| persistence_error(error, request.0))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<MicroBlockStorageResponse>, AppError> {
    let id = parse_id(&id, request.0)?;
    state
        .store
        .block_storage(id)
        .map(Json)
        .map_err(|error| persistence_error(error, request.0))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    ValidatedJson(req): ValidatedJson<CreateMicroBlockStorageRequest>,
) -> Result<(StatusCode, Json<MicroBlockStorageResponse>), AppError> {
    let mut fields = BTreeMap::new();
    if req.name.is_empty()
        || req.name.len() > 64
        || !req
            .name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        || !req
            .name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        fields.insert(
            "name".into(),
            "must be 1-64 ASCII letters, numbers, '.', '_' or '-'".into(),
        );
    }
    if req.size_gib == 0 {
        fields.insert("sizeGib".into(), "must be at least 1 GiB".into());
    }
    let storage_root = req
        .storage_root
        .unwrap_or_else(|| state.storage.default_id().to_owned());
    if let Err(error) = state.ensure_storage_capacity(&storage_root, u64::from(req.size_gib) << 30)
    {
        fields.insert("storageRoot".into(), error.to_string());
    }
    if !fields.is_empty() {
        return Err(AppError::validation(fields, request.0));
    }
    let registered_pool = if state.storage.get(&storage_root).is_some() {
        None
    } else {
        Uuid::parse_str(&storage_root).ok()
    };
    let volume = MicroBlockStorageResponse {
        id: Uuid::new_v4(),
        name: req.name,
        size_gib: req.size_gib,
        storage_root,
        state: "creating".into(),
        attached_vm_id: None,
        mount_path: None,
    };
    // A blocking task finishes file/DB work even if the HTTP deadline expires.
    let result = tokio::task::spawn_blocking(move || {
        state
            .store
            .insert_block_storage(&volume, registered_pool)
            .map_err(|error| persistence_error(error, request.0))?;
        if let Err(error) = block_storage::create_disk(&state, &volume) {
            tracing::error!(volume_id = %volume.id, %error, "volume format failed");
            if state.store.abort_block_storage_create(volume.id).is_ok()
                && block_storage::remove_disk(&state, &volume).is_ok()
            {
                let _ = state.store.finish_block_storage_delete(volume.id);
            }
            return Err(AppError::conflict(
                "volume_create_failed",
                "could not create the volume filesystem; check host storage and mkfs.ext4",
                request.0,
            ));
        }
        state
            .store
            .finish_block_storage_create(volume.id)
            .map_err(|error| persistence_error(error, request.0))?;
        state
            .store
            .block_storage(volume.id)
            .map_err(|error| persistence_error(error, request.0))
    })
    .await
    .map_err(|_| AppError::internal(request.0))??;
    Ok((StatusCode::CREATED, Json(result)))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let id = parse_id(&id, request.0)?;
    tokio::task::spawn_blocking(move || {
        let volume = state
            .store
            .block_storage(id)
            .map_err(|error| persistence_error(error, request.0))?;
        state
            .store
            .begin_block_storage_delete(id)
            .map_err(|error| persistence_error(error, request.0))?;
        block_storage::remove_disk(&state, &volume).map_err(|error| {
            tracing::error!(volume_id = %id, %error, "volume deletion needs a retry");
            AppError::conflict(
                "volume_delete_failed",
                "volume cleanup failed; retry deletion after checking storage",
                request.0,
            )
        })?;
        state
            .store
            .finish_block_storage_delete(id)
            .map_err(|error| persistence_error(error, request.0))
    })
    .await
    .map_err(|_| AppError::internal(request.0))??;
    Ok(StatusCode::NO_CONTENT)
}

fn require_inactive_user_vm(state: &AppState, id: Uuid, request: Uuid) -> Result<(), AppError> {
    if state
        .store
        .cluster_worker(id)
        .map_err(|_| AppError::internal(request))?
        .is_some()
    {
        return Err(AppError::conflict(
            "controller_owned_vm",
            "use the owning controller placement API for this VM",
            request,
        ));
    }
    let vm = state
        .vms
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&id)
        .cloned()
        .ok_or_else(|| AppError::not_found(request))?;
    if vm.purpose != crate::model::VmPurpose::Instance
        || state
            .store
            .vm_has_service_owner(id)
            .map_err(|error| persistence_error(error, request))?
    {
        return Err(AppError::conflict(
            "volume_owner_unsupported",
            "pool members, builders, and MicroService replicas cannot own data volumes",
            request,
        ));
    }
    if crate::network_policy::vm_is_active(state, &vm).map_err(|_| AppError::internal(request))? {
        return Err(AppError::conflict(
            "volume_vm_active",
            "stop the VM before changing its data volumes",
            request,
        ));
    }
    Ok(())
}

pub async fn attach(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(req): ValidatedJson<AttachMicroBlockStorageRequest>,
) -> Result<Json<MicroBlockStorageResponse>, AppError> {
    let id = parse_id(&id, request.0)?;
    if !block_storage::valid_mount_path(&req.mount_path) {
        return Err(AppError::validation(
            BTreeMap::from([(
                "mountPath".into(),
                "must be a safe absolute data directory without spaces or dot components".into(),
            )]),
            request.0,
        ));
    }
    let _guard = state.lock_vm(req.vm_id).await;
    require_inactive_user_vm(&state, req.vm_id, request.0)?;
    let volume = state
        .store
        .block_storage(id)
        .map_err(|error| persistence_error(error, request.0))?;
    block_storage::verify_disk(&state, &volume).map_err(|error| {
        tracing::error!(%error, volume_id = %id, "volume disk is unavailable");
        AppError::conflict(
            "volume_disk_unavailable",
            "volume disk is missing or invalid; repair storage before attaching",
            request.0,
        )
    })?;
    state
        .store
        .attach_block_storages(
            req.vm_id,
            &[VmBlockStorageMount {
                volume_id: id,
                mount_path: req.mount_path,
            }],
        )
        .map_err(|error| persistence_error(error, request.0))?;
    state
        .store
        .block_storage(id)
        .map(Json)
        .map_err(|error| persistence_error(error, request.0))
}

pub async fn detach(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<MicroBlockStorageResponse>, AppError> {
    let id = parse_id(&id, request.0)?;
    let volume = state
        .store
        .block_storage(id)
        .map_err(|error| persistence_error(error, request.0))?;
    let Some(vm_id) = volume.attached_vm_id else {
        return Ok(Json(volume));
    };
    let _guard = state.lock_vm(vm_id).await;
    require_inactive_user_vm(&state, vm_id, request.0)?;
    state
        .store
        .detach_block_storage(id, vm_id)
        .map_err(|error| persistence_error(error, request.0))?;
    state
        .store
        .block_storage(id)
        .map(Json)
        .map_err(|error| persistence_error(error, request.0))
}

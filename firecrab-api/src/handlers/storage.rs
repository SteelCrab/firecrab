//! `GET /api/storage` — selectable roots; `GET /api/storage/devices` — mounts.

use axum::Json;
use axum::extract::State;
use firecrab_api_types::{StorageDeviceResponse, StorageRootResponse};
use std::path::PathBuf;

use crate::state::AppState;
use crate::storage::{self, StorageRegistry};

/// `GET /api/storage`: env/default roots plus every MicroStorage, with live
/// free-space numbers for the create form and assign UI.
pub async fn list_storage(State(state): State<AppState>) -> Json<Vec<StorageRootResponse>> {
    let storage = state.storage.clone();
    let store = state.store.clone();
    let roots = tokio::task::spawn_blocking(move || {
        let mut roots = storage.list_responses();
        if let Ok(rows) = store.list_micro_storages() {
            for (id, name, path) in rows {
                let (total_gib, available_gib) =
                    StorageRegistry::space_for(PathBuf::from(&path).as_path());
                roots.push(StorageRootResponse {
                    id: id.to_string(),
                    name,
                    path,
                    available_gib,
                    total_gib,
                    kind: "micro_storage".to_owned(),
                });
            }
        }
        roots
    })
    .await
    .unwrap_or_default();
    Json(roots)
}

/// `GET /api/storage/devices`: mounted partitions/filesystems the operator
/// can pick when creating a MicroStorage. Discovery only — never formats.
pub async fn list_storage_devices() -> Json<Vec<StorageDeviceResponse>> {
    let devices = tokio::task::spawn_blocking(storage::list_mounted_devices)
        .await
        .unwrap_or_default();
    Json(devices)
}

#[cfg(test)]
mod tests {
    use firecrab_api_types::CreateMicroStorageRequest;
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::*;
    use crate::extract::ValidatedJson;
    use crate::handlers::micro_storages::create_micro_storage;
    use crate::handlers::vms::test_support::test_state;
    use crate::server::RequestId;
    use crate::storage::StorageRegistry;
    use axum::extract::Extension;

    #[tokio::test]
    async fn list_storage_returns_the_default_root_when_env_is_unset() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let Json(roots) = list_storage(State(state)).await;
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].id, "default");
        assert_eq!(roots[0].kind, "default");
        assert!(!roots[0].path.is_empty());
    }

    #[tokio::test]
    async fn list_storage_reflects_registered_roots() {
        let directory = tempdir().unwrap();
        let a = directory.path().join("a");
        let b = directory.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let state = test_state(directory.path()).await.with_test_storage(
            StorageRegistry::parse(&format!("disk-a={}:disk-b={}", a.display(), b.display()))
                .unwrap(),
        );

        let Json(roots) = list_storage(State(state)).await;
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].id, "disk-a");
        assert_eq!(roots[1].id, "disk-b");
        assert!(roots[0].available_gib > 0 || roots[0].total_gib > 0);
    }

    /// Discovery only: `/proc/mounts` always has at least the root
    /// filesystem, and the handler must never fail even where it does not.
    #[tokio::test]
    async fn list_storage_devices_reads_real_mounts() {
        let Json(devices) = list_storage_devices().await;
        assert!(
            devices.iter().all(|d| !d.mountpoint.is_empty()),
            "{devices:?}"
        );
    }

    #[tokio::test]
    async fn list_storage_includes_micro_storages() {
        let directory = tempdir().unwrap();
        let pool = directory.path().join("pool");
        let state = test_state(directory.path()).await;
        let _ = create_micro_storage(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(CreateMicroStorageRequest {
                name: "pool".to_owned(),
                path: pool.display().to_string(),
            }),
        )
        .await
        .unwrap();

        let Json(roots) = list_storage(State(state)).await;
        assert!(
            roots
                .iter()
                .any(|r| r.kind == "micro_storage" && r.name == "pool"),
            "{roots:?}"
        );
    }
}

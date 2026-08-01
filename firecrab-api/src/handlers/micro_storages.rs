//! MicroStorage CRUD — named host paths VMs can put disks on
//! (`docs/30-tasks/task-vm-physical-disk-selection.md`).

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use firecrab_api_types::{
    CreateMicroStorageRequest, MicroStorageDetailResponse, MicroStorageResponse, MicroStorageVm,
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::handlers::vms::parse_id;
use crate::persistence::PersistenceError;
use crate::server::RequestId;
use crate::state::AppState;
use crate::storage::{self, StorageRegistry};

pub async fn list_micro_storages(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
) -> Result<Json<Vec<MicroStorageResponse>>, AppError> {
    let store = state.store.clone();
    let rows = tokio::task::spawn_blocking(move || store.list_micro_storages())
        .await
        .map_err(|_| AppError::internal(request_id.0))?
        .map_err(|error| {
            tracing::error!(request_id = %request_id.0, %error, "failed to list micro storages");
            AppError::internal(request_id.0)
        })?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, name, path)| {
                let (total_gib, available_gib) =
                    StorageRegistry::space_for(PathBuf::from(&path).as_path());
                MicroStorageResponse {
                    id,
                    name,
                    path,
                    available_gib,
                    total_gib,
                }
            })
            .collect(),
    ))
}

pub async fn get_micro_storage(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<MicroStorageDetailResponse>, AppError> {
    let id = parse_id(&id, request_id.0)?;
    let store = state.store.clone();
    let (row, vms) = tokio::task::spawn_blocking(move || {
        let row = store.micro_storage(id)?;
        let vms = store.load_all()?;
        Ok::<_, PersistenceError>((row, vms))
    })
    .await
    .map_err(|_| AppError::internal(request_id.0))?
    .map_err(|error| {
        tracing::error!(request_id = %request_id.0, %error, "failed to load micro storage");
        AppError::internal(request_id.0)
    })?;

    let (id, name, path) = row.ok_or_else(|| AppError::not_found(request_id.0))?;
    let id_str = id.to_string();
    let mut members: Vec<MicroStorageVm> = vms
        .into_values()
        .filter(|vm| vm.storage_root == id_str)
        .map(|vm| MicroStorageVm {
            id: vm.id,
            name: vm.name,
            state: vm.state,
            disk_gb: vm.disk_gb,
        })
        .collect();
    members.sort_by(|a, b| a.name.cmp(&b.name));
    let (total_gib, available_gib) = StorageRegistry::space_for(PathBuf::from(&path).as_path());

    Ok(Json(MicroStorageDetailResponse {
        id,
        name,
        path,
        available_gib,
        total_gib,
        vms: members,
    }))
}

pub async fn create_micro_storage(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    ValidatedJson(req): ValidatedJson<CreateMicroStorageRequest>,
) -> Result<(StatusCode, Json<MicroStorageResponse>), AppError> {
    let mut fields = BTreeMap::new();
    if !valid_name(&req.name) {
        fields.insert(
            "name".to_owned(),
            "must be 1-64 ASCII letters, numbers, '.', '_' or '-'".to_owned(),
        );
    }
    let path = match storage::validate_storage_path(&req.path) {
        Ok(path) => path,
        Err(reason) => {
            fields.insert("path".to_owned(), reason);
            PathBuf::new()
        }
    };
    if !fields.is_empty() {
        return Err(AppError::validation(fields, request_id.0));
    }

    // Create the directory if missing so a fresh mount point can be registered
    // before the first VM lands on it.
    if let Err(error) = fs::create_dir_all(&path) {
        let mut fields = BTreeMap::new();
        fields.insert(
            "path".to_owned(),
            format!("could not create directory: {error}"),
        );
        return Err(AppError::validation(fields, request_id.0));
    }
    if !path.is_dir() {
        let mut fields = BTreeMap::new();
        fields.insert("path".to_owned(), "must be a directory".to_owned());
        return Err(AppError::validation(fields, request_id.0));
    }

    let id = Uuid::new_v4();
    let path_text = path.display().to_string();
    let store = state.store.clone();
    let name = req.name.clone();
    let path_for_db = path_text.clone();
    tokio::task::spawn_blocking(move || store.insert_micro_storage(id, &name, &path_for_db))
        .await
        .map_err(|_| AppError::internal(request_id.0))?
        .map_err(|error| match error {
            PersistenceError::DuplicateMicroStoragePath { path } => {
                let mut fields = BTreeMap::new();
                fields.insert("path".to_owned(), format!("{path} is already registered"));
                AppError::validation(fields, request_id.0)
            }
            other => {
                tracing::error!(request_id = %request_id.0, %other, "failed to persist micro storage");
                AppError::internal(request_id.0)
            }
        })?;

    let (total_gib, available_gib) = StorageRegistry::space_for(&path);
    tracing::info!(
        request_id = %request_id.0,
        micro_storage_id = %id,
        name = %req.name,
        path = %path_text,
        "micro storage created"
    );
    Ok((
        StatusCode::CREATED,
        Json(MicroStorageResponse {
            id,
            name: req.name,
            path: path_text,
            available_gib,
            total_gib,
        }),
    ))
}

pub async fn delete_micro_storage(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let id = parse_id(&id, request_id.0)?;
    let id_str = id.to_string();
    let store = state.store.clone();
    let in_use = tokio::task::spawn_blocking({
        let store = store.clone();
        let id_str = id_str.clone();
        move || store.count_vms_with_storage_root(&id_str)
    })
    .await
    .map_err(|_| AppError::internal(request_id.0))?
    .map_err(|error| {
        tracing::error!(request_id = %request_id.0, %error, "failed to count vms on micro storage");
        AppError::internal(request_id.0)
    })?;

    if in_use > 0 {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            "storage_in_use",
            "MicroStorage still has VMs assigned; reassign or delete them first",
            request_id.0,
        ));
    }

    tokio::task::spawn_blocking(move || store.delete_micro_storage(id))
        .await
        .map_err(|_| AppError::internal(request_id.0))?
        .map_err(|error| match error {
            PersistenceError::MissingMicroStorage { .. } => AppError::not_found(request_id.0),
            other => {
                tracing::error!(request_id = %request_id.0, %other, "failed to delete micro storage");
                AppError::internal(request_id.0)
            }
        })?;

    tracing::info!(request_id = %request_id.0, micro_storage_id = %id, "micro storage deleted");
    Ok(StatusCode::NO_CONTENT)
}

fn valid_name(name: &str) -> bool {
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
    use crate::handlers::vms::test_support::{record, seed_vm, test_state};
    use crate::server::RequestId;
    use axum::body::to_bytes;
    use axum::response::IntoResponse;
    use tempfile::tempdir;

    /// `AppError` keeps its `fields` private, so validation messages are read
    /// back through the rendered response body like `error.rs`'s own tests do.
    async fn error_body(error: AppError) -> serde_json::Value {
        let response = error.into_response();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let mut json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        json["status"] = serde_json::json!(status.as_u16());
        json
    }

    async fn create_pool(state: &AppState, name: &str, path: &std::path::Path) -> Uuid {
        let (_, Json(created)) = create_micro_storage(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(CreateMicroStorageRequest {
                name: name.to_owned(),
                path: path.display().to_string(),
            }),
        )
        .await
        .expect("create");
        created.id
    }

    #[tokio::test]
    async fn create_list_delete_micro_storage_round_trips() {
        let directory = tempdir().unwrap();
        let pool = directory.path().join("pool-a");
        let state = test_state(directory.path()).await;

        let (_, Json(created)) = create_micro_storage(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(CreateMicroStorageRequest {
                name: "pool-a".to_owned(),
                path: pool.display().to_string(),
            }),
        )
        .await
        .expect("create");
        assert_eq!(created.name, "pool-a");
        assert!(pool.is_dir());

        let Json(list) =
            list_micro_storages(State(state.clone()), Extension(RequestId(Uuid::new_v4())))
                .await
                .unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, created.id);

        let status = delete_micro_storage(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(created.id.to_string()),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn create_rejects_relative_path() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let err = create_micro_storage(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(CreateMicroStorageRequest {
                name: "bad".to_owned(),
                path: "relative/path".to_owned(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.into_response().status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_rejects_a_name_that_is_not_a_label() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let err = create_micro_storage(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(CreateMicroStorageRequest {
                name: "-nope!".to_owned(),
                path: directory.path().join("pool").display().to_string(),
            }),
        )
        .await
        .unwrap_err();
        let body = error_body(err).await;
        assert_eq!(body["status"], 400);
        assert!(
            body["error"]["fields"]["name"].is_string(),
            "name should be the rejected field: {body}"
        );
    }

    #[tokio::test]
    async fn create_reports_both_invalid_name_and_invalid_path() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let err = create_micro_storage(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(CreateMicroStorageRequest {
                name: String::new(),
                path: "  ".to_owned(),
            }),
        )
        .await
        .unwrap_err();
        let body = error_body(err).await;
        assert_eq!(body["status"], 400);
        assert!(body["error"]["fields"]["name"].is_string(), "{body}");
        assert!(body["error"]["fields"]["path"].is_string(), "{body}");
    }

    #[tokio::test]
    async fn create_rejects_a_path_that_is_an_existing_file() {
        let directory = tempdir().unwrap();
        let file = directory.path().join("not-a-dir");
        fs::write(&file, b"regular file").unwrap();
        let state = test_state(directory.path()).await;

        let err = create_micro_storage(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(CreateMicroStorageRequest {
                name: "pool".to_owned(),
                path: file.display().to_string(),
            }),
        )
        .await
        .unwrap_err();
        let body = error_body(err).await;
        assert_eq!(body["status"], 400);
        assert!(
            body["error"]["fields"]["path"]
                .as_str()
                .unwrap()
                .contains("could not create directory"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn create_rejects_a_path_that_is_already_registered() {
        let directory = tempdir().unwrap();
        let pool = directory.path().join("pool");
        let state = test_state(directory.path()).await;
        create_pool(&state, "pool-a", &pool).await;

        let err = create_micro_storage(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            ValidatedJson(CreateMicroStorageRequest {
                name: "pool-b".to_owned(),
                path: pool.display().to_string(),
            }),
        )
        .await
        .unwrap_err();
        let body = error_body(err).await;
        assert_eq!(body["status"], 400);
        assert!(
            body["error"]["fields"]["path"]
                .as_str()
                .unwrap()
                .contains("already registered"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn get_micro_storage_lists_only_its_own_vms_sorted_by_name() {
        let directory = tempdir().unwrap();
        let pool = directory.path().join("pool");
        let state = test_state(directory.path()).await;
        let id = create_pool(&state, "pool-a", &pool).await;

        let mut zeta = record("zeta", Uuid::new_v4());
        zeta.storage_root = id.to_string();
        zeta.disk_gb = 7;
        let mut alpha = record("alpha", Uuid::new_v4());
        alpha.storage_root = id.to_string();
        let elsewhere = record("elsewhere", Uuid::new_v4());
        seed_vm(&state, &zeta);
        seed_vm(&state, &alpha);
        seed_vm(&state, &elsewhere);

        let Json(detail) = get_micro_storage(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(id.to_string()),
        )
        .await
        .expect("detail");

        assert_eq!(detail.id, id);
        assert_eq!(detail.name, "pool-a");
        assert_eq!(detail.path, pool.display().to_string());
        let names: Vec<_> = detail.vms.iter().map(|vm| vm.name.as_str()).collect();
        assert_eq!(names, ["alpha", "zeta"]);
        assert_eq!(detail.vms[1].id, zeta.id);
        assert_eq!(detail.vms[1].disk_gb, 7);
    }

    #[tokio::test]
    async fn get_micro_storage_rejects_a_malformed_id() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let err = get_micro_storage(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path("not-a-uuid".to_owned()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.into_response().status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_micro_storage_returns_not_found_for_an_unknown_id() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let err = get_micro_storage(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(Uuid::new_v4().to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_refuses_while_a_vm_still_sits_on_the_storage() {
        let directory = tempdir().unwrap();
        let pool = directory.path().join("pool");
        let state = test_state(directory.path()).await;
        let id = create_pool(&state, "pool-a", &pool).await;

        let mut vm = record("resident", Uuid::new_v4());
        vm.storage_root = id.to_string();
        seed_vm(&state, &vm);

        let err = delete_micro_storage(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(id.to_string()),
        )
        .await
        .unwrap_err();
        let body = error_body(err).await;
        assert_eq!(body["status"], 409);
        assert_eq!(body["error"]["code"], "storage_in_use");

        // Still registered after the refused delete.
        let Json(list) = list_micro_storages(State(state), Extension(RequestId(Uuid::new_v4())))
            .await
            .unwrap();
        assert_eq!(list.len(), 1);
    }

    #[tokio::test]
    async fn delete_returns_not_found_for_an_unknown_id() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let err = delete_micro_storage(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path(Uuid::new_v4().to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_rejects_a_malformed_id() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let err = delete_micro_storage(
            State(state),
            Extension(RequestId(Uuid::new_v4())),
            Path("not-a-uuid".to_owned()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.into_response().status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn valid_name_accepts_labels_and_rejects_the_rest() {
        assert!(valid_name("a"));
        assert!(valid_name("pool-1.nvme_0"));
        assert!(!valid_name(""));
        assert!(!valid_name("-leading-dash"));
        assert!(!valid_name("has space"));
        assert!(!valid_name(&"a".repeat(65)));
    }
}

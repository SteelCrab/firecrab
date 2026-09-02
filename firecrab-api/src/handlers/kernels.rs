//! Kernel catalog, acquisition, and lifecycle endpoints.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use firecrab_api_types::KernelResponse;

use crate::error::AppError;
use crate::image_install::Architecture;
use crate::kernel_manager;
use crate::oci::kernel as kernel_cache;
use crate::server::RequestId;
use crate::state::AppState;

/// `GET /api/kernels` — host-architecture kernel catalog plus local cache
/// state.
pub async fn list_kernels(State(state): State<AppState>) -> Json<Vec<KernelResponse>> {
    Json(
        kernel_manager::list(
            state.templates.image_root_path(),
            state.templates.as_ref(),
            &state.kernel_installs,
        )
        .await,
    )
}

/// `GET /api/kernels/{version}/install` — latest acquisition snapshot.
pub async fn get_kernel_install(
    State(state): State<AppState>,
    Path(version): Path<String>,
    axum::Extension(request_id): axum::Extension<RequestId>,
) -> Result<Json<firecrab_api_types::KernelInstallResponse>, AppError> {
    if kernel_manager::cache_path(&version).is_none() {
        return Err(AppError::not_found(request_id.0));
    }
    Ok(Json(kernel_manager::snapshot(
        &state.kernel_installs,
        &version,
    )))
}

/// `POST /api/kernels/{version}/install` — download and verify one catalog
/// kernel into the shared cache.
pub async fn start_kernel_install(
    State(state): State<AppState>,
    Path(version): Path<String>,
    axum::Extension(request_id): axum::Extension<RequestId>,
) -> Result<impl IntoResponse, AppError> {
    if kernel_manager::cache_path(&version).is_none() {
        return Err(AppError::not_found(request_id.0));
    }
    if state.kernel_installs.is_running(&version) {
        return Err(AppError::conflict(
            "kernel_install_in_progress",
            "a kernel install is already running for this version",
            request_id.0,
        ));
    }

    let base_url = state.kernel_installs.base_url().map(str::to_owned);
    if base_url.is_none()
        && !kernel_cache::cached_kernel_is_valid(
            state.templates.image_root_path(),
            Architecture::HOST,
            &version,
        )
        .await
    {
        return Err(AppError::unavailable(
            "FIRECRAB_IMAGE_BASE_URL is not set; cannot download kernels",
            request_id.0,
        ));
    }

    let _started = match state
        .kernel_installs
        .begin_with(&version, "kernel install started")
    {
        Ok(snapshot) => snapshot,
        Err("running") => {
            return Err(AppError::conflict(
                "kernel_install_in_progress",
                "a kernel install is already running for this version",
                request_id.0,
            ));
        }
        Err(_) => return Err(AppError::internal(request_id.0)),
    };

    let tracker = state.kernel_installs.clone();
    let templates = (*state.templates).clone();
    let version_for_task = version.clone();
    tokio::spawn(async move {
        kernel_manager::run_install(tracker, templates, version_for_task, base_url).await;
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(kernel_manager::snapshot(&state.kernel_installs, &version)),
    ))
}

/// `DELETE /api/kernels/{version}` — remove an unused local kernel cache.
pub async fn delete_kernel(
    State(state): State<AppState>,
    Path(version): Path<String>,
    axum::Extension(request_id): axum::Extension<RequestId>,
) -> Result<StatusCode, AppError> {
    let Some(relative) = kernel_manager::cache_path(&version) else {
        return Err(AppError::not_found(request_id.0));
    };
    if state.kernel_installs.is_running(&version) {
        return Err(AppError::conflict(
            "kernel_install_in_progress",
            "cannot delete a kernel while its install is running",
            request_id.0,
        ));
    }

    let users: Vec<String> = state
        .templates
        .list_aliases()
        .into_iter()
        .filter(|template| template.kernel.relative_path() == relative)
        .map(|template| template.name.clone())
        .collect();
    if !users.is_empty() {
        let mut fields = BTreeMap::new();
        fields.insert("images".to_owned(), users.join(", "));
        fields.insert("count".to_owned(), users.len().to_string());
        return Err(AppError::in_use_with_fields(
            "kernel is still referenced by one or more images; update those images first",
            fields,
            request_id.0,
        ));
    }

    let path = state.templates.image_root_path().join(relative);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(AppError::conflict(
                "not_installed",
                "kernel is not installed on this host",
                request_id.0,
            ));
        }
        Err(error) => {
            tracing::error!(
                request_id = %request_id.0,
                version,
                path = %path.display(),
                %error,
                "failed to remove kernel cache"
            );
            return Err(AppError::internal(request_id.0));
        }
    }
    state.kernel_installs.clear(&version);
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_install::ImageInstallTracker;
    use crate::state::AppState;
    use crate::templates::{TemplateRegistry, TemplateSpec};
    use axum::Extension;
    use axum::extract::State;
    use std::fs;
    use std::path::{Path as FsPath, PathBuf};
    use tempfile::tempdir;

    async fn empty_state(root: &FsPath) -> AppState {
        let templates = TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
        AppState::with_db_file(templates, root.join("state.db"))
            .await
            .unwrap()
    }

    fn write_file(path: &FsPath, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn request_id() -> Extension<RequestId> {
        Extension(RequestId(uuid::Uuid::nil()))
    }

    #[tokio::test]
    async fn list_kernels_returns_the_host_catalog() {
        let directory = tempdir().unwrap();
        let mut state = empty_state(directory.path()).await;
        state.kernel_installs = ImageInstallTracker::disabled();

        let Json(kernels) = list_kernels(State(state)).await;

        assert_eq!(kernels.len(), 4);
        assert_eq!(kernels[0].version, "7.2.2");
        assert!(kernels.iter().all(|kernel| {
            kernel.architecture == Architecture::HOST.as_str()
                && !kernel.installed
                && !kernel.in_use
                && kernel.package_url.is_none()
        }));
    }

    #[tokio::test]
    async fn get_kernel_install_returns_idle_for_a_known_version() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;

        let Json(snapshot) =
            get_kernel_install(State(state), Path("7.2.2".to_owned()), request_id())
                .await
                .unwrap();

        assert_eq!(snapshot.version, "7.2.2");
        assert_eq!(
            snapshot.status,
            firecrab_api_types::ImageInstallStatus::Idle
        );
    }

    #[tokio::test]
    async fn kernel_install_endpoints_reject_unknown_versions() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;

        let get_result =
            get_kernel_install(State(state.clone()), Path("9.9.9".to_owned()), request_id()).await;
        assert_eq!(
            get_result.err().unwrap().into_response().status(),
            StatusCode::NOT_FOUND
        );

        let start_result =
            start_kernel_install(State(state.clone()), Path("9.9.9".to_owned()), request_id())
                .await;
        assert_eq!(
            start_result.err().unwrap().into_response().status(),
            StatusCode::NOT_FOUND
        );

        let delete_result =
            delete_kernel(State(state), Path("9.9.9".to_owned()), request_id()).await;
        assert_eq!(
            delete_result.err().unwrap().into_response().status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn start_kernel_install_requires_a_remote_or_cached_kernel() {
        let directory = tempdir().unwrap();
        let mut state = empty_state(directory.path()).await;
        state.kernel_installs = ImageInstallTracker::disabled();

        let result =
            start_kernel_install(State(state), Path("7.2.2".to_owned()), request_id()).await;

        assert_eq!(
            result.err().unwrap().into_response().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn start_kernel_install_rejects_a_second_running_job() {
        let directory = tempdir().unwrap();
        let mut state = empty_state(directory.path()).await;
        state.kernel_installs = ImageInstallTracker::disabled();
        state.kernel_installs.begin("7.2.2").unwrap();

        let result =
            start_kernel_install(State(state), Path("7.2.2".to_owned()), request_id()).await;

        assert_eq!(
            result.err().unwrap().into_response().status(),
            StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn delete_kernel_reports_missing_cache() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;

        let result = delete_kernel(State(state), Path("7.2.2".to_owned()), request_id()).await;

        assert_eq!(
            result.err().unwrap().into_response().status(),
            StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn delete_kernel_removes_an_unused_cache() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let state = empty_state(root).await;
        let relative = kernel_manager::cache_path("7.2.2").unwrap();
        let path = root.join(&relative);
        write_file(&path, b"cached kernel");

        let status = delete_kernel(State(state.clone()), Path("7.2.2".to_owned()), request_id())
            .await
            .unwrap();

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(!path.exists());
        assert_eq!(
            state.kernel_installs.snapshot("7.2.2").status,
            firecrab_api_types::ImageInstallStatus::Idle
        );
    }

    #[tokio::test]
    async fn delete_kernel_refuses_an_image_reference() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let relative = kernel_manager::cache_path("7.2.2").unwrap();
        write_file(&root.join(&relative), b"cached kernel");
        write_file(&root.join("rootfs/root.ext4"), b"rootfs");
        let templates = TemplateRegistry::from_specs(
            root,
            [TemplateSpec {
                alias: "demo".to_owned(),
                version: "demo-v1".to_owned(),
                kernel: relative.clone(),
                initrd: None,
                rootfs: PathBuf::from("rootfs/root.ext4"),
                boot_args: "console=ttyS0".to_owned(),
            }],
        )
        .unwrap();
        let state = AppState::with_db_file(templates, root.join("state.db"))
            .await
            .unwrap();

        let result = delete_kernel(State(state), Path("7.2.2".to_owned()), request_id()).await;
        let response = result.err().unwrap().into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "in_use");
        assert_eq!(json["error"]["fields"]["images"], "demo");
    }
}

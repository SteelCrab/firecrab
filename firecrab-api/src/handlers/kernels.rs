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
pub async fn list_kernels(
    State(state): State<AppState>,
    axum::Extension(request_id): axum::Extension<RequestId>,
) -> Result<Json<Vec<KernelResponse>>, AppError> {
    kernel_manager::list(
        state.templates.image_root_path(),
        state.templates.as_ref(),
        &state.kernel_installs,
    )
    .await
    .map(Json)
    .map_err(|error| {
        tracing::warn!(%error, "cannot query the kernel registry");
        AppError::unavailable(
            "cannot query the kernel registry; check host network access",
            request_id.0,
        )
    })
}

/// `GET /api/kernels/{version}/install` — latest acquisition snapshot.
pub async fn get_kernel_install(
    State(state): State<AppState>,
    Path(version): Path<String>,
    axum::Extension(request_id): axum::Extension<RequestId>,
) -> Result<Json<firecrab_api_types::KernelInstallResponse>, AppError> {
    if version != "stable" && kernel_manager::cache_path(&version).is_none() {
        return Err(AppError::not_found(request_id.0));
    }
    Ok(Json(kernel_manager::snapshot(
        &state.kernel_installs,
        &version,
    )))
}

/// `POST /api/kernels/{version}/install` — download and verify a registry package
/// into the shared cache. `stable` resolves the newest active published kernel.
pub async fn start_kernel_install(
    State(state): State<AppState>,
    Path(version): Path<String>,
    axum::Extension(request_id): axum::Extension<RequestId>,
) -> Result<impl IntoResponse, AppError> {
    if version != "stable" && kernel_manager::cache_path(&version).is_none() {
        return Err(AppError::not_found(request_id.0));
    }
    let operation = state.kernel_installs.operation_lock(&version);
    let _operation = operation.lock().await;
    if state.kernel_installs.is_running(&version) {
        return Ok((
            StatusCode::ACCEPTED,
            Json(kernel_manager::snapshot(&state.kernel_installs, &version)),
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
            "FIRECRAB_KERNEL_RELEASES_URL is disabled; cannot build kernels",
            request_id.0,
        ));
    }

    let _started = match state
        .kernel_installs
        .begin_with(&version, "kernel package installation started")
    {
        Ok(snapshot) => snapshot,
        Err("running") => {
            return Ok((
                StatusCode::ACCEPTED,
                Json(kernel_manager::snapshot(&state.kernel_installs, &version)),
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

/// `DELETE /api/kernels/{version}/install` — request cancellation, then poll for cleanup.
pub async fn cancel_kernel_install(
    State(state): State<AppState>,
    Path(version): Path<String>,
    axum::Extension(request_id): axum::Extension<RequestId>,
) -> Result<impl IntoResponse, AppError> {
    if version != "stable" && kernel_manager::cache_path(&version).is_none() {
        return Err(AppError::not_found(request_id.0));
    }
    let operation = state.kernel_installs.operation_lock(&version);
    let _operation = operation.lock().await;
    state.kernel_installs.request_cancel(&version);
    let status = if state.kernel_installs.is_running(&version) {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok((
        status,
        Json(kernel_manager::snapshot(&state.kernel_installs, &version)),
    ))
}

fn ensure_unused(
    state: &AppState,
    relative: &std::path::Path,
    request_id: uuid::Uuid,
) -> Result<(), AppError> {
    let users: Vec<String> = state
        .templates
        .list_aliases()
        .into_iter()
        .filter(|template| template.kernel.relative_path() == relative)
        .map(|template| template.name.clone())
        .collect();
    if users.is_empty() {
        return Ok(());
    }
    let mut fields = BTreeMap::new();
    fields.insert("images".to_owned(), users.join(", "));
    fields.insert("count".to_owned(), users.len().to_string());
    Err(AppError::in_use_with_fields(
        "kernel is still referenced by one or more images; update those images first",
        fields,
        request_id,
    ))
}

/// `DELETE /api/kernels/{version}` — stop installation and remove unused local files.
pub async fn delete_kernel(
    State(state): State<AppState>,
    Path(version): Path<String>,
    axum::Extension(request_id): axum::Extension<RequestId>,
) -> Result<StatusCode, AppError> {
    let Some(relative) = kernel_manager::cache_path(&version) else {
        return Err(AppError::not_found(request_id.0));
    };
    let operation = state.kernel_installs.operation_lock(&version);
    let _operation = operation.lock().await;
    ensure_unused(&state, &relative, request_id.0)?;
    state.kernel_installs.cancel_and_wait(&version).await;
    let lock = kernel_cache::cache_lock(
        state.templates.image_root_path(),
        Architecture::HOST,
        &version,
    )
    .await
    .map_err(|_| AppError::internal(request_id.0))?;
    let _guard = lock.try_lock().map_err(|_| {
        AppError::conflict(
            "kernel_install_in_progress",
            "this kernel is being prepared",
            request_id.0,
        )
    })?;

    ensure_unused(&state, &relative, request_id.0)?;
    let path = state.templates.image_root_path().join(relative);
    let mut removed = false;
    for file in [
        &path,
        &path.with_added_extension("json"),
        &path.with_added_extension("config"),
    ] {
        match tokio::fs::remove_file(file).await {
            Ok(()) => removed = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::error!(
                    request_id = %request_id.0,
                    version,
                    path = %file.display(),
                    %error,
                    "failed to remove kernel cache"
                );
                return Err(AppError::internal(request_id.0));
            }
        }
    }
    removed |= kernel_cache::remove_source_and_builds(
        state.templates.image_root_path(),
        Architecture::HOST,
        &version,
    )
    .await
    .map_err(|error| {
        tracing::error!(%error, version, "failed to remove kernel source cache");
        AppError::internal(request_id.0)
    })?;
    if !removed
        && state.kernel_installs.snapshot(&version).status
            == firecrab_api_types::ImageInstallStatus::Idle
    {
        return Err(AppError::conflict(
            "not_installed",
            "kernel has no local installation or files",
            request_id.0,
        ));
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

        let Json(kernels) = list_kernels(State(state), request_id()).await.unwrap();

        assert!(kernels.is_empty());
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

        let get_result = get_kernel_install(
            State(state.clone()),
            Path("9.9.9-rc1".to_owned()),
            request_id(),
        )
        .await;
        assert_eq!(
            get_result.err().unwrap().into_response().status(),
            StatusCode::NOT_FOUND
        );

        let start_result = start_kernel_install(
            State(state.clone()),
            Path("9.9.9-rc1".to_owned()),
            request_id(),
        )
        .await;
        assert_eq!(
            start_result.err().unwrap().into_response().status(),
            StatusCode::NOT_FOUND
        );

        let cancel_result = cancel_kernel_install(
            State(state.clone()),
            Path("9.9.9-rc1".to_owned()),
            request_id(),
        )
        .await;
        assert_eq!(
            cancel_result.err().unwrap().into_response().status(),
            StatusCode::NOT_FOUND
        );
        let delete_result =
            delete_kernel(State(state), Path("9.9.9-rc1".to_owned()), request_id()).await;
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
    async fn start_kernel_install_reconnects_to_the_same_running_job() {
        let directory = tempdir().unwrap();
        let mut state = empty_state(directory.path()).await;
        state.kernel_installs = ImageInstallTracker::disabled();
        let started = state.kernel_installs.begin("7.2.2").unwrap();
        state
            .kernel_installs
            .set_download_progress("7.2.2", 1234, Some(5678));

        let result =
            start_kernel_install(State(state.clone()), Path("7.2.2".to_owned()), request_id())
                .await;

        let response = result.unwrap().into_response();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let snapshot: firecrab_api_types::KernelInstallResponse =
            serde_json::from_slice(&body).unwrap();
        assert_eq!(snapshot.started_at_ms, started.started_at_ms);
        assert_eq!(snapshot.log, started.log);
        assert_eq!(snapshot.downloaded_bytes, Some(1234));
        assert_eq!(snapshot.total_bytes, Some(5678));
        assert!(state.kernel_installs.is_running("7.2.2"));
    }

    #[tokio::test]
    async fn cancellation_before_worker_start_is_retained_and_retry_gets_a_new_signal() {
        let directory = tempdir().unwrap();
        let mut state = empty_state(directory.path()).await;
        state.kernel_installs = ImageInstallTracker::disabled();
        state.kernel_installs.begin("7.2.15").unwrap();
        for _ in 0..2 {
            let response =
                cancel_kernel_install(State(state.clone()), Path("7.2.15".into()), request_id())
                    .await
                    .unwrap()
                    .into_response();
            assert_eq!(response.status(), StatusCode::ACCEPTED);
        }
        let pending = kernel_manager::snapshot(&state.kernel_installs, "7.2.15");
        assert_eq!(pending.cancelling, Some(true));
        assert_eq!(pending.log.matches("cancellation requested").count(), 1);
        assert!(state.kernel_installs.begin("7.2.15").is_err());
        kernel_manager::run_install(
            state.kernel_installs.clone(),
            (*state.templates).clone(),
            "7.2.15".into(),
            None,
        )
        .await;
        let finished = kernel_manager::snapshot(&state.kernel_installs, "7.2.15");
        assert_eq!(
            finished.status,
            firecrab_api_types::ImageInstallStatus::Cancelled
        );
        assert_eq!(finished.cancelling, Some(false));
        assert!(finished.ended_at_ms.is_some());
        let response =
            cancel_kernel_install(State(state.clone()), Path("7.2.15".into()), request_id())
                .await
                .unwrap()
                .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        state.kernel_installs.begin("7.2.15").unwrap();
        assert!(!state.kernel_installs.is_cancelling("7.2.15"));
    }

    #[tokio::test]
    async fn deletion_waits_for_worker_cleanup_and_preserves_other_kernel_files() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let mut state = empty_state(root).await;
        state.kernel_installs = ImageInstallTracker::disabled();
        state.kernel_installs.begin("7.2.15").unwrap();
        let source = root.join(".oci/kernel/sources/linux-7.2.15.tar.xz");
        let other = root.join(".oci/kernel/sources/linux-7.3.tar.xz");
        let manual = root.join(".oci/kernel/sources/linux-7.2.15.tar.manual-partial");
        let partial = root.join(format!(
            ".oci/kernel/sources/linux-7.2.15.tar.{}-partial",
            uuid::Uuid::new_v4()
        ));
        let work = root.join(format!(
            ".oci/kernel/{}/.build-7.2.15-{}",
            Architecture::HOST,
            uuid::Uuid::new_v4()
        ));
        for path in [&source, &other, &manual, &partial, &work.join("object.o")] {
            write_file(path, b"data");
        }
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        let worker_release = release.clone();
        let worker_tracker = state.kernel_installs.clone();
        let mut cancellation = worker_tracker.cancellation("7.2.15").unwrap();
        let worker = tokio::spawn(async move {
            while !*cancellation.borrow_and_update() {
                cancellation.changed().await.unwrap();
            }
            worker_release.notified().await;
            worker_tracker.finish_cancelled("7.2.15");
        });
        let delete_state = state.clone();
        let deletion = tokio::spawn(async move {
            delete_kernel(State(delete_state), Path("7.2.15".into()), request_id()).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !state.kernel_installs.is_cancelling("7.2.15") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!deletion.is_finished());
        assert!(source.exists());
        let start_state = state.clone();
        let restart = tokio::spawn(async move {
            start_kernel_install(State(start_state), Path("7.2.15".into()), request_id())
                .await
                .map(|response| response.into_response().status())
        });
        tokio::task::yield_now().await;
        assert!(
            !restart.is_finished(),
            "a new start must wait until deletion finishes"
        );
        release.notify_one();
        worker.await.unwrap();
        assert_eq!(deletion.await.unwrap().unwrap(), StatusCode::NO_CONTENT);
        assert!(
            restart.await.unwrap().is_err(),
            "disabled sources cannot start a replacement after deletion"
        );
        assert!(!source.exists());
        assert!(!partial.exists());
        assert!(!work.exists());
        assert!(other.exists());
        assert!(manual.exists());
        assert_eq!(
            state.kernel_installs.snapshot("7.2.15").status,
            firecrab_api_types::ImageInstallStatus::Idle
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
        write_file(&path.with_added_extension("json"), b"receipt");
        write_file(&path.with_added_extension("config"), b"config");

        let status = delete_kernel(State(state.clone()), Path("7.2.2".to_owned()), request_id())
            .await
            .unwrap();

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(!path.exists());
        assert!(!path.with_added_extension("json").exists());
        assert!(!path.with_added_extension("config").exists());
        assert_eq!(
            state.kernel_installs.snapshot("7.2.2").status,
            firecrab_api_types::ImageInstallStatus::Idle
        );
    }

    #[tokio::test]
    async fn delete_kernel_refuses_an_oci_or_stable_alias_build() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        let lock = kernel_cache::cache_lock(directory.path(), Architecture::HOST, "7.2.12")
            .await
            .unwrap();
        let _building = lock.lock().await;
        let result = delete_kernel(State(state), Path("7.2.12".to_owned()), request_id()).await;
        assert_eq!(
            result.err().unwrap().into_response().status(),
            StatusCode::CONFLICT
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
        state.kernel_installs.begin("7.2.2").unwrap();
        let result =
            delete_kernel(State(state.clone()), Path("7.2.2".to_owned()), request_id()).await;
        let response = result.err().unwrap().into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "in_use");
        assert_eq!(json["error"]["fields"]["images"], "demo");
        assert!(state.kernel_installs.is_running("7.2.2"));
        assert!(!state.kernel_installs.is_cancelling("7.2.2"));
    }
}

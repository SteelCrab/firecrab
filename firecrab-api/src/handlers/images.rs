//! `GET /api/images` catalog, install, and `DELETE /api/images/{alias}`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use firecrab_api_types::{ImageInstallResponse, ImageInstallStatus, ImageResponse};

use crate::error::AppError;
use crate::image_install;
use crate::server::RequestId;
use crate::state::AppState;
use crate::templates::{TemplateRegistry, TemplateVersion};

/// Smallest disk (GiB) that can hold `rootfs_bytes`, matching create validation.
fn min_disk_gb_for(rootfs_bytes: u64) -> u16 {
    const GIB: u64 = 1024 * 1024 * 1024;
    rootfs_bytes.div_ceil(GIB).try_into().unwrap_or(u16::MAX)
}

fn installed_response(template: &TemplateVersion) -> ImageResponse {
    let rootfs_size_bytes = template.rootfs.length();
    ImageResponse {
        alias: template.name.clone(),
        version: template.version.clone(),
        kernel_sha256: template.kernel.sha256().to_owned(),
        rootfs_sha256: template.rootfs.sha256().to_owned(),
        initrd_sha256: template
            .initrd
            .as_ref()
            .map(|artifact| artifact.sha256().to_owned()),
        min_disk_gb: min_disk_gb_for(rootfs_size_bytes),
        rootfs_size_bytes,
        installed: true,
        description: String::new(),
    }
}

fn not_installed_response(alias: &str, version: &str) -> ImageResponse {
    ImageResponse {
        alias: alias.to_owned(),
        version: version.to_owned(),
        kernel_sha256: String::new(),
        rootfs_sha256: String::new(),
        initrd_sha256: None,
        min_disk_gb: 0,
        rootfs_size_bytes: 0,
        installed: false,
        description: String::new(),
    }
}

/// `GET /api/images`: known templates (installed + not-yet-installed).
/// Host paths are never included.
pub async fn list_images(State(state): State<AppState>) -> Json<Vec<ImageResponse>> {
    let templates = state.templates.clone();
    let images = tokio::task::spawn_blocking(move || {
        let mut images: Vec<ImageResponse> = TemplateRegistry::known_specs()
            .into_iter()
            .map(|spec| match templates.resolve_alias(&spec.alias) {
                Some(template) => installed_response(template.as_ref()),
                None => not_installed_response(&spec.alias, &spec.version),
            })
            .collect();
        // Any extra registered aliases not in the built-in set (future registration API).
        for template in templates.list_aliases() {
            if !images.iter().any(|image| image.alias == template.name) {
                images.push(installed_response(template.as_ref()));
            }
        }
        images.sort_by(|left, right| left.alias.cmp(&right.alias));
        images
    })
    .await
    .unwrap_or_default();
    Json(images)
}

/// `GET /api/images/{alias}/install` — latest install job snapshot.
pub async fn get_image_install(
    State(state): State<AppState>,
    Path(alias): Path<String>,
    Extension(request_id): Extension<RequestId>,
) -> Result<Json<ImageInstallResponse>, AppError> {
    if TemplateRegistry::known_spec(&alias).is_none()
        && state.templates.resolve_alias(&alias).is_none()
    {
        return Err(AppError::not_found(request_id.0));
    }
    Ok(Json(state.image_installs.snapshot(&alias)))
}

/// `POST /api/images/{alias}/install` — start (or reject) an async install.
pub async fn start_image_install(
    State(state): State<AppState>,
    Path(alias): Path<String>,
    Extension(request_id): Extension<RequestId>,
) -> Result<impl IntoResponse, AppError> {
    let Some(spec) = TemplateRegistry::known_spec(&alias) else {
        return Err(AppError::not_found(request_id.0));
    };

    if state.templates.resolve_alias(&alias).is_some() {
        return Err(AppError::conflict(
            "already_installed",
            "template is already installed on this host",
            request_id.0,
        ));
    }

    let Some(base_url) = state.image_installs.base_url().map(str::to_owned) else {
        return Err(AppError::unavailable(
            "FIRECRAB_IMAGE_BASE_URL is not set; cannot download template images",
            request_id.0,
        ));
    };

    let response = match state.image_installs.begin(&alias) {
        Ok(snapshot) => snapshot,
        Err("running") => {
            return Err(AppError::conflict(
                "install_in_progress",
                "an install is already running for this template",
                request_id.0,
            ));
        }
        Err(_) => return Err(AppError::internal(request_id.0)),
    };

    let tracker = state.image_installs.clone();
    let templates = (*state.templates).clone();
    tokio::spawn(async move {
        image_install::run_install(tracker, templates, base_url, spec).await;
    });

    Ok((StatusCode::ACCEPTED, Json(response)))
}

/// `DELETE /api/images/{alias}` — unregister the template and remove its
/// orphan artifact files. Refuses when VMs still use the alias or an install
/// is running.
pub async fn delete_image(
    State(state): State<AppState>,
    Path(alias): Path<String>,
    Extension(request_id): Extension<RequestId>,
) -> Result<StatusCode, AppError> {
    if TemplateRegistry::known_spec(&alias).is_none()
        && state.templates.resolve_alias(&alias).is_none()
    {
        return Err(AppError::not_found(request_id.0));
    }

    if state.templates.resolve_alias(&alias).is_none() {
        return Err(AppError::conflict(
            "not_installed",
            "template is not installed on this host",
            request_id.0,
        ));
    }

    if state.image_installs.is_running(&alias) {
        return Err(AppError::conflict(
            "install_in_progress",
            "cannot delete while an install is running for this template",
            request_id.0,
        ));
    }

    {
        let vms = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let users: Vec<String> = vms
            .values()
            .filter(|vm| vm.template == alias)
            .map(|vm| {
                format!(
                    "{} [{}]",
                    vm.name,
                    crate::persistence::encode_state(vm.state)
                )
            })
            .collect();
        if !users.is_empty() {
            let mut fields = std::collections::BTreeMap::new();
            fields.insert("vms".to_owned(), users.join(", "));
            fields.insert("count".to_owned(), users.len().to_string());
            return Err(AppError::in_use_with_fields(
                "template is still used by one or more VMs; delete those VMs first",
                fields,
                request_id.0,
            ));
        }
    }

    let templates = state.templates.clone();
    let alias_for_task = alias.clone();
    let removed = tokio::task::spawn_blocking(move || templates.unregister_alias(&alias_for_task))
        .await
        .map_err(|_| AppError::internal(request_id.0))?;

    let Some((_version, orphan_paths)) = removed else {
        return Err(AppError::conflict(
            "not_installed",
            "template is not installed on this host",
            request_id.0,
        ));
    };

    for path in orphan_paths {
        if let Err(error) = tokio::fs::remove_file(&path).await {
            // Not-found is fine (already gone); other errors still mean the
            // registry entry is gone so the image is unusable for create.
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    alias = %alias,
                    path = %path.display(),
                    error = %error,
                    "failed to remove template artifact file"
                );
            }
        }
    }

    state.image_installs.clear(&alias);
    Ok(StatusCode::NO_CONTENT)
}

use axum::Extension;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_install::ImageInstallTracker;
    use crate::state::AppState;
    use crate::templates::{TemplateRegistry, TemplateSpec};
    use axum::extract::State;
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;
    use tokio::net::TcpListener;

    fn write_file(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }

    async fn empty_state(root: &Path) -> AppState {
        let templates = TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
        AppState::with_db_file(templates, root.join("state.db"))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn list_images_includes_not_installed_known_aliases() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        let Json(images) = list_images(State(state)).await;
        assert!(
            images
                .iter()
                .any(|image| image.alias == "ubuntu-26.04" && !image.installed)
        );
        assert!(
            images
                .iter()
                .any(|image| image.alias == "alpine-3.24" && !image.installed)
        );
    }

    #[tokio::test]
    async fn list_images_marks_present_templates_installed() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        write_file(&root.join("kernel/vmlinux"), b"kernel-bytes");
        write_file(&root.join("rootfs/root.ext4"), b"rootfs-content-here");
        let templates = TemplateRegistry::from_specs(
            root,
            [TemplateSpec {
                alias: "demo".to_owned(),
                version: "demo-v1".to_owned(),
                kernel: Path::new("kernel/vmlinux").to_path_buf(),
                initrd: None,
                rootfs: Path::new("rootfs/root.ext4").to_path_buf(),
                boot_args: "console=ttyS0".to_owned(),
            }],
        )
        .unwrap();
        let state = AppState::with_db_file(templates, root.join("state.db"))
            .await
            .unwrap();
        let Json(images) = list_images(State(state)).await;
        let demo = images.iter().find(|image| image.alias == "demo").unwrap();
        assert!(demo.installed);
        assert_eq!(demo.min_disk_gb, 1);
        assert_eq!(demo.rootfs_size_bytes, b"rootfs-content-here".len() as u64);
        assert_eq!(demo.kernel_sha256.len(), 64);
    }

    #[tokio::test]
    async fn install_refuses_without_base_url() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        let result = start_image_install(
            State(state),
            Path("alpine-3.24".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await;
        let err = result.err().expect("should fail");
        // IntoResponse path: check via status on a rebuilt error is awkward;
        // match by re-calling logic — assert message through unavailable code.
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn install_downloads_registers_and_marks_installed() {
        let source = tempdir().unwrap();
        let dest = tempdir().unwrap();
        let alpine = TemplateRegistry::known_spec("alpine-3.24").unwrap();
        write_file(&source.path().join(&alpine.kernel), b"fake-alpine-kernel");
        write_file(
            &source.path().join(alpine.initrd.as_ref().unwrap()),
            b"fake-alpine-initrd",
        );
        write_file(&source.path().join(&alpine.rootfs), b"fake-alpine-rootfs");

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().fallback_service(tower_http::services::ServeDir::new(
            source.path().to_path_buf(),
        ));
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let templates = TemplateRegistry::from_specs(dest.path(), std::iter::empty()).unwrap();
        let base = format!("http://{addr}");
        let mut state = AppState::with_db_file(templates, dest.path().join("state.db"))
            .await
            .unwrap();
        state.image_installs = ImageInstallTracker::with_base_url(base);

        let accepted = start_image_install(
            State(state.clone()),
            Path("alpine-3.24".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await
        .expect("accepted");
        let response = accepted.into_response();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let mut ok = false;
        for _ in 0..80 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let Json(snap) = get_image_install(
                State(state.clone()),
                Path("alpine-3.24".to_owned()),
                Extension(RequestId(uuid::Uuid::nil())),
            )
            .await
            .unwrap();
            if snap.status == ImageInstallStatus::Succeeded {
                ok = true;
                assert!(snap.log.contains("succeeded"));
                break;
            }
            if snap.status == ImageInstallStatus::Failed {
                panic!("install failed:\n{}", snap.log);
            }
        }
        assert!(ok, "install did not succeed in time");
        assert!(state.templates.resolve_alias("alpine-3.24").is_some());

        let Json(images) = list_images(State(state)).await;
        let alpine_row = images
            .iter()
            .find(|image| image.alias == "alpine-3.24")
            .unwrap();
        assert!(alpine_row.installed);
    }

    #[tokio::test]
    async fn delete_image_unregisters_and_removes_files() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        write_file(&root.join("kernel/vmlinux"), b"kernel-bytes");
        write_file(&root.join("rootfs/root.ext4"), b"rootfs-content-here");
        let templates = TemplateRegistry::from_specs(
            root,
            [TemplateSpec {
                alias: "demo".to_owned(),
                version: "demo-v1".to_owned(),
                kernel: Path::new("kernel/vmlinux").to_path_buf(),
                initrd: None,
                rootfs: Path::new("rootfs/root.ext4").to_path_buf(),
                boot_args: "console=ttyS0".to_owned(),
            }],
        )
        .unwrap();
        let state = AppState::with_db_file(templates, root.join("state.db"))
            .await
            .unwrap();
        assert!(state.templates.resolve_alias("demo").is_some());

        let status = delete_image(
            State(state.clone()),
            Path("demo".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await
        .expect("delete ok");
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(state.templates.resolve_alias("demo").is_none());
        assert!(!root.join("kernel/vmlinux").exists());
        assert!(!root.join("rootfs/root.ext4").exists());

        let Json(images) = list_images(State(state)).await;
        // Built-in known aliases remain listed as missing; demo was not known.
        assert!(
            !images
                .iter()
                .any(|image| image.alias == "demo" && image.installed)
        );
    }

    #[tokio::test]
    async fn delete_image_refuses_when_not_installed() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        let result = delete_image(
            State(state),
            Path("alpine-3.24".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await;
        let err = result.err().expect("should fail");
        assert_eq!(err.into_response().status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn delete_image_unknown_alias_is_not_found() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        let result = delete_image(
            State(state),
            Path("no-such-image".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await;
        assert_eq!(
            result.err().unwrap().into_response().status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn get_image_install_idle_for_known_alias() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        let Json(snap) = get_image_install(
            State(state),
            Path("alpine-3.24".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await
        .unwrap();
        assert_eq!(snap.alias, "alpine-3.24");
        assert_eq!(snap.status, ImageInstallStatus::Idle);
    }

    #[tokio::test]
    async fn get_image_install_unknown_is_not_found() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        let result = get_image_install(
            State(state),
            Path("nope".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await;
        assert_eq!(
            result.err().unwrap().into_response().status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn start_image_install_unknown_is_not_found() {
        let directory = tempdir().unwrap();
        let mut state = empty_state(directory.path()).await;
        state.image_installs = ImageInstallTracker::with_base_url("http://127.0.0.1:9");
        let result = start_image_install(
            State(state),
            Path("nope".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await;
        assert_eq!(
            result.err().unwrap().into_response().status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn start_image_install_refuses_already_installed() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let alpine = TemplateRegistry::known_spec("alpine-3.24").unwrap();
        write_file(&root.join(&alpine.kernel), b"k");
        write_file(&root.join(alpine.initrd.as_ref().unwrap()), b"i");
        write_file(&root.join(&alpine.rootfs), b"r");
        let templates = TemplateRegistry::from_specs(root, [alpine]).unwrap();
        let mut state = AppState::with_db_file(templates, root.join("state.db"))
            .await
            .unwrap();
        state.image_installs = ImageInstallTracker::with_base_url("http://127.0.0.1:9");
        let result = start_image_install(
            State(state),
            Path("alpine-3.24".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await;
        let response = result.err().unwrap().into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn start_image_install_refuses_when_already_running() {
        let directory = tempdir().unwrap();
        let mut state = empty_state(directory.path()).await;
        state.image_installs = ImageInstallTracker::with_base_url("http://127.0.0.1:9");
        state.image_installs.begin("alpine-3.24").unwrap();
        let result = start_image_install(
            State(state),
            Path("alpine-3.24".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await;
        assert_eq!(
            result.err().unwrap().into_response().status(),
            StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn delete_image_refuses_while_install_running() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        write_file(&root.join("kernel/vmlinux"), b"k");
        write_file(&root.join("rootfs/root.ext4"), b"r");
        let templates = TemplateRegistry::from_specs(
            root,
            [TemplateSpec {
                alias: "demo".to_owned(),
                version: "v1".to_owned(),
                kernel: Path::new("kernel/vmlinux").to_path_buf(),
                initrd: None,
                rootfs: Path::new("rootfs/root.ext4").to_path_buf(),
                boot_args: "console=ttyS0".to_owned(),
            }],
        )
        .unwrap();
        let mut state = AppState::with_db_file(templates, root.join("state.db"))
            .await
            .unwrap();
        // Force a running job even though demo is not a known_spec — is_running
        // only checks the tracker.
        state.image_installs = ImageInstallTracker::with_base_url("http://example");
        state.image_installs.begin("demo").unwrap();
        // known_spec is none for demo but resolve_alias is some → delete proceeds
        // until is_running check.
        let result = delete_image(
            State(state),
            Path("demo".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await;
        assert_eq!(
            result.err().unwrap().into_response().status(),
            StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn delete_image_refuses_when_vm_uses_template() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        write_file(&root.join("kernel/vmlinux"), b"k");
        write_file(&root.join("rootfs/root.ext4"), b"r");
        let templates = TemplateRegistry::from_specs(
            root,
            [TemplateSpec {
                alias: "demo".to_owned(),
                version: "v1".to_owned(),
                kernel: Path::new("kernel/vmlinux").to_path_buf(),
                initrd: None,
                rootfs: Path::new("rootfs/root.ext4").to_path_buf(),
                boot_args: "console=ttyS0".to_owned(),
            }],
        )
        .unwrap();
        let state = AppState::with_db_file(templates, root.join("state.db"))
            .await
            .unwrap();
        {
            let mut vms = state.vms.lock().unwrap();
            let mut vm =
                crate::handlers::vms::test_support::record("blocker", uuid::Uuid::new_v4());
            vm.template = "demo".to_owned();
            vm.state = crate::model::VmState::Stopped;
            vms.insert(vm.id, vm);
        }
        let result = delete_image(
            State(state),
            Path("demo".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await;
        let response = result.err().unwrap().into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "in_use");
        assert_eq!(json["error"]["fields"]["count"], "1");
        assert!(
            json["error"]["fields"]["vms"]
                .as_str()
                .unwrap()
                .contains("blocker")
        );
    }

    #[tokio::test]
    async fn install_fails_on_http_404() {
        let source = tempdir().unwrap();
        let dest = tempdir().unwrap();
        // Empty source dir → downloads 404.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().fallback_service(tower_http::services::ServeDir::new(
            source.path().to_path_buf(),
        ));
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let templates = TemplateRegistry::from_specs(dest.path(), std::iter::empty()).unwrap();
        let mut state = AppState::with_db_file(templates, dest.path().join("state.db"))
            .await
            .unwrap();
        state.image_installs = ImageInstallTracker::with_base_url(format!("http://{addr}"));

        let _ = start_image_install(
            State(state.clone()),
            Path("alpine-3.24".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await
        .expect("accepted");

        let mut failed = false;
        for _ in 0..80 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let Json(snap) = get_image_install(
                State(state.clone()),
                Path("alpine-3.24".to_owned()),
                Extension(RequestId(uuid::Uuid::nil())),
            )
            .await
            .unwrap();
            if snap.status == ImageInstallStatus::Failed {
                failed = true;
                assert!(snap.log.contains("failed") || snap.log.contains("HTTP"));
                break;
            }
        }
        assert!(failed, "install should fail when artifacts are missing");
        assert!(state.templates.resolve_alias("alpine-3.24").is_none());
    }

    #[tokio::test]
    async fn list_images_includes_extra_registered_aliases() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        write_file(&root.join("kernel/vmlinux"), b"k");
        write_file(&root.join("rootfs/root.ext4"), b"r");
        let templates = TemplateRegistry::from_specs(
            root,
            [TemplateSpec {
                alias: "custom-image".to_owned(),
                version: "v1".to_owned(),
                kernel: Path::new("kernel/vmlinux").to_path_buf(),
                initrd: None,
                rootfs: Path::new("rootfs/root.ext4").to_path_buf(),
                boot_args: "console=ttyS0".to_owned(),
            }],
        )
        .unwrap();
        let state = AppState::with_db_file(templates, root.join("state.db"))
            .await
            .unwrap();
        let Json(images) = list_images(State(state)).await;
        assert!(
            images
                .iter()
                .any(|image| image.alias == "custom-image" && image.installed)
        );
    }
}

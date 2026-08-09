//! `GET /api/images` catalog, package acquisition, image installation, and delete.

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

fn installed_response(
    template: &TemplateVersion,
    package_url: Option<String>,
    package_staged: bool,
) -> ImageResponse {
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
        package_url,
        package_staged,
        description: String::new(),
    }
}

fn not_installed_response(
    alias: &str,
    version: &str,
    package_url: Option<String>,
    package_staged: bool,
) -> ImageResponse {
    ImageResponse {
        alias: alias.to_owned(),
        version: version.to_owned(),
        kernel_sha256: String::new(),
        rootfs_sha256: String::new(),
        initrd_sha256: None,
        min_disk_gb: 0,
        rootfs_size_bytes: 0,
        installed: false,
        package_url,
        package_staged,
        description: String::new(),
    }
}

/// `GET /api/images`: known templates (installed + not-yet-installed).
/// Host paths are never included; each row may carry an official `packageUrl`
/// and/or a `packageStaged` flag for an archive already sitting in the local
/// cache — the latter is how a web-bootstrapped image becomes installable on
/// a host with no `FIRECRAB_IMAGE_BASE_URL` at all.
pub async fn list_images(State(state): State<AppState>) -> Json<Vec<ImageResponse>> {
    let templates = state.templates.clone();
    let package_base = state.image_packages.base_url().map(str::to_owned);
    let images = tokio::task::spawn_blocking(move || {
        let package_for = |alias: &str| -> Option<String> {
            package_base
                .as_deref()
                .map(|base| image_install::package_url(base, alias))
        };
        // One `is_file()` per alias, on the same blocking task that already
        // does this catalog's other per-alias filesystem work.
        let image_root = templates.image_root_path();
        let staged_for =
            |alias: &str| -> bool { image_install::staged_package_exists(image_root, alias) };
        let mut images: Vec<ImageResponse> = TemplateRegistry::known_specs()
            .into_iter()
            .map(|spec| {
                let url = package_for(&spec.alias);
                let staged = staged_for(&spec.alias);
                match templates.resolve_alias(&spec.alias) {
                    Some(template) => installed_response(template.as_ref(), url, staged),
                    None => not_installed_response(&spec.alias, &spec.version, url, staged),
                }
            })
            .collect();
        // Any extra registered aliases not in the built-in set (future
        // registration API) — except the internal MicroBoot builder source
        // (crate::microboot), which is registered into TemplateRegistry
        // purely so create_vm's existing machinery can provision it, and
        // must never appear as an installable image.
        for template in templates.list_aliases() {
            if template.name == crate::microboot::MICROBOOT_ALIAS {
                continue;
            }
            if !images.iter().any(|image| image.alias == template.name) {
                images.push(installed_response(
                    template.as_ref(),
                    package_for(&template.name),
                    staged_for(&template.name),
                ));
            }
        }
        images.sort_by(|left, right| left.alias.cmp(&right.alias));
        images
    })
    .await
    .unwrap_or_default();
    Json(images)
}

/// `GET /api/images/{alias}/package` — latest package acquisition snapshot.
///
/// A verified archive survives an API restart, so an otherwise-idle tracker
/// is reported as ready whenever the staged archive is still present.
pub async fn get_image_package(
    State(state): State<AppState>,
    Path(alias): Path<String>,
    Extension(request_id): Extension<RequestId>,
) -> Result<Json<ImageInstallResponse>, AppError> {
    if TemplateRegistry::known_spec(&alias).is_none()
        && state.templates.resolve_alias(&alias).is_none()
    {
        return Err(AppError::not_found(request_id.0));
    }

    let snapshot = state.image_packages.snapshot(&alias);
    if snapshot.status == ImageInstallStatus::Idle
        && image_install::staged_package_exists(state.templates.image_root_path(), &alias)
    {
        return Ok(Json(ImageInstallResponse {
            alias,
            status: ImageInstallStatus::Succeeded,
            log: "package ready — using the verified local archive".to_owned(),
            started_at_ms: None,
            ended_at_ms: None,
            downloaded_bytes: None,
            total_bytes: None,
        }));
    }
    Ok(Json(snapshot))
}

/// `POST /api/images/{alias}/package` — download and verify a package only.
/// It does not extract or register the image; that is the separate install
/// action below.
pub async fn start_image_package(
    State(state): State<AppState>,
    Path(alias): Path<String>,
    Extension(request_id): Extension<RequestId>,
) -> Result<impl IntoResponse, AppError> {
    let Some(spec) = TemplateRegistry::known_spec(&alias) else {
        return Err(AppError::not_found(request_id.0));
    };

    // Replacing a cache while it is being extracted would make the two
    // dashboard actions race.  A completed cache may be downloaded again so
    // an operator can repair it after a failed image install.
    if state.image_installs.is_running(&alias) {
        return Err(AppError::conflict(
            "install_in_progress",
            "cannot replace the package while this image is installing",
            request_id.0,
        ));
    }

    let Some(base_url) = state.image_packages.base_url().map(str::to_owned) else {
        return Err(AppError::unavailable(
            "FIRECRAB_IMAGE_BASE_URL is not set; cannot download template packages",
            request_id.0,
        ));
    };

    let response = match state
        .image_packages
        .begin_with(&alias, "package install started")
    {
        Ok(snapshot) => snapshot,
        Err("running") => {
            return Err(AppError::conflict(
                "package_in_progress",
                "a package download is already running for this template",
                request_id.0,
            ));
        }
        Err(_) => return Err(AppError::internal(request_id.0)),
    };

    let tracker = state.image_packages.clone();
    let templates = (*state.templates).clone();
    tokio::spawn(async move {
        image_install::run_package_install(tracker, templates, base_url, spec).await;
    });

    Ok((StatusCode::ACCEPTED, Json(response)))
}

/// `GET /api/images/{alias}/install` — latest image installation snapshot.
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

/// `POST /api/images/{alias}/install` — extract/register an image from its
/// already prepared local package.  It performs no network download.
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

    if state.image_packages.is_running(&alias) {
        return Err(AppError::conflict(
            "package_in_progress",
            "wait for the package download to finish before installing the image",
            request_id.0,
        ));
    }

    if !image_install::staged_package_exists(state.templates.image_root_path(), &alias) {
        return Err(AppError::conflict(
            "package_required",
            "package is not ready; run package install first",
            request_id.0,
        ));
    }

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
        image_install::run_image_install(tracker, templates, spec).await;
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
    // Same reason list_images omits it: `__microboot` is internal
    // (see crate::microboot). Deleting it would strip the registration and
    // its cached artifacts out from under an in-flight bootstrap.
    if alias == crate::microboot::MICROBOOT_ALIAS
        || (TemplateRegistry::known_spec(&alias).is_none()
            && state.templates.resolve_alias(&alias).is_none())
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
        // Builder VMs are deliberately excluded. They are transient build
        // artifacts, not user resources: `list_vms` hides them, so the
        // dashboard's recovery flow (which offers to delete the listed VMs
        // and retry) can neither show nor remove one — a bootstrap in flight
        // would block the delete with a conflict the operator can't act on.
        // `handlers::bootstrap` owns their lifecycle instead.
        let users: Vec<String> = vms
            .values()
            .filter(|vm| vm.template == alias && vm.purpose == crate::model::VmPurpose::Instance)
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

/// `DELETE /api/images/{alias}/package` — deletes a staged-but-not-installed
/// local package (`.packages/{alias}.tar.zst`). Independent of `delete_image`
/// (which only ever acts on an *installed* template): a staged package can be
/// deleted even for an alias that's still installed, since the staged archive
/// only feeds a future (re)install and isn't itself load-bearing.
pub async fn delete_staged_package(
    State(state): State<AppState>,
    Path(alias): Path<String>,
    Extension(request_id): Extension<RequestId>,
) -> Result<StatusCode, AppError> {
    // Same reason `delete_image` excludes it: `__microboot` is internal
    // (see crate::microboot). It never has anything under `.packages/`
    // anyway (this would just fall through to `not_staged`), but excluding
    // it explicitly keeps this handler's alias guard symmetric with
    // `delete_image`'s.
    if alias == crate::microboot::MICROBOOT_ALIAS
        || (TemplateRegistry::known_spec(&alias).is_none()
            && state.templates.resolve_alias(&alias).is_none())
    {
        return Err(AppError::not_found(request_id.0));
    }

    // A download/verify or an install-from-staged may be reading or writing
    // this exact file right now.
    if state.image_packages.is_running(&alias) || state.image_installs.is_running(&alias) {
        return Err(AppError::conflict(
            "package_in_progress",
            "cannot delete while a package download or install is running for this template",
            request_id.0,
        ));
    }

    let path = image_install::staged_package_path(state.templates.image_root_path(), &alias);
    if !path.is_file() {
        return Err(AppError::conflict(
            "not_staged",
            "no staged package exists for this alias",
            request_id.0,
        ));
    }

    if let Err(error) = tokio::fs::remove_file(&path).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(AppError::internal(request_id.0));
        }
    }

    state.image_packages.clear(&alias);
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
        let Json(images) = list_images(State(state.clone())).await;
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
        assert!(
            images
                .iter()
                .any(|image| image.alias == "rocky-9" && !image.installed)
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
    async fn package_install_refuses_when_remote_disabled() {
        let directory = tempdir().unwrap();
        let mut state = empty_state(directory.path()).await;
        state.image_packages = ImageInstallTracker::disabled();
        let result = start_image_package(
            State(state),
            Path("alpine-3.24".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await;
        let err = result.err().expect("should fail");
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn list_images_includes_package_url_when_base_set() {
        let directory = tempdir().unwrap();
        let mut state = empty_state(directory.path()).await;
        state.image_packages = ImageInstallTracker::with_base_url("http://127.0.0.1:8765");
        let Json(images) = list_images(State(state)).await;
        let ubuntu = images
            .iter()
            .find(|image| image.alias == "ubuntu-26.04")
            .unwrap();
        assert!(!ubuntu.installed);
        assert_eq!(
            ubuntu.package_url.as_deref(),
            Some("http://127.0.0.1:8765/ubuntu/26.04/ubuntu-26.04.tar.zst")
        );
    }

    /// The gap that made a completed web bootstrap unreachable from the UI:
    /// `packageUrl` is derived purely from `FIRECRAB_IMAGE_BASE_URL`, so on a
    /// host without one — the common dev case, and exactly the case web
    /// bootstrap exists for — a freshly staged archive showed up as
    /// "패키지 URL 없음" with no way to install it. `packageStaged` reports
    /// the local archive independently, which is what the dashboard's local
    /// install button keys off.
    #[tokio::test]
    async fn list_images_reports_a_staged_package_even_with_no_remote_base_url() {
        let directory = tempdir().unwrap();
        let mut state = empty_state(directory.path()).await;
        state.image_packages = ImageInstallTracker::disabled();

        // Nothing staged yet: no URL and no local archive.
        let Json(before) = list_images(State(state.clone())).await;
        let ubuntu = before
            .iter()
            .find(|image| image.alias == "ubuntu-26.04")
            .unwrap();
        assert!(ubuntu.package_url.is_none());
        assert!(!ubuntu.package_staged);

        // Stand in for what `handlers::bootstrap::package_bootstrap` writes.
        let staged = crate::image_install::staged_package_path(
            state.templates.image_root_path(),
            "ubuntu-26.04",
        );
        fs::create_dir_all(staged.parent().unwrap()).unwrap();
        fs::write(&staged, b"pretend tar.zst").unwrap();

        let Json(after) = list_images(State(state)).await;
        let ubuntu = after
            .iter()
            .find(|image| image.alias == "ubuntu-26.04")
            .unwrap();
        assert!(ubuntu.package_staged);
        assert!(!ubuntu.installed);
        assert!(
            ubuntu.package_url.is_none(),
            "a locally staged package must not fabricate a remote URL"
        );
    }

    fn make_tar_zst(source: &std::path::Path, members: &[&str], dest: &std::path::Path) {
        use std::process::{Command, Stdio};
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let tar = Command::new("tar")
            .args(["-C"])
            .arg(source)
            .arg("-cf")
            .arg("-")
            .args(members)
            .stdout(Stdio::piped())
            .spawn()
            .expect("tar");
        let status = Command::new("zstd")
            .args(["-q", "-f", "-o"])
            .arg(dest)
            .stdin(tar.stdout.unwrap())
            .status()
            .expect("zstd");
        assert!(status.success(), "zstd failed");
    }

    #[tokio::test]
    async fn package_then_image_install_registers_and_marks_installed() {
        let source = tempdir().unwrap();
        let dest = tempdir().unwrap();
        let alpine = TemplateRegistry::known_spec("alpine-3.24").unwrap();
        write_file(&source.path().join(&alpine.kernel), b"fake-alpine-kernel");
        write_file(
            &source.path().join(alpine.initrd.as_ref().unwrap()),
            b"fake-alpine-initrd",
        );
        write_file(&source.path().join(&alpine.rootfs), b"fake-alpine-rootfs");
        let members = [
            alpine.kernel.to_str().unwrap(),
            alpine.initrd.as_ref().unwrap().to_str().unwrap(),
            alpine.rootfs.to_str().unwrap(),
        ];
        let package = crate::image_install::package_path(&alpine.alias);
        let archive = source.path().join(package);
        fs::create_dir_all(archive.parent().unwrap()).unwrap();
        make_tar_zst(source.path(), &members, &archive);

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
        state.image_packages = ImageInstallTracker::with_base_url(base);

        let accepted = start_image_package(
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
            let Json(snap) = get_image_package(
                State(state.clone()),
                Path("alpine-3.24".to_owned()),
                Extension(RequestId(uuid::Uuid::nil())),
            )
            .await
            .unwrap();
            if snap.status == ImageInstallStatus::Succeeded {
                ok = true;
                assert!(snap.log.contains("package ready"));
                for stage in ["source", "rootfs", "kernel", "package"] {
                    assert!(snap.log.contains(&format!("[packer:{stage}] complete")));
                }
                break;
            }
            if snap.status == ImageInstallStatus::Failed {
                panic!("package install failed:\n{}", snap.log);
            }
        }
        assert!(ok, "package install did not succeed in time");
        assert!(crate::image_install::staged_package_exists(
            state.templates.image_root_path(),
            "alpine-3.24"
        ));
        assert!(
            state.templates.resolve_alias("alpine-3.24").is_none(),
            "package acquisition must not register an image"
        );

        let accepted = start_image_install(
            State(state.clone()),
            Path("alpine-3.24".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await
        .expect("image install accepted");
        assert_eq!(accepted.into_response().status(), StatusCode::ACCEPTED);

        let mut installed = false;
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
                installed = true;
                assert!(snap.log.contains("template registered"));
                break;
            }
            if snap.status == ImageInstallStatus::Failed {
                panic!("image install failed:\n{}", snap.log);
            }
        }
        assert!(installed, "image install did not succeed in time");
        assert!(state.templates.resolve_alias("alpine-3.24").is_some());

        let Json(images) = list_images(State(state.clone())).await;
        let alpine_row = images
            .iter()
            .find(|image| image.alias == "alpine-3.24")
            .unwrap();
        assert!(alpine_row.installed);

        let deleted = delete_image(
            State(state.clone()),
            Path("alpine-3.24".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await
        .expect("delete installed image");
        assert_eq!(deleted, StatusCode::NO_CONTENT);
        assert!(
            crate::image_install::staged_package_exists(
                state.templates.image_root_path(),
                "alpine-3.24"
            ),
            "image deletion must preserve the prepared package"
        );

        let accepted = start_image_install(
            State(state.clone()),
            Path("alpine-3.24".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await
        .expect("reinstall from cached package accepted");
        assert_eq!(accepted.into_response().status(), StatusCode::ACCEPTED);
        let mut reinstalled = false;
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
                reinstalled = true;
                break;
            }
            if snap.status == ImageInstallStatus::Failed {
                panic!("cached image reinstall failed:\n{}", snap.log);
            }
        }
        assert!(
            reinstalled,
            "cached image reinstall did not succeed in time"
        );
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
    async fn image_install_requires_a_prepared_package() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        let result = start_image_install(
            State(state),
            Path("alpine-3.24".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await;
        let response = result
            .err()
            .expect("package must be required")
            .into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "package_required");
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
        write_file(
            &crate::image_install::staged_package_path(
                state.templates.image_root_path(),
                "alpine-3.24",
            ),
            b"already-prepared",
        );
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
    async fn package_install_fails_on_http_404() {
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
        state.image_packages = ImageInstallTracker::with_base_url(format!("http://{addr}"));

        let _ = start_image_package(
            State(state.clone()),
            Path("alpine-3.24".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await
        .expect("accepted");

        let mut failed = false;
        for _ in 0..80 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let Json(snap) = get_image_package(
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
        assert!(
            failed,
            "package install should fail when artifacts are missing"
        );
        assert!(state.templates.resolve_alias("alpine-3.24").is_none());
        assert!(
            !crate::image_install::staged_package_exists(
                state.templates.image_root_path(),
                "alpine-3.24"
            ),
            "a failed download must not publish a ready package"
        );
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

    #[tokio::test]
    async fn list_images_never_surfaces_the_internal_microboot_alias() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        write_file(&root.join("kernel/vmlinux"), b"k");
        write_file(&root.join("rootfs/root.ext4"), b"r");
        let templates = TemplateRegistry::from_specs(
            root,
            [TemplateSpec {
                alias: crate::microboot::MICROBOOT_ALIAS.to_owned(),
                version: "v1".to_owned(),
                kernel: Path::new("kernel/vmlinux").to_path_buf(),
                initrd: None,
                rootfs: Path::new("rootfs/root.ext4").to_path_buf(),
                boot_args: "console=ttyS0 reboot=k".to_owned(),
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
                .all(|image| image.alias != crate::microboot::MICROBOOT_ALIAS),
            "microboot must never appear in /api/images: {images:?}"
        );
    }

    #[tokio::test]
    async fn delete_staged_package_removes_the_staged_archive() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        state.image_packages.begin("ubuntu-26.04").unwrap();
        state.image_packages.finish_ok("ubuntu-26.04");
        let staged = crate::image_install::staged_package_path(
            state.templates.image_root_path(),
            "ubuntu-26.04",
        );
        fs::create_dir_all(staged.parent().unwrap()).unwrap();
        fs::write(&staged, b"pretend tar.zst").unwrap();

        let status = delete_staged_package(
            State(state.clone()),
            Path("ubuntu-26.04".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await
        .unwrap();

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(!staged.is_file());
        assert_eq!(
            state.image_packages.snapshot("ubuntu-26.04").status,
            ImageInstallStatus::Idle
        );
    }

    #[tokio::test]
    async fn delete_staged_package_refuses_when_nothing_is_staged() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;

        let error = delete_staged_package(
            State(state),
            Path("ubuntu-26.04".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn delete_staged_package_refuses_while_a_package_download_is_running() {
        let directory = tempdir().unwrap();
        let mut state = empty_state(directory.path()).await;
        state.image_packages = ImageInstallTracker::with_base_url("http://example");
        state.image_packages.begin("ubuntu-26.04").unwrap();
        let staged = crate::image_install::staged_package_path(
            state.templates.image_root_path(),
            "ubuntu-26.04",
        );
        fs::create_dir_all(staged.parent().unwrap()).unwrap();
        fs::write(&staged, b"pretend tar.zst").unwrap();

        let error = delete_staged_package(
            State(state),
            Path("ubuntu-26.04".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn delete_staged_package_succeeds_even_when_the_alias_is_installed() {
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

        let staged =
            crate::image_install::staged_package_path(state.templates.image_root_path(), "demo");
        fs::create_dir_all(staged.parent().unwrap()).unwrap();
        fs::write(&staged, b"pretend tar.zst").unwrap();

        let status = delete_staged_package(
            State(state.clone()),
            Path("demo".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await
        .unwrap();

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(!staged.is_file());
        // Deleting the staged package must never touch the installed template.
        assert!(state.templates.resolve_alias("demo").is_some());
    }

    #[tokio::test]
    async fn delete_staged_package_unknown_alias_is_not_found() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;

        let error = delete_staged_package(
            State(state),
            Path("no-such-alias".to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::NOT_FOUND);
    }
}

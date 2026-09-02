use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json};
use firecrab_api_types::{ImageInstallResponse, ImageInstallStatus};
use tempfile::tempdir;

use super::images::{delete_image, get_image_install, start_image_install};
use crate::image_install::{self, ImageInstallTracker};
use crate::persistence::LocalCatalogEntry;
use crate::server::RequestId;
use crate::state::AppState;
use crate::templates::{TemplateRegistry, TemplateSpec};

const ALIAS: &str = "nginx-1.27";
const LOCAL_VERSION: &str = "1";

fn write_file(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

async fn custom_state(root: &Path) -> AppState {
    write_file(&root.join("kernel/vmlinux-nginx"), b"custom-kernel");
    write_file(&root.join("rootfs/nginx-1.27.ext4"), b"custom-rootfs");
    let templates = TemplateRegistry::from_specs(
        root,
        [TemplateSpec {
            alias: ALIAS.to_owned(),
            version: "oci-import".to_owned(),
            kernel: PathBuf::from("kernel/vmlinux-nginx"),
            initrd: None,
            rootfs: PathBuf::from("rootfs/nginx-1.27.ext4"),
            boot_args: "console=ttyS0 root=/dev/vda rw".to_owned(),
        }],
    )
    .unwrap();
    AppState::with_db_file(templates, root.join("state.db"))
        .await
        .unwrap()
}

async fn stage_local_catalog_package(state: &AppState) {
    let tracker = ImageInstallTracker::disabled();
    tracker.begin_with(ALIAS, "register started").unwrap();
    let packed = crate::package::pack_registered_template(
        &tracker,
        state.templates.as_ref(),
        ALIAS,
        LOCAL_VERSION,
    )
    .await
    .unwrap();
    state
        .store
        .insert_microregistry_local(&LocalCatalogEntry {
            alias: ALIAS.to_owned(),
            architecture: image_install::host_architecture().to_owned(),
            version: LOCAL_VERSION.to_owned(),
            package: packed.package,
            sha256: packed.sha256,
            min_disk_gb: 1,
            published_at: "2026-08-25T00:00:00Z".to_owned(),
        })
        .unwrap();
}

async fn remove_installed_source(state: &AppState) {
    let status = delete_image(
        State(state.clone()),
        AxumPath(ALIAS.to_owned()),
        Extension(RequestId(uuid::Uuid::nil())),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(state.templates.resolve_alias(ALIAS).is_none());
    assert!(image_install::staged_package_exists(
        state.templates.image_root_path(),
        ALIAS
    ));
}

async fn wait_for_install(state: &AppState) -> ImageInstallResponse {
    for _ in 0..80 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let Json(snapshot) = get_image_install(
            State(state.clone()),
            AxumPath(ALIAS.to_owned()),
            Extension(RequestId(uuid::Uuid::nil())),
        )
        .await
        .unwrap();
        if matches!(
            snapshot.status,
            ImageInstallStatus::Succeeded | ImageInstallStatus::Failed
        ) {
            return snapshot;
        }
    }
    panic!("local image install did not finish in time");
}

#[tokio::test]
async fn host_local_catalog_row_reinstalls_custom_alias() {
    let directory = tempdir().unwrap();
    let state = custom_state(directory.path()).await;
    stage_local_catalog_package(&state).await;
    remove_installed_source(&state).await;

    let Json(idle) = get_image_install(
        State(state.clone()),
        AxumPath(ALIAS.to_owned()),
        Extension(RequestId(uuid::Uuid::nil())),
    )
    .await
    .expect("host-local row must make the install status route addressable");
    assert_eq!(idle.status, ImageInstallStatus::Idle);

    let accepted = start_image_install(
        State(state.clone()),
        AxumPath(ALIAS.to_owned()),
        Extension(RequestId(uuid::Uuid::nil())),
    )
    .await
    .expect("host-local package install accepted");
    assert_eq!(accepted.into_response().status(), StatusCode::ACCEPTED);

    let finished = wait_for_install(&state).await;
    assert_eq!(
        finished.status,
        ImageInstallStatus::Succeeded,
        "{}",
        finished.log
    );
    assert!(
        finished
            .log
            .contains("local MicroRegistry package checksum verified")
    );
    let installed = state
        .templates
        .resolve_alias(ALIAS)
        .expect("custom alias must be registered from the local package");
    assert_eq!(installed.version, LOCAL_VERSION);
}

#[tokio::test]
async fn host_local_catalog_install_rejects_checksum_tampering() {
    let directory = tempdir().unwrap();
    let state = custom_state(directory.path()).await;
    stage_local_catalog_package(&state).await;
    remove_installed_source(&state).await;

    let staged = image_install::staged_package_path(state.templates.image_root_path(), ALIAS);
    fs::write(&staged, b"tampered archive bytes").unwrap();

    let accepted = start_image_install(
        State(state.clone()),
        AxumPath(ALIAS.to_owned()),
        Extension(RequestId(uuid::Uuid::nil())),
    )
    .await
    .expect("validation runs inside the asynchronous install job");
    assert_eq!(accepted.into_response().status(), StatusCode::ACCEPTED);

    let finished = wait_for_install(&state).await;
    assert_eq!(finished.status, ImageInstallStatus::Failed);
    assert!(
        finished.log.contains("checksum mismatch"),
        "{}",
        finished.log
    );
    assert!(state.templates.resolve_alias(ALIAS).is_none());
}

#[tokio::test]
async fn host_local_catalog_install_reports_missing_staged_package() {
    let directory = tempdir().unwrap();
    let state = custom_state(directory.path()).await;
    stage_local_catalog_package(&state).await;
    remove_installed_source(&state).await;

    let staged = image_install::staged_package_path(state.templates.image_root_path(), ALIAS);
    fs::remove_file(staged).unwrap();

    let error = start_image_install(
        State(state),
        AxumPath(ALIAS.to_owned()),
        Extension(RequestId(uuid::Uuid::nil())),
    )
    .await
    .err()
    .expect("missing local package must fail before starting a job");
    let response = error.into_response();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"]["code"], "package_required");
}

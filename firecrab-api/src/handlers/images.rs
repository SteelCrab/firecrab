//! Host-local MicroRegistry install compatibility layer.
//!
//! All ordinary image handlers are re-exported from `images_core`; only the
//! install status/start pair is extended so a locally registered MicroRegistry
//! package can be reinstalled even when its alias is not compiled into the
//! public M2Image catalog.

use std::fs::File;
use std::io::Read;
use std::path::{Path as FsPath, PathBuf};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use firecrab_api_types::ImageInstallResponse;
use sha2::{Digest, Sha256};

use crate::error::AppError;
use crate::image_install;
use crate::persistence::LocalCatalogEntry;
use crate::server::RequestId;
use crate::state::AppState;
use crate::templates::{TemplateRegistry, TemplateSpec};

pub use super::images_core::{
    delete_image, delete_staged_package, get_image_package, list_image_detail, list_images,
    start_image_package, update_image_kernel,
};

/// Read one host-local MicroRegistry row without blocking the async worker.
async fn host_local_catalog_entry(
    state: &AppState,
    alias: &str,
    request_id: RequestId,
) -> Result<Option<LocalCatalogEntry>, AppError> {
    let store = state.store.clone();
    let alias = alias.to_owned();
    let request_id = request_id.0;
    tokio::task::spawn_blocking(move || {
        store.microregistry_local(&alias, image_install::Architecture::HOST.as_str())
    })
    .await
    .map_err(|_| AppError::internal(request_id))?
    .map_err(|error| {
        tracing::error!(request_id = %request_id, %error, "failed to read local MicroRegistry row");
        AppError::internal(request_id)
    })
}

/// Stream a file through SHA-256 without loading the package into memory.
fn sha256_file(path: &FsPath) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Verify a local catalog row against the staged archive, then recover the
/// embedded template specification that the normal installer will validate
/// again immediately before extraction.
fn local_package_spec_blocking(
    archive: &FsPath,
    entry: &LocalCatalogEntry,
) -> Result<TemplateSpec, String> {
    let expected_package = image_install::package_name(&entry.alias);
    if entry.package != expected_package {
        return Err(format!(
            "local MicroRegistry package mismatch for {}: expected {}, got {}",
            entry.alias, expected_package, entry.package
        ));
    }
    if entry.sha256.is_empty() {
        return Err(format!(
            "local MicroRegistry row for {} is missing package sha256",
            entry.alias
        ));
    }

    let actual_sha256 = sha256_file(archive)?;
    if !actual_sha256.eq_ignore_ascii_case(&entry.sha256) {
        return Err(format!(
            "staged package checksum mismatch for {}: expected {}, got {}",
            entry.alias, entry.sha256, actual_sha256
        ));
    }

    let file =
        File::open(archive).map_err(|error| format!("open {}: {error}", archive.display()))?;
    let decoder = zstd::stream::read::Decoder::new(file)
        .map_err(|error| format!("zstd decoder {}: {error}", archive.display()))?;
    let mut tar = tar::Archive::new(decoder);
    for archive_entry in tar
        .entries()
        .map_err(|error| format!("tar entries: {error}"))?
    {
        let mut archive_entry = archive_entry.map_err(|error| format!("tar entry: {error}"))?;
        let name = archive_entry
            .path()
            .map_err(|error| format!("tar member path: {error}"))?
            .to_string_lossy()
            .replace('\\', "/");
        if name != crate::package::TEMPLATE_SPEC_MEMBER {
            continue;
        }
        let spec: TemplateSpec = serde_json::from_reader(&mut archive_entry)
            .map_err(|error| format!("parse {}: {error}", crate::package::TEMPLATE_SPEC_MEMBER))?;
        if spec.alias != entry.alias {
            return Err(format!(
                "local package alias mismatch: catalog has {}, package has {}",
                entry.alias, spec.alias
            ));
        }
        if spec.version != entry.version {
            return Err(format!(
                "local package version mismatch for {}: catalog has {}, package has {}",
                entry.alias, entry.version, spec.version
            ));
        }
        return Ok(spec);
    }

    Err(format!(
        "archive missing required member `{}`",
        crate::package::TEMPLATE_SPEC_MEMBER
    ))
}

/// Validate a staged host-local package on Tokio's blocking pool.
async fn local_package_spec(
    image_root: PathBuf,
    entry: LocalCatalogEntry,
) -> Result<TemplateSpec, String> {
    let archive = image_install::staged_package_path(&image_root, &entry.alias);
    tokio::task::spawn_blocking(move || local_package_spec_blocking(&archive, &entry))
        .await
        .map_err(|error| format!("local package validation task panicked: {error}"))?
}

/// `GET /api/images/{alias}/install` — latest installation snapshot for a
/// built-in, installed, or host-local MicroRegistry alias.
pub async fn get_image_install(
    State(state): State<AppState>,
    Path(alias): Path<String>,
    Extension(request_id): Extension<RequestId>,
) -> Result<Json<ImageInstallResponse>, AppError> {
    if TemplateRegistry::known_spec(&alias).is_some()
        || state.templates.resolve_alias(&alias).is_some()
    {
        return super::images_core::get_image_install(
            State(state),
            Path(alias),
            Extension(request_id),
        )
        .await;
    }

    if host_local_catalog_entry(&state, &alias, request_id)
        .await?
        .is_none()
    {
        return Err(AppError::not_found(request_id.0));
    }
    Ok(Json(state.image_installs.snapshot(&alias)))
}

/// `POST /api/images/{alias}/install` — install a prepared public package or
/// a checksum-pinned package from the host-local MicroRegistry catalog.
pub async fn start_image_install(
    State(state): State<AppState>,
    Path(alias): Path<String>,
    Extension(request_id): Extension<RequestId>,
) -> Result<Response, AppError> {
    if TemplateRegistry::known_spec(&alias).is_some() {
        let response = super::images_core::start_image_install(
            State(state),
            Path(alias),
            Extension(request_id),
        )
        .await?;
        return Ok(response.into_response());
    }

    let Some(local_entry) = host_local_catalog_entry(&state, &alias, request_id).await? else {
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
    let image_root = templates.image_root_path().to_path_buf();
    let alias_for_job = alias.clone();
    tokio::spawn(async move {
        match local_package_spec(image_root, local_entry).await {
            Ok(spec) => {
                tracker.append_log(
                    &alias_for_job,
                    "local MicroRegistry package checksum verified",
                );
                image_install::run_image_install(tracker, templates, spec).await;
            }
            Err(error) => tracker.finish_err(&alias_for_job, error),
        }
    });

    Ok((StatusCode::ACCEPTED, Json(response)).into_response())
}

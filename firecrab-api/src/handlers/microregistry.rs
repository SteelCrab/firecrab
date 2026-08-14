//! Public MicroRegistry catalog for the Images dashboard.
//!
//! The browser calls this endpoint rather than the R2 public bucket directly:
//! it keeps registry topology in one server-side configuration point and avoids
//! depending on the bucket's CORS policy. Package acquisition itself remains
//! on the existing `/api/images/{alias}/package` endpoint, which verifies the
//! per-distribution checksum before an archive becomes installable.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::{Path as AliasPath, State};
use axum::http::StatusCode;
use firecrab_api_types::{
    ImageInstallResponse, MicroRegistryImageResponse, MicroRegistryRegisterRequest,
    MicroRegistryResponse,
};
use serde::Deserialize;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::image_install::{self, Architecture, ImageInstallTracker};
use crate::server::RequestId;
use crate::state::{AppState, LocalCatalogEntry};
use crate::templates::TemplateRegistry;

const CATALOG_MAX_BYTES: usize = 256 * 1024;

#[derive(Debug, Deserialize)]
struct Catalog {
    images: Vec<CatalogImage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogImage {
    alias: String,
    architecture: Architecture,
    #[serde(deserialize_with = "catalog_version")]
    version: String,
    package: String,
    sha256: String,
    min_disk_gb: u16,
    published_at: String,
}

/// Accept both the first publisher's numeric version and the current string
/// version. The dashboard always receives a string, keeping its type stable.
fn catalog_version<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::String(version) if !version.is_empty() => Ok(version),
        serde_json::Value::Number(version) if version.is_u64() => Ok(version.to_string()),
        _ => Err(D::Error::custom(
            "version must be a non-empty string or unsigned integer",
        )),
    }
}

fn unavailable(request_id: RequestId, detail: impl std::fmt::Display) -> AppError {
    tracing::warn!(request_id = %request_id.0, error = %detail, "MicroRegistry catalog request failed");
    AppError::unavailable("MicroRegistry catalog is unavailable", request_id.0)
}

/// Smallest disk (GiB) that can hold `rootfs_bytes`, matching `images.rs`.
fn min_disk_gb_for(rootfs_bytes: u64) -> u16 {
    const GIB: u64 = 1024 * 1024 * 1024;
    rootfs_bytes.div_ceil(GIB).try_into().unwrap_or(u16::MAX)
}

fn rfc3339_utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    unix_secs_to_rfc3339(secs)
}

fn unix_secs_to_rfc3339(secs: u64) -> String {
    const MONTH_DAYS: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut days = secs / 86_400;
    let rem = secs % 86_400;
    let hour = rem / 3600;
    let min = (rem % 3600) / 60;
    let sec = rem % 60;
    let mut year = 1970_u64;
    loop {
        let length = if is_leap_year(year) { 366 } else { 365 };
        if days < length {
            break;
        }
        days -= length;
        year += 1;
    }
    let mut month = 1_u64;
    while month <= 12 {
        let mut dim = MONTH_DAYS[(month - 1) as usize];
        if month == 2 && is_leap_year(year) {
            dim = 29;
        }
        if days < dim {
            break;
        }
        days -= dim;
        month += 1;
    }
    format!(
        "{year:04}-{month:02}-{:02}T{hour:02}:{min:02}:{sec:02}Z",
        days + 1
    )
}

fn is_leap_year(year: u64) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn local_catalog_image(
    templates: &TemplateRegistry,
    image_root: &Path,
    entry: &LocalCatalogEntry,
) -> MicroRegistryImageResponse {
    let installed = templates.resolve_alias(&entry.alias);
    let min_disk_gb = installed
        .as_ref()
        .map(|template| min_disk_gb_for(template.rootfs.length()))
        .unwrap_or(0);
    MicroRegistryImageResponse {
        installed: installed.is_some(),
        package_staged: image_install::staged_package_exists(image_root, &entry.alias),
        package_origin: image_install::staged_package_origin(image_root, &entry.alias),
        downloadable: false,
        alias: entry.alias.clone(),
        version: entry.version.clone(),
        package: String::new(),
        sha256: String::new(),
        min_disk_gb,
        published_at: entry.published_at.clone(),
    }
}

/// `GET /api/microregistry`: supported published packages plus this host's
/// local install/cache state. Entries outside the release manifest are hidden
/// because this Firecrab build cannot validate or install them. Locally
/// registered custom aliases are appended after the public filters so a
/// catalog refresh cannot drop them.
pub async fn list_microregistry(
    State(state): State<AppState>,
    axum::Extension(request_id): axum::Extension<RequestId>,
) -> Result<Json<MicroRegistryResponse>, AppError> {
    let Some(base_url) = state.image_packages.base_url().map(str::to_owned) else {
        return Err(AppError::unavailable(
            "MicroRegistry is disabled by FIRECRAB_IMAGE_BASE_URL",
            request_id.0,
        ));
    };
    let source = format!("{}/catalog.json", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|error| unavailable(request_id, error))?;
    let mut response = client
        .get(&source)
        .send()
        .await
        .map_err(|error| unavailable(request_id, error))?
        .error_for_status()
        .map_err(|error| unavailable(request_id, error))?;

    if response
        .content_length()
        .is_some_and(|length| length > CATALOG_MAX_BYTES as u64)
    {
        return Err(unavailable(
            request_id,
            "catalog content-length exceeds limit",
        ));
    }

    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| unavailable(request_id, error))?
    {
        if body.len().saturating_add(chunk.len()) > CATALOG_MAX_BYTES {
            return Err(unavailable(request_id, "catalog body exceeds limit"));
        }
        body.extend_from_slice(&chunk);
    }
    let catalog: Catalog =
        serde_json::from_slice(&body).map_err(|error| unavailable(request_id, error))?;

    let templates = state.templates.clone();
    let image_root = templates.image_root_path().to_owned();
    let mut images = catalog
        .images
        .into_iter()
        .filter(|image| image.architecture == Architecture::HOST)
        .filter(|image| TemplateRegistry::known_spec(&image.alias).is_some())
        .map(|image| {
            let package_origin = image_install::staged_package_origin(&image_root, &image.alias);
            MicroRegistryImageResponse {
                installed: templates.resolve_alias(&image.alias).is_some(),
                package_staged: image_install::staged_package_exists(&image_root, &image.alias),
                package_origin,
                downloadable: true,
                alias: image.alias,
                version: image.version,
                package: image.package,
                sha256: image.sha256,
                min_disk_gb: image.min_disk_gb,
                published_at: image.published_at,
            }
        })
        .collect::<Vec<_>>();

    let locals: Vec<LocalCatalogEntry> = state
        .microregistry_local
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .values()
        .cloned()
        .collect();
    for entry in locals {
        if images.iter().any(|image| image.alias == entry.alias) {
            continue;
        }
        images.push(local_catalog_image(&templates, &image_root, &entry));
    }
    images.sort_by(|left, right| left.alias.cmp(&right.alias));

    Ok(Json(MicroRegistryResponse { source, images }))
}

/// `POST /api/microregistry/register` — claim the alias and start the job.
pub async fn start_microregistry_register(
    State(state): State<AppState>,
    axum::Extension(request_id): axum::Extension<RequestId>,
    ValidatedJson(body): ValidatedJson<MicroRegistryRegisterRequest>,
) -> Result<(StatusCode, Json<ImageInstallResponse>), AppError> {
    let mut fields = BTreeMap::new();
    if body.alias.trim().is_empty() {
        fields.insert("alias".to_owned(), "must not be empty".to_owned());
    }
    if body.version.trim().is_empty() {
        fields.insert("version".to_owned(), "must not be empty".to_owned());
    }
    if !fields.is_empty() {
        return Err(AppError::validation(fields, request_id.0));
    }

    if body.alias == crate::microboot::MICROBOOT_ALIAS {
        return Err(AppError::not_found(request_id.0));
    }

    if TemplateRegistry::known_spec(&body.alias).is_some() {
        return Err(AppError::conflict(
            "alias_collision",
            "alias is already published in the catalog",
            request_id.0,
        ));
    }

    {
        let local = state
            .microregistry_local
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if local.contains_key(&body.alias) {
            return Err(AppError::conflict(
                "alias_collision",
                "alias is already published in the catalog",
                request_id.0,
            ));
        }
    }

    if state.templates.resolve_alias(&body.alias).is_none() {
        return Err(AppError::not_found(request_id.0));
    }

    let response = match state
        .microregistry_registers
        .begin_with(&body.alias, "register started")
    {
        Ok(snapshot) => snapshot,
        Err("running") => {
            return Err(AppError::conflict(
                "register_in_progress",
                "a register is already running for this image",
                request_id.0,
            ));
        }
        Err(_) => return Err(AppError::internal(request_id.0)),
    };

    let tracker = state.microregistry_registers.clone();
    let templates = state.templates.clone();
    let catalog = Arc::clone(&state.microregistry_local);
    let alias = body.alias.clone();
    let version = body.version.clone();
    tokio::spawn(async move {
        run_microregistry_register(tracker, templates, catalog, alias, version).await;
    });

    Ok((StatusCode::ACCEPTED, Json(response)))
}

/// `GET /api/microregistry/register/{alias}` — latest snapshot, including idle.
pub async fn get_microregistry_register(
    State(state): State<AppState>,
    AliasPath(alias): AliasPath<String>,
    axum::Extension(_request_id): axum::Extension<RequestId>,
) -> Result<Json<ImageInstallResponse>, AppError> {
    Ok(Json(state.microregistry_registers.snapshot(&alias)))
}

async fn run_microregistry_register(
    tracker: ImageInstallTracker,
    templates: Arc<TemplateRegistry>,
    catalog: Arc<Mutex<HashMap<String, LocalCatalogEntry>>>,
    alias: String,
    version: String,
) {
    tracker.append_log(&alias, "checking installed template");
    if templates.resolve_alias(&alias).is_none() {
        tracker.finish_err_with(&alias, "register failed: template is not installed");
        return;
    }
    tracker.append_log(&alias, "updating catalog");
    let published_at = rfc3339_utc_now();
    {
        let mut catalog = catalog
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        catalog.insert(
            alias.clone(),
            LocalCatalogEntry {
                alias: alias.clone(),
                version,
                published_at,
            },
        );
    }
    tracker.finish_ok_with(&alias, "register succeeded — catalog updated");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::ValidatedJson;
    use crate::image_install::ImageInstallTracker;
    use crate::state::AppState;
    use crate::templates::TemplateSpec;
    use axum::Json;
    use axum::extract::{Path, State};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::{Extension, Router};
    use firecrab_api_types::ImageInstallStatus;
    use serde_json::json;
    use tempfile::tempdir;

    async fn empty_state(root: &std::path::Path) -> AppState {
        let templates = TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
        AppState::with_db_file(templates, root.join("state.db"))
            .await
            .unwrap()
    }

    fn install_custom(state: &AppState, alias: &str) {
        let root = state.templates.image_root_path();
        std::fs::create_dir_all(root.join("kernel")).unwrap();
        std::fs::write(root.join("kernel/vmlinux"), b"kernel").unwrap();
        std::fs::write(root.join(format!("{alias}.ext4")), vec![0_u8; 64]).unwrap();
        state
            .templates
            .register_spec(TemplateSpec {
                alias: alias.to_owned(),
                version: format!("{alias}-local"),
                kernel: std::path::PathBuf::from("kernel/vmlinux"),
                initrd: None,
                rootfs: std::path::PathBuf::from(format!("{alias}.ext4")),
                boot_args: "console=ttyS0 root=/dev/vda rw".to_owned(),
            })
            .unwrap();
    }

    fn request_id() -> RequestId {
        RequestId(uuid::Uuid::nil())
    }

    fn register_body(alias: &str, version: &str) -> ValidatedJson<MicroRegistryRegisterRequest> {
        ValidatedJson(MicroRegistryRegisterRequest {
            alias: alias.to_owned(),
            version: version.to_owned(),
        })
    }

    async fn error_json(error: AppError) -> (StatusCode, serde_json::Value) {
        let response = error.into_response();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&body).expect("error body is json"),
        )
    }

    async fn wait_for_status(
        state: &AppState,
        alias: &str,
        want: ImageInstallStatus,
    ) -> ImageInstallResponse {
        let mut snapshot = state.microregistry_registers.snapshot(alias);
        for _ in 0..80 {
            if snapshot.status == want {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            snapshot = state.microregistry_registers.snapshot(alias);
        }
        snapshot
    }

    fn local_aliases(state: &AppState) -> Vec<String> {
        let local = state
            .microregistry_local
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut aliases: Vec<String> = local.keys().cloned().collect();
        aliases.sort();
        aliases
    }

    async fn state_with_catalog(
        images: serde_json::Value,
    ) -> (AppState, tempfile::TempDir, String) {
        let app = Router::new().route(
            "/catalog.json",
            get(move || {
                let images = images.clone();
                async move { Json(json!({ "images": images })) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let directory = tempdir().unwrap();
        let mut state = empty_state(directory.path()).await;
        state.image_packages = ImageInstallTracker::with_base_url(format!("http://{address}"));
        (state, directory, format!("http://{address}/catalog.json"))
    }

    #[tokio::test]
    async fn lists_published_catalog_with_local_state() {
        let app = Router::new().route(
            "/catalog.json",
            get(|| async {
                Json(json!({
                    "images": [{
                        "alias": "ubuntu-26.04",
                        "architecture": image_install::host_architecture(),
                        "version": 3,
                        "package": format!(
                            "ubuntu/26.04/{}/ubuntu-26.04.tar.zst",
                            image_install::host_architecture()
                        ),
                        "sha256": "aabb",
                        "minDiskGb": 2,
                        "publishedAt": "2026-08-09T10:00:00Z"
                    }, {
                        "alias": "rocky-9",
                        "architecture": image_install::host_architecture(),
                        "version": "2",
                        "package": "rocky/9/rocky-9.tar.zst",
                        "sha256": "ccdd",
                        "minDiskGb": 2,
                        "publishedAt": "2026-08-09T10:00:00Z"
                    }, {
                        "alias": "wrong-architecture",
                        "architecture": Architecture::HOST.other().as_str(),
                        "version": "1",
                        "package": "wrong/package.tar.zst",
                        "sha256": "eeff",
                        "minDiskGb": 1,
                        "publishedAt": "2026-08-09T10:00:00Z"
                    }]
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let directory = tempdir().unwrap();
        let mut state = empty_state(directory.path()).await;
        state.image_packages = ImageInstallTracker::with_base_url(format!("http://{address}"));
        let Json(response) =
            list_microregistry(State(state), Extension(RequestId(uuid::Uuid::nil())))
                .await
                .unwrap();

        assert_eq!(response.source, format!("http://{address}/catalog.json"));
        assert_eq!(response.images.len(), 1);
        assert_eq!(response.images[0].alias, "ubuntu-26.04");
        assert_eq!(response.images[0].version, "3");
        assert!(response.images[0].downloadable);
        assert!(!response.images[0].installed);
        assert!(!response.images[0].package_staged);
    }

    #[test]
    fn catalog_entries_require_an_explicit_architecture() {
        let error = serde_json::from_value::<Catalog>(json!({
            "images": [{
                "alias": "ubuntu-26.04",
                "version": 1,
                "package": "ubuntu/26.04/x86_64/ubuntu-26.04.tar.zst",
                "sha256": "aabb",
                "minDiskGb": 2,
                "publishedAt": "2026-08-09T10:00:00Z"
            }]
        }))
        .unwrap_err();

        assert!(error.to_string().contains("architecture"));
    }

    /// `arm64` and `amd64` are the vocabulary of rootfs *filenames*, not of
    /// catalog entries. Accepting them here would let a package be published
    /// under a label nothing else in firecrab resolves.
    #[test]
    fn catalog_entries_reject_a_debian_architecture_spelling() {
        for spelling in ["arm64", "amd64"] {
            let error = serde_json::from_value::<Catalog>(json!({
                "images": [{
                    "alias": "ubuntu-26.04",
                    "version": 1,
                    "architecture": spelling,
                    "package": "ubuntu/26.04/ubuntu-26.04.tar.zst",
                    "sha256": "aabb",
                    "minDiskGb": 2,
                    "publishedAt": "2026-08-09T10:00:00Z"
                }]
            }))
            .unwrap_err();

            let message = error.to_string();
            assert!(message.contains(spelling), "{message}");
            assert!(message.contains("x86_64"), "{message}");
            assert!(message.contains("aarch64"), "{message}");
        }
    }

    #[test]
    fn unix_secs_to_rfc3339_formats_utc() {
        assert_eq!(unix_secs_to_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(unix_secs_to_rfc3339(1_702_080_000), "2023-12-09T00:00:00Z");
        assert_eq!(unix_secs_to_rfc3339(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[tokio::test]
    async fn start_microregistry_register_accepts_an_installed_custom_alias() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        install_custom(&state, "nginx-1.27");

        let (status, Json(response)) = start_microregistry_register(
            State(state.clone()),
            Extension(request_id()),
            register_body("nginx-1.27", "1"),
        )
        .await
        .expect("installed custom alias must be accepted");

        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(response.alias, "nginx-1.27");
        assert_eq!(response.status, ImageInstallStatus::Running);
        assert!(response.log.contains("register started"));

        let Json(polled) = get_microregistry_register(
            State(state.clone()),
            Path("nginx-1.27".to_owned()),
            Extension(request_id()),
        )
        .await
        .expect("accepted job must have a snapshot");
        assert_eq!(polled.alias, response.alias);
        assert_eq!(polled.status, ImageInstallStatus::Running);

        let finished = wait_for_status(&state, "nginx-1.27", ImageInstallStatus::Succeeded).await;
        assert_eq!(finished.status, ImageInstallStatus::Succeeded);
        assert!(
            finished
                .log
                .contains("register succeeded — catalog updated"),
            "succeeded job must record the catalog update: {}",
            finished.log
        );
        assert_eq!(local_aliases(&state), ["nginx-1.27"]);
        let published_at = state
            .microregistry_local
            .lock()
            .unwrap()
            .get("nginx-1.27")
            .expect("local row")
            .published_at
            .clone();
        assert!(published_at.contains('T') && published_at.ends_with('Z'));
    }

    #[tokio::test]
    async fn start_microregistry_register_rejects_an_in_progress_job() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        install_custom(&state, "nginx-1.27");
        state
            .microregistry_registers
            .begin_with("nginx-1.27", "register started")
            .unwrap();

        let (status, json) = error_json(
            start_microregistry_register(
                State(state.clone()),
                Extension(request_id()),
                register_body("nginx-1.27", "1"),
            )
            .await
            .unwrap_err(),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(json["error"]["code"], "register_in_progress");
        assert!(local_aliases(&state).is_empty());
        assert_eq!(
            state.microregistry_registers.snapshot("nginx-1.27").status,
            ImageInstallStatus::Running
        );
    }

    #[tokio::test]
    async fn start_microregistry_register_rejects_a_public_alias() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;

        let (status, json) = error_json(
            start_microregistry_register(
                State(state.clone()),
                Extension(request_id()),
                register_body("ubuntu-26.04", "3"),
            )
            .await
            .unwrap_err(),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(json["error"]["code"], "alias_collision");
        assert_eq!(
            state
                .microregistry_registers
                .snapshot("ubuntu-26.04")
                .status,
            ImageInstallStatus::Idle
        );
        assert!(local_aliases(&state).is_empty());
    }

    #[tokio::test]
    async fn start_microregistry_register_rejects_a_local_duplicate() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        install_custom(&state, "nginx-1.27");

        let _first = start_microregistry_register(
            State(state.clone()),
            Extension(request_id()),
            register_body("nginx-1.27", "1"),
        )
        .await
        .expect("first register");
        let finished = wait_for_status(&state, "nginx-1.27", ImageInstallStatus::Succeeded).await;
        assert_eq!(finished.status, ImageInstallStatus::Succeeded);

        let (status, json) = error_json(
            start_microregistry_register(
                State(state.clone()),
                Extension(request_id()),
                register_body("nginx-1.27", "1"),
            )
            .await
            .unwrap_err(),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(json["error"]["code"], "alias_collision");
        assert_eq!(local_aliases(&state), ["nginx-1.27"]);
    }

    #[tokio::test]
    async fn start_microregistry_register_rejects_empty_fields_without_a_job() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        install_custom(&state, "nginx-1.27");

        for (alias, version, field) in [
            ("", "1", "alias"),
            ("   ", "1", "alias"),
            ("nginx-1.27", "", "version"),
            ("nginx-1.27", "  ", "version"),
        ] {
            let (status, json) = error_json(
                start_microregistry_register(
                    State(state.clone()),
                    Extension(request_id()),
                    register_body(alias, version),
                )
                .await
                .unwrap_err(),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{alias:?} {version:?}");
            assert_eq!(json["error"]["code"], "validation_failed");
            assert!(
                json["error"]["fields"][field].is_string(),
                "expected field {field}: {json}"
            );
            assert_eq!(
                state.microregistry_registers.snapshot("nginx-1.27").status,
                ImageInstallStatus::Idle
            );
            assert!(local_aliases(&state).is_empty());
        }
    }

    #[tokio::test]
    async fn start_microregistry_register_rejects_unknown_uninstalled_and_microboot() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;

        for alias in [
            "does-not-exist",
            "nginx-1.27",
            crate::microboot::MICROBOOT_ALIAS,
        ] {
            let (status, json) = error_json(
                start_microregistry_register(
                    State(state.clone()),
                    Extension(request_id()),
                    register_body(alias, "1"),
                )
                .await
                .unwrap_err(),
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{alias}");
            assert_eq!(json["error"]["code"], "not_found");
            assert_eq!(
                state.microregistry_registers.snapshot(alias).status,
                ImageInstallStatus::Idle
            );
        }
        assert!(local_aliases(&state).is_empty());
    }

    #[tokio::test]
    async fn list_microregistry_merges_public_and_local_rows() {
        let package = format!(
            "ubuntu/26.04/{}/ubuntu-26.04.tar.zst",
            image_install::host_architecture()
        );
        let (state, _directory, source) = state_with_catalog(json!([
            {
                "alias": "ubuntu-26.04",
                "architecture": image_install::host_architecture(),
                "version": 3,
                "package": package,
                "sha256": "aabb",
                "minDiskGb": 2,
                "publishedAt": "2026-08-09T10:00:00Z"
            },
            {
                "alias": "wrong-architecture",
                "architecture": Architecture::HOST.other().as_str(),
                "version": "1",
                "package": "wrong/package.tar.zst",
                "sha256": "eeff",
                "minDiskGb": 1,
                "publishedAt": "2026-08-09T10:00:00Z"
            },
            {
                "alias": "custom-remote",
                "architecture": image_install::host_architecture(),
                "version": "9",
                "package": "custom/remote.tar.zst",
                "sha256": "ffff",
                "minDiskGb": 1,
                "publishedAt": "2026-08-09T10:00:00Z"
            }
        ]))
        .await;
        install_custom(&state, "nginx-1.27");

        let _started = start_microregistry_register(
            State(state.clone()),
            Extension(request_id()),
            register_body("nginx-1.27", "1"),
        )
        .await
        .expect("local register");
        let finished = wait_for_status(&state, "nginx-1.27", ImageInstallStatus::Succeeded).await;
        assert_eq!(finished.status, ImageInstallStatus::Succeeded);

        let Json(response) = list_microregistry(State(state.clone()), Extension(request_id()))
            .await
            .unwrap();
        assert_eq!(response.source, source);
        let aliases: Vec<&str> = response
            .images
            .iter()
            .map(|image| image.alias.as_str())
            .collect();
        assert_eq!(aliases, ["nginx-1.27", "ubuntu-26.04"]);

        let ubuntu = response
            .images
            .iter()
            .find(|image| image.alias == "ubuntu-26.04")
            .unwrap();
        assert!(ubuntu.downloadable);
        assert_eq!(ubuntu.package, package);
        assert_eq!(ubuntu.sha256, "aabb");
        assert_eq!(ubuntu.version, "3");

        let local = response
            .images
            .iter()
            .find(|image| image.alias == "nginx-1.27")
            .unwrap();
        assert!(!local.downloadable);
        assert_eq!(local.package, "");
        assert_eq!(local.sha256, "");
        assert_eq!(local.version, "1");
        assert!(local.installed);
        assert_eq!(local.min_disk_gb, 1);

        let Json(again) = list_microregistry(State(state), Extension(request_id()))
            .await
            .unwrap();
        assert!(
            again
                .images
                .iter()
                .any(|image| image.alias == "nginx-1.27" && !image.downloadable)
        );
    }

    #[tokio::test]
    async fn a_failed_register_job_leaves_no_local_row() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        install_custom(&state, "nginx-1.27");
        state
            .microregistry_registers
            .begin_with("nginx-1.27", "register started")
            .unwrap();
        state.templates.unregister_alias("nginx-1.27");

        run_microregistry_register(
            state.microregistry_registers.clone(),
            state.templates.clone(),
            Arc::clone(&state.microregistry_local),
            "nginx-1.27".to_owned(),
            "1".to_owned(),
        )
        .await;

        assert_eq!(
            state.microregistry_registers.snapshot("nginx-1.27").status,
            ImageInstallStatus::Failed
        );
        assert!(
            state
                .microregistry_registers
                .snapshot("nginx-1.27")
                .log
                .contains("register failed"),
            "failed job must record why"
        );
        assert!(local_aliases(&state).is_empty());
    }

    #[tokio::test]
    async fn get_microregistry_register_returns_idle_for_an_unknown_alias() {
        let directory = tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        let Json(snapshot) = get_microregistry_register(
            State(state),
            Path("never-seen".to_owned()),
            Extension(request_id()),
        )
        .await
        .unwrap();
        assert_eq!(snapshot.alias, "never-seen");
        assert_eq!(snapshot.status, ImageInstallStatus::Idle);
        assert!(snapshot.log.is_empty());
    }
}

//! Public MicroRegistry catalog for the Images dashboard.
//!
//! The browser calls this endpoint rather than the R2 public bucket directly:
//! it keeps registry topology in one server-side configuration point and avoids
//! depending on the bucket's CORS policy. Package acquisition itself remains
//! on the existing `/api/images/{alias}/package` endpoint, which verifies the
//! per-distribution checksum before an archive becomes installable.

use std::time::Duration;

use axum::Json;
use axum::extract::State;
use firecrab_api_types::{MicroRegistryImageResponse, MicroRegistryResponse};
use serde::Deserialize;

use crate::error::AppError;
use crate::image_install;
use crate::server::RequestId;
use crate::state::AppState;
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
    architecture: CatalogArchitecture,
    #[serde(deserialize_with = "catalog_version")]
    version: String,
    package: String,
    sha256: String,
    min_disk_gb: u16,
    published_at: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
enum CatalogArchitecture {
    #[serde(rename = "x86_64")]
    X86_64,
    #[serde(rename = "aarch64")]
    Aarch64,
}

impl CatalogArchitecture {
    fn is_host(&self) -> bool {
        #[cfg(target_arch = "aarch64")]
        {
            *self == Self::Aarch64
        }
        #[cfg(target_arch = "x86_64")]
        {
            *self == Self::X86_64
        }
    }
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

/// `GET /api/microregistry`: public published packages plus this host's local
/// install/cache state. Catalog metadata is display-only; package download is
/// still restricted to aliases Firecrab knows how to validate and install.
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
        .filter(|image| image.architecture.is_host())
        .map(|image| {
            let package_origin = image_install::staged_package_origin(&image_root, &image.alias);
            MicroRegistryImageResponse {
                installed: templates.resolve_alias(&image.alias).is_some(),
                package_staged: image_install::staged_package_exists(&image_root, &image.alias),
                package_origin,
                downloadable: TemplateRegistry::known_spec(&image.alias).is_some(),
                alias: image.alias,
                version: image.version,
                package: image.package,
                sha256: image.sha256,
                min_disk_gb: image.min_disk_gb,
                published_at: image.published_at,
            }
        })
        .collect::<Vec<_>>();
    images.sort_by(|left, right| left.alias.cmp(&right.alias));

    Ok(Json(MicroRegistryResponse { source, images }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_install::ImageInstallTracker;
    use crate::state::AppState;
    use axum::routing::get;
    use axum::{Extension, Router};
    use serde_json::json;
    use tempfile::tempdir;
    use tokio::net::TcpListener;

    #[cfg(target_arch = "aarch64")]
    const OTHER_ARCHITECTURE: &str = "x86_64";
    #[cfg(target_arch = "x86_64")]
    const OTHER_ARCHITECTURE: &str = "aarch64";

    async fn empty_state(root: &std::path::Path) -> AppState {
        let templates = TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
        AppState::with_db_file(templates, root.join("state.db"))
            .await
            .unwrap()
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
                        "package": "ubuntu/26.04/ubuntu-26.04.tar.zst",
                        "sha256": "aabb",
                        "minDiskGb": 2,
                        "publishedAt": "2026-08-09T10:00:00Z"
                    }, {
                        "alias": "example-1",
                        "architecture": image_install::host_architecture(),
                        "version": "1",
                        "package": "example/1/example-1.tar.zst",
                        "sha256": "ccdd",
                        "minDiskGb": 1,
                        "publishedAt": "2026-08-09T10:00:00Z"
                    }, {
                        "alias": "wrong-architecture",
                        "architecture": OTHER_ARCHITECTURE,
                        "version": "1",
                        "package": "wrong/package.tar.zst",
                        "sha256": "eeff",
                        "minDiskGb": 1,
                        "publishedAt": "2026-08-09T10:00:00Z"
                    }]
                }))
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
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
        assert_eq!(response.images.len(), 2);
        assert_eq!(response.images[0].alias, "example-1");
        assert!(!response.images[0].downloadable);
        assert_eq!(response.images[1].alias, "ubuntu-26.04");
        assert_eq!(response.images[1].version, "3");
        assert!(response.images[1].downloadable);
        assert!(!response.images[1].installed);
        assert!(!response.images[1].package_staged);
    }

    #[test]
    fn catalog_entries_require_an_explicit_architecture() {
        let error = serde_json::from_value::<Catalog>(json!({
            "images": [{
                "alias": "ubuntu-26.04",
                "version": 1,
                "package": "ubuntu/26.04/ubuntu-26.04.tar.zst",
                "sha256": "aabb",
                "minDiskGb": 2,
                "publishedAt": "2026-08-09T10:00:00Z"
            }]
        }))
        .unwrap_err();

        assert!(error.to_string().contains("architecture"));
    }
}

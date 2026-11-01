//! Catalog and installation jobs for host-built stable Linux guest kernels.

use crate::image_install::{Architecture, ImageInstallTracker};
use crate::oci::{kernel, kernel_legacy};
use crate::templates::TemplateRegistry;
use firecrab_api_types::{KernelInstallResponse, KernelResponse};
use std::path::{Path, PathBuf};

/// Three recent published releases plus locally installed versions for rollback.
pub async fn list(
    image_root: &Path,
    templates: &TemplateRegistry,
    downloads: &ImageInstallTracker,
) -> Result<Vec<KernelResponse>, crate::oci::ResolveError> {
    let architecture = Architecture::HOST;
    let mut versions = kernel::installed_versions(image_root, architecture).await;
    match kernel::latest_stable(image_root, downloads.base_url()).await {
        Ok(release) => {
            let recent = if downloads.base_url().is_none() {
                vec![release.version]
            } else {
                match kernel::recent_stable_versions(image_root, &release).await {
                    Ok(recent) => recent,
                    Err(error) => {
                        tracing::debug!(%error, "kernel checksum index unavailable; listing stable and local kernels");
                        vec![release.version]
                    }
                }
            };
            versions.retain(|version| !recent.contains(version));
            versions.splice(0..0, recent);
        }
        Err(error) if versions.is_empty() && downloads.base_url().is_some() => return Err(error),
        Err(error) => {
            tracing::debug!(%error, "kernel stable catalog is unavailable; listing local kernels")
        }
    }
    let mut result = Vec::new();
    for version in versions {
        let installed = kernel::cached_kernel_is_valid(image_root, architecture, &version).await;
        let relative =
            kernel::cache_relative(architecture, &version).expect("validated catalog version");
        let receipt = kernel::built_kernel(image_root, architecture, &version).await;
        let legacy = kernel_legacy::image_digest(architecture, &version);
        let image_sha256 = receipt
            .as_ref()
            .map(|receipt| receipt.image_sha256.clone())
            .or_else(|| legacy.as_ref().map(|pin| digest_body(pin)))
            .unwrap_or_default();
        let package_sha256 = receipt
            .as_ref()
            .map(|receipt| receipt.source_sha256.clone())
            .unwrap_or_default();
        result.push(KernelResponse {
            version: version.clone(),
            architecture: architecture.as_str().to_owned(),
            image: kernel::image_name(architecture, &version).expect("validated image name"),
            image_sha256,
            package_sha256,
            size_bytes: if installed {
                tokio::fs::metadata(image_root.join(&relative))
                    .await
                    .ok()
                    .map(|metadata| metadata.len())
            } else {
                None
            },
            installed,
            in_use: templates
                .list_aliases()
                .into_iter()
                .any(|template| template.kernel.relative_path() == relative),
            package_url: downloads
                .base_url()
                .and_then(|_| kernel::source_url(&version)),
        });
    }
    Ok(result)
}

/// Version validation is independent from network availability.
pub fn cache_path(version: &str) -> Option<PathBuf> {
    kernel::cache_relative(Architecture::HOST, version)
}

/// Recover a kernel version only from a managed path for the host architecture.
pub fn version_for_path(path: &Path) -> Option<String> {
    kernel::version_for_path(Architecture::HOST, path)
}

/// Build or reuse a kernel and record the install job's final result.
pub async fn run_install(
    tracker: ImageInstallTracker,
    templates: TemplateRegistry,
    version: String,
    releases_url: Option<String>,
) {
    tracker.append_log(
        &version,
        format!("resolving Linux {version} from kernel.org source"),
    );
    match kernel::ensure_managed_kernel(
        templates.image_root_path(),
        Architecture::HOST,
        &version,
        releases_url.as_deref(),
        Some(&tracker),
    )
    .await
    {
        Ok(path) => {
            tracker.append_log(&version, format!("verified and cached {}", path.display()));
            tracker.finish_ok_with(&version, "kernel ready — source and image verified");
        }
        Err(error) => tracker.finish_err_with(&version, format!("kernel install failed: {error}")),
    }
}

/// Expose the current install status, log, timing, and source download progress.
pub fn snapshot(tracker: &ImageInstallTracker, version: &str) -> KernelInstallResponse {
    let snapshot = tracker.snapshot(version);
    KernelInstallResponse {
        version: version.to_owned(),
        status: snapshot.status,
        log: snapshot.log,
        started_at_ms: snapshot.started_at_ms,
        ended_at_ms: snapshot.ended_at_ms,
        downloaded_bytes: snapshot.downloaded_bytes,
        total_bytes: snapshot.total_bytes,
    }
}

/// Convert a prefixed legacy digest to the API's bare hexadecimal form.
fn digest_body(value: &str) -> String {
    value.strip_prefix("sha256:").unwrap_or(value).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn cache_path_accepts_numeric_releases_and_rejects_paths_and_prereleases() {
        assert!(cache_path("7.2.12").is_some());
        for version in ["not-a-kernel", "../7.2.12", "7.3-rc1"] {
            assert!(cache_path(version).is_none());
        }
    }

    #[test]
    fn managed_paths_round_trip_for_new_and_legacy_releases() {
        for version in ["7.2.12", "7.2.2-vsock1"] {
            let path = cache_path(version).unwrap();
            assert_eq!(version_for_path(&path).as_deref(), Some(version));
        }
        assert!(version_for_path(Path::new("kernel/vmlinux-7.2.12-x86_64")).is_none());
    }

    #[tokio::test]
    async fn disabled_source_lists_only_local_kernels_and_fails_a_new_build() {
        let directory = tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(directory.path(), std::iter::empty()).unwrap();
        let tracker = ImageInstallTracker::disabled();
        assert!(
            list(directory.path(), &templates, &tracker)
                .await
                .unwrap()
                .is_empty()
        );
        tracker.begin_with("7.2.12", "test install").unwrap();
        run_install(tracker.clone(), templates, "7.2.12".to_owned(), None).await;
        let result = snapshot(&tracker, "7.2.12");
        assert_eq!(
            result.status,
            firecrab_api_types::ImageInstallStatus::Failed
        );
        assert!(result.log.contains("kernel source downloads are disabled"));
    }

    #[tokio::test]
    async fn unavailable_upstream_is_reported_when_no_local_catalog_exists() {
        let directory = tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(directory.path(), std::iter::empty()).unwrap();
        let downloads = ImageInstallTracker::with_base_url("http://127.0.0.1:1");
        assert!(
            list(directory.path(), &templates, &downloads)
                .await
                .is_err()
        );
    }
}

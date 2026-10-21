//! Independent catalog and cache management for digest-pinned guest kernels.
//!
//! M2Image rootfs files stay immutable and are registered separately from this
//! cache. An image may reference one of these kernels, but installing or
//! removing a kernel does not by itself change any image alias.

use std::path::{Path, PathBuf};

use firecrab_api_types::{KernelInstallResponse, KernelResponse};

use crate::image_install::{Architecture, ImageInstallTracker};
use crate::oci::kernel;
use crate::templates::TemplateRegistry;

/// Builds the host-architecture catalog with verified local cache state.
pub async fn list(
    image_root: &Path,
    templates: &TemplateRegistry,
    downloads: &ImageInstallTracker,
) -> Vec<KernelResponse> {
    let base_url = downloads.base_url();
    let mut result = Vec::new();
    for pinned in kernel::supported_kernels(Architecture::HOST) {
        let installed =
            kernel::cached_kernel_is_valid(image_root, Architecture::HOST, pinned.version).await;
        let relative = kernel::cache_relative(Architecture::HOST, &pinned);
        let size_bytes = if installed {
            tokio::fs::metadata(image_root.join(&relative))
                .await
                .ok()
                .map(|metadata| metadata.len())
        } else {
            None
        };
        let in_use = templates
            .list_aliases()
            .into_iter()
            .any(|template| template.kernel.relative_path() == relative);
        let package_url = base_url.map(|base| {
            format!(
                "{}/{}",
                base.trim_end_matches('/'),
                kernel::package_key(Architecture::HOST, &pinned)
            )
        });
        result.push(KernelResponse {
            version: pinned.version.to_owned(),
            architecture: Architecture::HOST.as_str().to_owned(),
            image: pinned.image.to_owned(),
            image_sha256: digest_body(pinned.image_digest),
            package_sha256: digest_body(pinned.package_digest),
            size_bytes,
            installed,
            in_use,
            package_url,
        });
    }
    result
}

/// Relative cache path for a catalog version, or `None` when it is unknown.
pub fn cache_path(version: &str) -> Option<PathBuf> {
    let pinned = kernel::kernel_for_version(Architecture::HOST, version)?;
    Some(kernel::cache_relative(Architecture::HOST, &pinned))
}

/// Managed kernel release encoded by an image's relative kernel path.
pub fn version_for_path(path: &Path) -> Option<String> {
    kernel::supported_kernels(Architecture::HOST)
        .into_iter()
        .find(|pinned| kernel::cache_relative(Architecture::HOST, pinned) == path)
        .map(|pinned| pinned.version.to_owned())
}

/// Starts the actual download/verify operation for one kernel job.
pub async fn run_install(
    tracker: ImageInstallTracker,
    templates: TemplateRegistry,
    version: String,
    base_url: Option<String>,
) {
    tracker.append_log(
        &version,
        format!("resolving digest-pinned Linux kernel {}", version),
    );
    let result = kernel::ensure_managed_kernel(
        templates.image_root_path(),
        Architecture::HOST,
        &version,
        base_url.as_deref(),
    )
    .await;
    match result {
        Ok(path) => {
            tracker.append_log(&version, format!("verified and cached {}", path.display()));
            tracker.finish_ok_with(&version, "kernel ready — digest verified");
        }
        Err(error) => tracker.finish_err_with(&version, format!("kernel install failed: {error}")),
    }
}

/// Converts the shared job tracker wire shape to the kernel-specific shape.
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

fn digest_body(value: &str) -> String {
    value.strip_prefix("sha256:").unwrap_or(value).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_install::ImageInstallTracker;
    use crate::templates::TemplateRegistry;
    use tempfile::tempdir;

    #[test]
    fn cache_path_only_accepts_catalog_versions() {
        assert!(cache_path("7.2.2").is_some());
        assert!(cache_path("not-a-kernel").is_none());
    }

    #[test]
    fn managed_path_round_trips_to_its_version() {
        let path = cache_path("7.2.2").expect("catalog kernel");
        assert_eq!(version_for_path(&path).as_deref(), Some("7.2.2"));
        assert!(version_for_path(Path::new("kernel/vmlinux-7.2.2-x86_64")).is_none());
    }

    #[tokio::test]
    async fn list_reports_catalog_versions_and_remote_package_urls() {
        let directory = tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(directory.path(), std::iter::empty()).unwrap();
        let downloads = ImageInstallTracker::with_base_url("https://mirror.example/");

        let kernels = list(directory.path(), &templates, &downloads).await;

        assert_eq!(kernels.len(), 4);
        assert_eq!(kernels[0].version, "7.2.2");
        assert_eq!(kernels[1].version, "7.1.12");
        assert_eq!(kernels[2].version, "7.1.9");
        assert_eq!(kernels[3].version, "7.1.8");
        assert!(kernels.iter().all(|kernel| {
            kernel.architecture == Architecture::HOST.as_str()
                && !kernel.installed
                && !kernel.in_use
                && kernel.size_bytes.is_none()
                && kernel
                    .package_url
                    .as_deref()
                    .is_some_and(|url| url.starts_with("https://mirror.example/kernel/"))
        }));
    }

    #[tokio::test]
    async fn run_install_marks_a_disabled_remote_as_failed() {
        let directory = tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(directory.path(), std::iter::empty()).unwrap();
        let tracker = ImageInstallTracker::disabled();
        tracker.begin_with("7.2.2", "test install").unwrap();

        run_install(tracker.clone(), templates, "7.2.2".to_owned(), None).await;

        let snapshot = snapshot(&tracker, "7.2.2");
        assert_eq!(
            snapshot.status,
            firecrab_api_types::ImageInstallStatus::Failed
        );
        assert!(snapshot.log.contains("kernel install failed"));
        assert!(snapshot.ended_at_ms.is_some());
    }
}

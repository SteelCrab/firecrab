use super::kernel;
use super::*;
use axum::{Router, routing::get};
use tempfile::tempdir;

fn kernel_bytes(architecture: Architecture) -> Vec<u8> {
    let mut bytes = vec![0; 4096];
    match architecture {
        Architecture::X86_64 => {
            bytes[..4].copy_from_slice(b"\x7fELF");
            bytes[4] = 2;
            bytes[5] = 1;
            bytes[6] = 1;
            bytes[16..18].copy_from_slice(&2_u16.to_le_bytes());
            bytes[18..20].copy_from_slice(&0x3e_u16.to_le_bytes());
        }
        Architecture::Aarch64 => {
            bytes[..2].copy_from_slice(b"MZ");
            bytes[56..60].copy_from_slice(&0x644d_5241_u32.to_le_bytes());
        }
    }
    bytes
}

pub(crate) async fn install_fixture_kernel(root: &Path, architecture: Architecture, version: &str) {
    let bytes = kernel_bytes(architecture);
    let relative = kernel::cache_relative(architecture, version).unwrap();
    let path = root.join(relative);
    tokio::fs::create_dir_all(path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&path, &bytes).await.unwrap();
    let receipt = kernel::BuiltKernel {
        version: version.to_owned(),
        architecture: architecture.as_str().to_owned(),
        source_url: kernel::source_url(version).unwrap(),
        source_sha256: "a".repeat(64),
        image_sha256: Sha256Digest::of_bytes(&bytes).encoded().to_owned(),
        build_revision: "firecrab-host1".to_owned(),
    };
    tokio::fs::write(
        path.with_added_extension("json"),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .await
    .unwrap();
}

fn releases(version: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"releases": [
        {"moniker": "mainline", "version": "9.0-rc1", "source": "https://example.com/rc.tar.xz"},
        {"moniker": "stable", "version": version, "iseol": false, "source": kernel::source_url(version)},
        {"moniker": "longterm", "version": "6.18.1"}
    ]})).unwrap()
}

async fn fixture_server(bytes: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
    let router = Router::new().route(
        "/",
        get(move || {
            let bytes = bytes.clone();
            async move { bytes }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (url, task)
}

#[test]
fn stable_selection_rejects_release_candidates_and_unexpected_source_hosts() {
    assert_eq!(
        kernel::parse_stable_release(&releases("7.2.12"))
            .unwrap()
            .version,
        "7.2.12"
    );
    for value in [
        serde_json::json!({"releases": [{"moniker": "mainline", "version": "7.3-rc1"}]}),
        serde_json::json!({"releases": [{"moniker": "stable", "version": "7.3-rc1", "source": "https://example.com"}]}),
        serde_json::json!({"releases": [{"moniker": "stable", "version": "7.2.12", "source": "https://example.com/linux-7.2.12.tar.xz"}]}),
        serde_json::json!({"releases": [{"moniker": "stable", "version": "7.2.12", "iseol": true}]}),
    ] {
        assert!(kernel::parse_stable_release(&serde_json::to_vec(&value).unwrap()).is_err());
    }
}

#[test]
fn recent_versions_use_published_releases_and_numeric_order() {
    let digest = "a".repeat(64);
    let sums = format!(
        "{digest}  linux-7.1.9.tar.xz\n{digest}  linux-7.2.tar.xz\n{digest}  linux-7.2.1.tar.xz\n{digest}  linux-7.2.10.tar.xz\n{digest}  linux-7.3-rc1.tar.xz\ninvalid  linux-7.2.2.tar.xz\n"
    );
    assert_eq!(
        kernel::parse_recent_versions(sums.as_bytes(), "7.2.1").unwrap(),
        ["7.2.1", "7.2", "7.1.9"]
    );
    assert!(kernel::parse_recent_versions(sums.as_bytes(), "7.2.2").is_err());
}

#[test]
fn source_checksum_matches_the_exact_archive_name() {
    let digest = "a".repeat(64);
    let sums = format!(
        "-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA256\n\n{}  linux-7.2.1.tar.xz\n{digest}  linux-7.2.12.tar.xz\n",
        "b".repeat(64)
    );
    assert_eq!(
        kernel::source_checksum(sums.as_bytes(), "7.2.12")
            .unwrap()
            .encoded(),
        digest
    );
    assert!(kernel::source_checksum(sums.as_bytes(), "7.2.2").is_err());
    assert!(kernel::source_checksum(b"oops linux-7.2.12.tar.xz", "7.2.12").is_err());
}

#[test]
fn cache_paths_reject_traversal_and_recognize_both_architectures() {
    for architecture in [Architecture::X86_64, Architecture::Aarch64] {
        for version in ["7.2.12", "7.3", "7.2.2-vsock1"] {
            let relative = kernel::cache_relative(architecture, version).unwrap();
            assert_eq!(
                kernel::version_for_path(architecture, &relative).as_deref(),
                Some(version)
            );
        }
        for version in [
            "../7.2.12",
            "7.2.12/other",
            "7.3-rc1",
            "007.2.12",
            "7.2.12\n",
            "",
            "stable",
        ] {
            assert!(kernel::cache_relative(architecture, version).is_none());
        }
    }
}

#[tokio::test]
async fn stable_metadata_is_cached_and_can_be_used_offline() {
    let directory = tempdir().unwrap();
    let (url, server) = fixture_server(releases("7.2.12")).await;
    let stable = kernel::latest_stable(directory.path(), Some(&url))
        .await
        .unwrap();
    server.abort();
    assert_eq!(
        kernel::latest_stable(directory.path(), Some(&url))
            .await
            .unwrap(),
        stable
    );
    assert_eq!(
        kernel::latest_stable(directory.path(), None).await.unwrap(),
        stable
    );
    filetime::set_file_mtime(
        directory.path().join(".oci/kernel/releases.json"),
        filetime::FileTime::from_unix_time(1, 0),
    )
    .unwrap();
    assert_eq!(
        kernel::latest_stable(directory.path(), Some("http://127.0.0.1:1"))
            .await
            .unwrap(),
        stable
    );
    let (new_url, new_server) = fixture_server(releases("7.2.13")).await;
    assert_eq!(
        kernel::latest_stable(directory.path(), Some(&new_url))
            .await
            .unwrap()
            .version,
        "7.2.13"
    );
    new_server.abort();
}

#[tokio::test]
async fn kernel_catalog_reports_current_stable_and_independent_local_versions() {
    let directory = tempdir().unwrap();
    let root = directory.path();
    for version in ["7.2.2", "7.2.12"] {
        install_fixture_kernel(root, Architecture::HOST, version).await;
    }
    let (url, server) = fixture_server(releases("7.2.13")).await;
    let cache = root.join(".oci/kernel");
    tokio::fs::create_dir_all(&cache).await.unwrap();
    let digest = "a".repeat(64);
    let sums = ["7.2.13", "7.2.12", "7.2.11", "7.2.10"]
        .iter()
        .map(|version| format!("{digest}  linux-{version}.tar.xz\n"))
        .collect::<String>();
    tokio::fs::write(cache.join("sha256sums-v7.asc"), sums)
        .await
        .unwrap();
    let templates = TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
    let downloads = ImageInstallTracker::with_base_url(&url);
    let catalog = crate::kernel_manager::list(root, &templates, &downloads)
        .await
        .unwrap();
    assert_eq!(
        catalog
            .iter()
            .map(|entry| entry.version.as_str())
            .collect::<Vec<_>>(),
        ["7.2.13", "7.2.12", "7.2.11", "7.2.2"]
    );
    assert!(!catalog[0].installed);
    assert!(catalog[0].image_sha256.is_empty());
    assert!(
        [1, 3]
            .into_iter()
            .all(|index| catalog[index].installed && catalog[index].image_sha256.len() == 64)
    );
    assert!(catalog.iter().all(|entry| {
        entry
            .package_url
            .as_ref()
            .unwrap()
            .starts_with("https://cdn.kernel.org/")
    }));
    server.abort();
}

#[tokio::test]
async fn source_download_rejects_corruption_and_removes_partial_files() {
    let directory = tempdir().unwrap();
    let bytes = b"verified upstream source".to_vec();
    let (url, server) = fixture_server(bytes.clone()).await;
    let destination = directory.path().join("linux.tar.xz");
    let expected = Sha256Digest::of_bytes(&bytes);
    kernel::download_source(&url, &destination, &expected, None)
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(&destination).await.unwrap(), bytes);
    tokio::fs::write(&destination, b"corrupted source")
        .await
        .unwrap();
    let wrong = Sha256Digest::of_bytes(b"different source");
    assert!(matches!(
        kernel::download_source(&url, &destination, &wrong, None).await,
        Err(ResolveError::DigestMismatch { .. })
    ));
    assert_eq!(
        tokio::fs::read_dir(directory.path())
            .await
            .unwrap()
            .next_entry()
            .await
            .unwrap()
            .unwrap()
            .file_name(),
        "linux.tar.xz"
    );
    kernel::download_source(&url, &destination, &expected, None)
        .await
        .unwrap();
    server.abort();
    kernel::download_source(&url, &destination, &expected, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn cached_builds_verify_digest_and_architecture_before_offline_reuse() {
    let root = tempdir().unwrap();
    for architecture in [Architecture::X86_64, Architecture::Aarch64] {
        install_fixture_kernel(root.path(), architecture, "7.2.12").await;
        assert!(kernel::cached_kernel_is_valid(root.path(), architecture, "7.2.12").await);
        let path = kernel::ensure_managed_kernel(root.path(), architecture, "7.2.12", None, None)
            .await
            .unwrap();
        assert_eq!(
            path,
            kernel::cache_relative(architecture, "7.2.12").unwrap()
        );
        tokio::fs::write(root.path().join(&path), b"corrupt")
            .await
            .unwrap();
        assert!(!kernel::cached_kernel_is_valid(root.path(), architecture, "7.2.12").await);
        assert!(
            kernel::ensure_managed_kernel(root.path(), architecture, "7.2.12", None, None)
                .await
                .is_err()
        );
        install_fixture_kernel(root.path(), architecture, "7.2.12").await;
        tokio::fs::write(root.path().join(path), kernel_bytes(architecture.other()))
            .await
            .unwrap();
        assert!(!kernel::cached_kernel_is_valid(root.path(), architecture, "7.2.12").await);
    }
}

#[tokio::test]
async fn cached_builds_reject_receipts_for_other_sources_or_architectures() {
    let root = tempdir().unwrap();
    let architecture = Architecture::HOST;
    install_fixture_kernel(root.path(), architecture, "7.2.12").await;
    let receipt_path = root
        .path()
        .join(kernel::cache_relative(architecture, "7.2.12").unwrap())
        .with_added_extension("json");
    let original = tokio::fs::read(&receipt_path).await.unwrap();
    for (field, value) in [
        ("architecture", architecture.other().as_str()),
        ("version", "7.2.1"),
        ("sourceUrl", "bad"),
    ] {
        let mut receipt: serde_json::Value = serde_json::from_slice(&original).unwrap();
        // Receipts use Rust field names; API responses use camelCase separately.
        receipt[if field == "sourceUrl" {
            "source_url"
        } else {
            field
        }] = value.into();
        tokio::fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap())
            .await
            .unwrap();
        assert!(!kernel::cached_kernel_is_valid(root.path(), architecture, "7.2.12").await);
    }
}

#[tokio::test]
async fn oci_import_reuses_latest_installed_kernel_when_upstream_is_down() {
    let root = tempdir().unwrap();
    for version in ["7.2.2", "7.2.12"] {
        install_fixture_kernel(root.path(), Architecture::HOST, version).await;
    }
    for version in ["7.2.2", "7.2.12"] {
        assert!(kernel::cached_kernel_is_valid(root.path(), Architecture::HOST, version).await);
    }
    let pair = kernel::ensure_host_kernel(
        root.path(),
        Architecture::HOST,
        None,
        Some("http://127.0.0.1:1"),
    )
    .await
    .unwrap();
    assert_eq!(
        pair.kernel,
        kernel::cache_relative(Architecture::HOST, "7.2.12").unwrap()
    );
}

#[tokio::test]
async fn operator_kernel_is_verified_and_works_without_source_downloads() {
    let root = tempdir().unwrap();
    let supplied = root.path().join("operator-kernel");
    tokio::fs::write(&supplied, kernel_bytes(Architecture::HOST))
        .await
        .unwrap();
    let pair = kernel::ensure_host_kernel(root.path(), Architecture::HOST, Some(&supplied), None)
        .await
        .unwrap();
    assert!(root.path().join(pair.kernel).is_file());
    tokio::fs::write(&supplied, kernel_bytes(Architecture::HOST.other()))
        .await
        .unwrap();
    assert!(matches!(
        kernel::ensure_host_kernel(root.path(), Architecture::HOST, Some(&supplied), None).await,
        Err(ResolveError::KernelArchitectureMismatch { .. })
    ));
}

#[tokio::test]
async fn cached_kernel_rejects_symlinks() {
    let root = tempdir().unwrap();
    let architecture = Architecture::HOST;
    install_fixture_kernel(root.path(), architecture, "7.2.12").await;
    let path = root
        .path()
        .join(kernel::cache_relative(architecture, "7.2.12").unwrap());
    let target = root.path().join("other");
    tokio::fs::rename(&path, &target).await.unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(!kernel::cached_kernel_is_valid(root.path(), architecture, "7.2.12").await);
}

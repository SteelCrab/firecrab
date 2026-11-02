//! Verified, prebuilt guest kernels. Installation never starts a compiler.
use super::*;
use std::io::{Read, Write};

pub(crate) const DEFAULT_BASE_URL: &str = "https://registry.firecrab.dev";
const CONFIG_MAX_BYTES: u64 = 1024 * 1024;
const REQUIRED: &[&str] = &[
    "BLOCK",
    "DEVTMPFS",
    "DEVTMPFS_MOUNT",
    "EXT4_FS",
    "NET",
    "INET",
    "UNIX",
    "PACKET",
    "VIRTIO_BLK",
    "VIRTIO_NET",
    "VIRTIO_MMIO",
    "VIRTIO_MMIO_CMDLINE_DEVICES",
    "VSOCKETS",
    "VIRTIO_VSOCKETS",
    "SERIAL_8250_CONSOLE",
    "UNIX98_PTYS",
    "BINFMT_ELF",
    "BINFMT_SCRIPT",
    "PROC_FS",
    "SYSFS",
    "TMPFS",
];

#[derive(Clone, Debug)]
pub(crate) struct Package {
    pub version: String,
    pub architecture: Architecture,
    pub revision: u64,
    pub url: String,
    pub sha256: Sha256Digest,
    pub source_sha256: String,
    pub published_at: Option<String>,
    pub end_of_life: Option<bool>,
}

pub(crate) fn parse(bytes: &[u8], base: &str) -> Result<Vec<Package>, ResolveError> {
    let invalid = || unavailable(Architecture::HOST, "invalid kernel registry catalog");
    let catalog: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if catalog["schemaVersion"] != 1 {
        return Err(invalid());
    }
    let base =
        reqwest::Url::parse(&format!("{}/", base.trim_end_matches('/'))).map_err(|_| invalid())?;
    if !matches!(base.scheme(), "https" | "http")
        || !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
    {
        return Err(invalid());
    }
    let mut packages: Vec<Package> = Vec::new();
    for entry in catalog["kernels"].as_array().ok_or_else(invalid)? {
        let version = entry["version"]
            .as_str()
            .filter(|v| is_source_version(v))
            .ok_or_else(invalid)?;
        let architecture = match entry["architecture"].as_str() {
            Some("aarch64") => Architecture::Aarch64,
            Some("x86_64") => Architecture::X86_64,
            _ => continue,
        };
        let image = image_name(architecture, version).ok_or_else(invalid)?;
        let format = if architecture == Architecture::Aarch64 {
            "arm64-image"
        } else {
            "elf"
        };
        let key = entry["package"].as_str().ok_or_else(invalid)?;
        if entry["alias"].as_str() != Some(format!("vmlinux-{version}").as_str())
            || entry["image"].as_str() != Some(image.as_str())
            || entry["format"].as_str() != Some(format)
            || !key.starts_with("kernel/")
            || !key.ends_with(".tar.zst")
            || key.split('/').any(|part| {
                part.is_empty()
                    || part == "."
                    || part == ".."
                    || !part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
            })
        {
            return Err(invalid());
        }
        let revision = entry["revision"]
            .as_u64()
            .filter(|revision| *revision > 0)
            .ok_or_else(invalid)?;
        let digest = |field: &serde_json::Value| -> Result<Sha256Digest, ResolveError> {
            let value = field.as_str().ok_or_else(invalid)?;
            Sha256Digest::parse(&format!("sha256:{value}")).map_err(|_| invalid())
        };
        let sha256 = digest(&entry["sha256"])?;
        let source_sha256 = digest(&entry["source"]["sha256"])?;
        if entry["source"]["url"].as_str() != source_url(version).as_deref() {
            return Err(invalid());
        }
        let package = Package {
            version: version.to_owned(),
            architecture,
            revision,
            url: base.join(key).map_err(|_| invalid())?.to_string(),
            sha256,
            source_sha256: source_sha256.encoded().to_owned(),
            published_at: entry["publishedAt"]
                .as_str()
                .filter(|value| chrono::DateTime::parse_from_rfc3339(value).is_ok())
                .map(str::to_owned),
            end_of_life: entry["iseol"].as_bool(),
        };
        if let Some(existing) = packages
            .iter_mut()
            .find(|old| old.version == version && old.architecture == architecture)
        {
            if existing.revision == revision
                && (existing.sha256 != package.sha256 || existing.url != package.url)
            {
                return Err(invalid());
            }
            if existing.revision < revision {
                *existing = package;
            }
        } else {
            packages.push(package);
        }
    }
    packages.sort_by_key(|package| std::cmp::Reverse(version_key(&package.version)));
    Ok(packages)
}

/// Cache is bound to the registry URL. A network outage can reuse verified metadata.
pub(crate) async fn catalog(root: &Path, base: Option<&str>) -> Result<Vec<Package>, ResolveError> {
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Cached {
        base: String,
        catalog: Vec<u8>,
    }
    let path = root.join(".oci/kernel/registry-catalog.json");
    let cached = tokio::fs::read(&path)
        .await
        .ok()
        .filter(|bytes| bytes.len() <= METADATA_MAX_BYTES as usize * 5)
        .and_then(|bytes| serde_json::from_slice::<Cached>(&bytes).ok());
    let usable = cached
        .as_ref()
        .filter(|cached| base.is_none_or(|base| base.trim_end_matches('/') == cached.base));
    if let Some(cached) = usable {
        let fresh = tokio::fs::metadata(&path)
            .await
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|time| time.elapsed().ok())
            .is_some_and(|age| age < RELEASE_CACHE_TTL);
        if fresh || base.is_none() {
            return parse(&cached.catalog, &cached.base);
        }
    }
    let Some(base) = base else {
        return Ok(Vec::new());
    };
    let url = format!("{}/kernel/catalog.json", base.trim_end_matches('/'));
    match fetch_bytes(&url, METADATA_MAX_BYTES).await {
        Ok(bytes) => {
            let packages = parse(&bytes, base)?;
            tokio::fs::create_dir_all(path.parent().unwrap())
                .await
                .map_err(|source| cache_io("create registry cache", path.clone(), source))?;
            write_atomic(
                &path,
                &serde_json::to_vec(&Cached {
                    base: base.trim_end_matches('/').to_owned(),
                    catalog: bytes,
                })
                .expect("serialize catalog cache"),
            )
            .await?;
            Ok(packages)
        }
        Err(error) => match usable {
            Some(cached) => parse(&cached.catalog, &cached.base),
            None => Err(error),
        },
    }
}

fn verify_config(config: &str, architecture: Architecture) -> Result<(), ResolveError> {
    let mut required = REQUIRED.to_vec();
    if architecture == Architecture::Aarch64 {
        required.extend(["ARM64_4K_PAGES", "EFI", "EFI_STUB"]);
    }
    let missing = required
        .into_iter()
        .filter(|symbol| {
            !config
                .lines()
                .any(|line| line == format!("CONFIG_{symbol}=y"))
        })
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(unavailable(
            architecture,
            format!(
                "registry kernel lacks built-in VM runtime drivers: {}",
                missing.join(", ")
            ),
        ))
    }
}

fn extract(
    archive: &Path,
    work: &Path,
    package: &Package,
    cancellation: Cancellation,
) -> Result<(), ResolveError> {
    let image = image_name(package.architecture, &package.version).expect("validated image name");
    let file = std::fs::File::open(archive)
        .map_err(|source| cache_io("open kernel package", archive.to_owned(), source))?;
    let decoder = zstd::stream::read::Decoder::new(file)
        .map_err(|source| cache_io("decode kernel package", archive.to_owned(), source))?;
    let mut tar =
        tar::Archive::new(decoder.take(KERNEL_MAX_BYTES + CONFIG_MAX_BYTES + 1024 * 1024));
    let mut seen = [false; 2];
    for entry in tar
        .entries()
        .map_err(|source| cache_io("read kernel package", archive.to_owned(), source))?
    {
        check_cancelled(&cancellation)?;
        let mut entry = entry
            .map_err(|source| cache_io("read kernel package entry", archive.to_owned(), source))?;
        let path = entry
            .path()
            .map_err(|source| cache_io("read kernel package path", archive.to_owned(), source))?
            .into_owned();
        if entry.header().entry_type().is_dir()
            && matches!(
                path.to_str(),
                Some("kernel" | "kernel/" | "config" | "config/")
            )
        {
            continue;
        }
        let index = if path == Path::new(&format!("kernel/{image}")) {
            0
        } else if path == Path::new(&format!("config/{image}.config")) {
            1
        } else {
            return Err(unavailable(
                package.architecture,
                "unexpected path in kernel package",
            ));
        };
        let maximum = if index == 0 {
            KERNEL_MAX_BYTES
        } else {
            CONFIG_MAX_BYTES
        };
        if seen[index] || !entry.header().entry_type().is_file() || entry.size() > maximum {
            return Err(unavailable(
                package.architecture,
                "invalid kernel package entry",
            ));
        }
        let target = work.join(if index == 0 {
            "kernel"
        } else {
            "kernel.config"
        });
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|source| cache_io("stage kernel package entry", target.clone(), source))?;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            check_cancelled(&cancellation)?;
            let read = entry
                .read(&mut buffer)
                .map_err(|source| cache_io("extract kernel package", target.clone(), source))?;
            if read == 0 {
                break;
            }
            output
                .write_all(&buffer[..read])
                .map_err(|source| cache_io("write extracted kernel", target.clone(), source))?;
        }
        seen[index] = true;
    }
    if !seen.into_iter().all(|seen| seen) {
        return Err(unavailable(
            package.architecture,
            "kernel package must include an image and runtime config",
        ));
    }
    let config = std::fs::read_to_string(work.join("kernel.config"))
        .map_err(|source| cache_io("read runtime config", work.to_owned(), source))?;
    verify_config(&config, package.architecture)
}

pub(crate) async fn install(
    root: &Path,
    architecture: Architecture,
    requested: &str,
    base: Option<&str>,
    tracker: Option<&ImageInstallTracker>,
) -> Result<PathBuf, ResolveError> {
    let mut cancellation = tracker.and_then(|tracker| tracker.cancellation(requested));
    check_cancelled(&cancellation)?;
    // Existing native receipts and legacy pins remain usable without network access.
    let mut packages = Vec::new();
    let version = if requested == "stable" {
        packages = until_cancelled(&mut cancellation, catalog(root, base)).await?;
        packages
            .iter()
            .find(|package| {
                package.architecture == architecture && package.end_of_life != Some(true)
            })
            .or_else(|| {
                packages
                    .iter()
                    .find(|package| package.architecture == architecture)
            })
            .ok_or_else(|| {
                unavailable(
                    architecture,
                    "registry contains no kernel for this architecture",
                )
            })?
            .version
            .clone()
    } else {
        requested.to_owned()
    };
    let relative = cache_relative(architecture, &version)
        .ok_or_else(|| unavailable(architecture, "invalid kernel version"))?;
    let lock = cache_lock(root, architecture, &version).await?;
    let _guard = until_cancelled(&mut cancellation, async { Ok(lock.lock().await) }).await?;
    if cached_kernel_is_valid(root, architecture, &version).await {
        return Ok(relative);
    }
    if base.is_none() {
        return Err(unavailable(
            architecture,
            "kernel registry downloads are disabled",
        ));
    }
    if packages.is_empty() {
        packages = until_cancelled(&mut cancellation, catalog(root, base)).await?;
    }
    let package = packages
        .into_iter()
        .find(|package| package.version == version && package.architecture == architecture)
        .ok_or_else(|| {
            unavailable(
                architecture,
                format!("kernel {version} has no published registry package"),
            )
        })?;
    let cache = root
        .join(".oci/kernel/packages")
        .join(architecture.as_str());
    tokio::fs::create_dir_all(&cache)
        .await
        .map_err(|source| cache_io("create kernel package cache", cache.clone(), source))?;
    let archive = cache.join(format!("vmlinux-{version}-r{}.tar.zst", package.revision));
    log(
        tracker,
        requested,
        format!("downloading kernel package {}", package.url),
    );
    download_source(
        &package.url,
        &archive,
        &package.sha256,
        tracker.map(|tracker| (tracker, requested)),
    )
    .await?;
    let destination = root.join(&relative);
    let work = destination
        .parent()
        .unwrap()
        .join(format!(".package-{version}-{}", Uuid::new_v4()));
    tokio::fs::create_dir(&work)
        .await
        .map_err(|source| cache_io("create kernel extraction directory", work.clone(), source))?;
    let result = async {
        log(tracker, requested, "extracting kernel package".to_owned());
        let (archive, extraction_work, extraction_package, signal) = (
            archive.clone(),
            work.clone(),
            package.clone(),
            cancellation.clone(),
        );
        // Always join the extraction worker before cleaning its files on cancellation.
        tokio::task::spawn_blocking(move || {
            extract(&archive, &extraction_work, &extraction_package, signal)
        })
        .await
        .map_err(|error| unavailable(architecture, error.to_string()))??;
        check_cancelled(&cancellation)?;
        log(tracker, requested, "verifying kernel package".to_owned());
        let digest = inspect_kernel(&work.join("kernel"), architecture).await?;
        let receipt = BuiltKernel {
            version: version.clone(),
            architecture: architecture.as_str().to_owned(),
            source_url: source_url(&version).expect("validated version"),
            source_sha256: package.source_sha256,
            image_sha256: digest.encoded().to_owned(),
            build_revision: BUILD_REVISION.to_owned(),
            package_url: Some(package.url),
            package_sha256: Some(package.sha256.encoded().to_owned()),
        };
        check_cancelled(&cancellation)?;
        log(tracker, requested, "installing kernel package".to_owned());
        for (source, target) in [
            (
                work.join("kernel.config"),
                destination.with_added_extension("config"),
            ),
            (work.join("kernel"), destination.clone()),
        ] {
            tokio::fs::rename(source, &target)
                .await
                .map_err(|source| cache_io("publish registry kernel", target, source))?;
        }
        write_atomic(
            &destination.with_added_extension("json"),
            &serde_json::to_vec_pretty(&receipt).expect("serialize receipt"),
        )
        .await?;
        Ok(relative)
    }
    .await;
    if let Err(source) = tokio::fs::remove_dir_all(&work).await
        && source.kind() != io::ErrorKind::NotFound
    {
        return Err(cache_io("remove kernel extraction directory", work, source));
    }
    result
}

pub(crate) async fn remove_packages(
    root: &Path,
    architecture: Architecture,
    version: &str,
) -> Result<bool, ResolveError> {
    let directory = root
        .join(".oci/kernel/packages")
        .join(architecture.as_str());
    let Ok(mut entries) = tokio::fs::read_dir(&directory).await else {
        return Ok(false);
    };
    let prefix = format!("vmlinux-{version}-r");
    let mut removed = false;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|source| cache_io("list kernel packages", directory.clone(), source))?
    {
        let name = entry.file_name();
        let Some(name) = name.to_str().and_then(|name| name.strip_prefix(&prefix)) else {
            continue;
        };
        let Some((revision, suffix)) = name.split_once(".tar.") else {
            continue;
        };
        if revision.parse::<u64>().is_err()
            || !(suffix == "zst"
                || suffix
                    .strip_suffix("-partial")
                    .is_some_and(|id| Uuid::parse_str(id).is_ok()))
        {
            continue;
        }
        tokio::fs::remove_file(entry.path())
            .await
            .map_err(|source| cache_io("remove cached kernel package", entry.path(), source))?;
        removed = true;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use serde_json::json;

    fn entry(version: &str, architecture: Architecture) -> serde_json::Value {
        json!({"alias": format!("vmlinux-{version}"), "version":version,"architecture":architecture.as_str(),"revision":2,"package":format!("kernel/{version}/r2/{}/vmlinux-{version}.tar.zst",architecture.as_str()),"sha256":"a".repeat(64),"image":image_name(architecture,version).unwrap(),"format":if architecture == Architecture::Aarch64 {"arm64-image"} else {"elf"},"source":{"url":source_url(version).unwrap(),"sha256":"b".repeat(64)},"publishedAt":"2026-11-02T00:00:00Z","iseol":false})
    }
    fn document(entries: Vec<serde_json::Value>) -> Vec<u8> {
        serde_json::to_vec(&json!({"schemaVersion":1,"kernels":entries})).unwrap()
    }
    fn config() -> String {
        REQUIRED
            .iter()
            .chain(["ARM64_4K_PAGES", "EFI", "EFI_STUB"].iter())
            .map(|symbol| format!("CONFIG_{symbol}=y\n"))
            .collect()
    }
    fn package_bytes(config: &str, symlink: bool) -> Vec<u8> {
        let mut image = vec![0; 4096];
        match Architecture::HOST {
            Architecture::Aarch64 => {
                image[..2].copy_from_slice(b"MZ");
                image[56..60].copy_from_slice(&0x644d_5241_u32.to_le_bytes());
            }
            Architecture::X86_64 => {
                image[..4].copy_from_slice(b"\x7fELF");
                image[4] = 2;
                image[5] = 1;
                image[6] = 1;
                image[16..18].copy_from_slice(&2u16.to_le_bytes());
                image[18..20].copy_from_slice(&0x3eu16.to_le_bytes());
            }
        }
        let image_name = image_name(Architecture::HOST, "7.2.15").unwrap();
        let mut archive = tar::Builder::new(Vec::new());
        for (path, bytes) in [
            (format!("kernel/{image_name}"), image),
            (
                format!("config/{image_name}.config"),
                config.as_bytes().to_vec(),
            ),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            header.set_size(bytes.len() as u64);
            if symlink && path.starts_with("kernel/") {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_link_name("/tmp/escape").unwrap();
                header.set_size(0);
                header.set_cksum();
                archive
                    .append_data(&mut header, path, std::io::empty())
                    .unwrap();
            } else {
                header.set_cksum();
                archive
                    .append_data(&mut header, path, bytes.as_slice())
                    .unwrap();
            }
        }
        zstd::stream::encode_all(archive.into_inner().unwrap().as_slice(), 1).unwrap()
    }
    async fn serve(
        bytes: Vec<u8>,
        corrupt_checksum: bool,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let mut metadata = entry("7.2.15", Architecture::HOST);
        metadata["sha256"] = if corrupt_checksum {
            "0".repeat(64)
        } else {
            Sha256Digest::of_bytes(&bytes).encoded().to_owned()
        }
        .into();
        let path = metadata["package"].as_str().unwrap().to_owned();
        let catalog = document(vec![metadata]);
        let router = Router::new()
            .route(
                "/kernel/catalog.json",
                get(move || {
                    let catalog = catalog.clone();
                    async move { catalog }
                }),
            )
            .route(
                &format!("/{path}"),
                get(move || {
                    let bytes = bytes.clone();
                    async move { bytes }
                }),
            );
        let handle = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (base, handle)
    }
    #[test]
    fn catalog_is_dynamic_architecture_specific_and_includes_eol() {
        let mut eol = entry("7.2.15", Architecture::HOST);
        eol["iseol"] = true.into();
        let entries = vec![
            entry("7.2.14", Architecture::HOST),
            eol,
            entry("7.9.1", Architecture::HOST),
        ];
        let result = parse(&document(entries), DEFAULT_BASE_URL).unwrap();
        assert_eq!(
            result
                .iter()
                .map(|package| package.version.as_str())
                .collect::<Vec<_>>(),
            ["7.9.1", "7.2.15", "7.2.14"]
        );
        assert_eq!(result[1].end_of_life, Some(true));
        assert_eq!(
            result[1].published_at.as_deref(),
            Some("2026-11-02T00:00:00Z")
        );
        assert!(result[0].url.contains("/r2/"));
    }
    #[test]
    fn rejects_path_escapes_foreign_urls_bad_checksums_and_conflicting_revisions() {
        for key in [
            "../escape.tar.zst",
            "https://example.com/kernel.tar.zst",
            "kernel/../escape.tar.zst",
            "kernel/%2e%2e/escape.tar.zst",
            "kernel/x.tar.zst?x=y",
            "kernel//x.tar.zst",
        ] {
            let mut invalid = entry("7.2.15", Architecture::HOST);
            invalid["package"] = key.into();
            assert!(
                parse(&document(vec![invalid]), DEFAULT_BASE_URL).is_err(),
                "{key}"
            );
        }
        let valid = entry("7.2.15", Architecture::HOST);
        let mut invalid = valid.clone();
        invalid["sha256"] = "bad".into();
        assert!(parse(&document(vec![invalid]), DEFAULT_BASE_URL).is_err());
        let mut conflicting = valid.clone();
        conflicting["sha256"] = "c".repeat(64).into();
        assert!(
            parse(
                &document(vec![valid.clone(), conflicting]),
                DEFAULT_BASE_URL
            )
            .is_err()
        );
        let mut newer = valid.clone();
        newer["revision"] = 3.into();
        let selected = parse(&document(vec![newer, valid]), DEFAULT_BASE_URL).unwrap();
        assert_eq!(selected[0].revision, 3);
    }
    #[test]
    fn requires_builtin_terminal_drivers_and_rejects_archive_links() {
        let directory = tempfile::tempdir().unwrap();
        let package = parse(
            &document(vec![entry("7.2.15", Architecture::HOST)]),
            DEFAULT_BASE_URL,
        )
        .unwrap()
        .remove(0);
        let invalid = config().replace("CONFIG_VSOCKETS=y", "CONFIG_VSOCKETS=m");
        assert!(verify_config(&invalid, Architecture::HOST).is_err());
        let archive = directory.path().join("package.tar.zst");
        std::fs::write(&archive, package_bytes(&config(), true)).unwrap();
        assert!(extract(&archive, directory.path(), &package, None).is_err());
        assert!(!directory.path().join("kernel").exists());
    }
    #[tokio::test]
    async fn downloads_prebuilt_kernel_without_a_builder_and_reuses_it_offline() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = package_bytes(&config(), false);
        let digest = Sha256Digest::of_bytes(&bytes);
        let (base, server) = serve(bytes, false).await;
        let relative = install(
            directory.path(),
            Architecture::HOST,
            "7.2.15",
            Some(&base),
            None,
        )
        .await
        .unwrap();
        assert!(cached_kernel_is_valid(directory.path(), Architecture::HOST, "7.2.15").await);
        let receipt = built_kernel(directory.path(), Architecture::HOST, "7.2.15")
            .await
            .unwrap();
        assert_eq!(receipt.package_sha256.as_deref(), Some(digest.encoded()));
        assert!(receipt.package_url.unwrap().starts_with(&base));
        server.abort();
        let _ = server.await;
        assert_eq!(
            install(directory.path(), Architecture::HOST, "7.2.15", None, None)
                .await
                .unwrap(),
            relative
        );
        assert_eq!(catalog(directory.path(), None).await.unwrap().len(), 1);
    }
    #[tokio::test]
    async fn rejects_incompatible_registry_kernel_and_checksum_failure_before_publication() {
        for (config, corrupt) in [
            (
                config().replace("CONFIG_VSOCKETS=y", "# CONFIG_VSOCKETS is not set"),
                false,
            ),
            (config(), true),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let (base, server) = serve(package_bytes(&config, false), corrupt).await;
            let error = install(
                directory.path(),
                Architecture::HOST,
                "7.2.15",
                Some(&base),
                None,
            )
            .await
            .unwrap_err();
            assert!(
                if corrupt {
                    error.to_string().contains("digest")
                } else {
                    error.to_string().contains("VSOCKETS")
                },
                "{error}"
            );
            assert!(
                !directory
                    .path()
                    .join(cache_relative(Architecture::HOST, "7.2.15").unwrap())
                    .exists()
            );
            let parent = directory
                .path()
                .join(".oci/kernel")
                .join(Architecture::HOST.as_str());
            if let Ok(entries) = std::fs::read_dir(parent) {
                assert!(
                    !entries
                        .flatten()
                        .any(|entry| entry.file_name().to_string_lossy().starts_with(".package-"))
                );
            }
            server.abort();
            let _ = server.await;
        }
    }
    #[tokio::test]
    async fn lists_registry_packages_with_package_dates_and_preserves_existing_kernels() {
        let directory = tempfile::tempdir().unwrap();
        let (base, server) = serve(package_bytes(&config(), false), false).await;
        crate::oci::kernel_tests::install_fixture_kernel(
            directory.path(),
            Architecture::HOST,
            "7.1.99",
        )
        .await;
        let templates =
            crate::templates::TemplateRegistry::from_specs(directory.path(), std::iter::empty())
                .unwrap();
        let tracker = ImageInstallTracker::with_base_url(base);
        let list = crate::kernel_manager::list(directory.path(), &templates, &tracker)
            .await
            .unwrap();
        assert_eq!(
            list.iter()
                .map(|kernel| kernel.version.as_str())
                .collect::<Vec<_>>(),
            ["7.2.15", "7.1.99"]
        );
        assert_eq!(
            list[0].published_at.as_deref(),
            Some("2026-11-02T00:00:00Z")
        );
        assert_eq!(list[0].released_at, None);
        assert!(list[1].installed);
        assert_eq!(list[1].published_at, None);
        server.abort();
        let _ = server.await;
    }
    #[tokio::test]
    async fn removing_packages_keeps_other_versions_and_manual_files() {
        let directory = tempfile::tempdir().unwrap();
        let packages = directory
            .path()
            .join(".oci/kernel/packages")
            .join(Architecture::HOST.as_str());
        std::fs::create_dir_all(&packages).unwrap();
        for name in [
            "vmlinux-7.2.15-r2.tar.zst",
            "vmlinux-7.2.15-r3.tar.zst",
            "vmlinux-7.2.14-r2.tar.zst",
            "vmlinux-7.2.15-r2.tar.manual-partial",
        ] {
            std::fs::write(packages.join(name), b"test").unwrap();
        }
        assert!(
            remove_packages(directory.path(), Architecture::HOST, "7.2.15")
                .await
                .unwrap()
        );
        assert!(!packages.join("vmlinux-7.2.15-r2.tar.zst").exists());
        assert!(packages.join("vmlinux-7.2.14-r2.tar.zst").exists());
        assert!(
            packages
                .join("vmlinux-7.2.15-r2.tar.manual-partial")
                .exists()
        );
    }
}

#[cfg(test)]
mod public_registry_smoke {
    use super::*;

    /// Real package verification in an isolated root. Never touches running VMs.
    #[tokio::test]
    #[ignore = "downloads published kernels from registry.firecrab.dev"]
    async fn registry_download_smoke() {
        let directory = tempfile::tempdir().unwrap();
        let packages = catalog(directory.path(), Some(DEFAULT_BASE_URL))
            .await
            .unwrap();
        assert!(!packages.is_empty());
        for package in packages {
            assert!(
                package.revision >= 2,
                "runtime-compatible package revision required"
            );
            let path = install(
                directory.path(),
                package.architecture,
                &package.version,
                Some(DEFAULT_BASE_URL),
                None,
            )
            .await
            .unwrap();
            assert!(
                cached_kernel_is_valid(directory.path(), package.architecture, &package.version)
                    .await
            );
            let receipt = built_kernel(directory.path(), package.architecture, &package.version)
                .await
                .unwrap();
            assert_eq!(
                receipt.package_sha256.as_deref(),
                Some(package.sha256.encoded())
            );
            eprintln!(
                "verified public kernel {} {}: {}",
                package.version,
                package.architecture,
                path.display()
            );
        }
    }
}

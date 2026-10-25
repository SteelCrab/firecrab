//! Native guest kernels built from kernel.org's stable source releases.
//! Receipts bind each cached image to its source digest and host architecture.

use super::boot::{KernelBootPair, verify_kernel_architecture};
use super::kernel_legacy;
use super::*;
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::process::Stdio;
use std::time::SystemTime;
use tokio::io::AsyncBufReadExt as _;

pub(crate) const DEFAULT_RELEASES_URL: &str = "https://www.kernel.org/releases.json";
const BUILD_SCRIPT: &str = include_str!("../../../scripts/build-host-kernel.sh");
const BUILD_REVISION: &str = "firecrab-host1";
const KERNEL_MAX_BYTES: u64 = 256 * 1024 * 1024;
const SOURCE_MAX_BYTES: u64 = 512 * 1024 * 1024;
const METADATA_MAX_BYTES: u64 = 4 * 1024 * 1024;
const RELEASE_CACHE_TTL: Duration = Duration::from_secs(3600);
pub(super) const X86_64_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw";
pub(super) const AARCH64_BOOT_ARGS: &str =
    "keep_bootcon console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct StableRelease {
    pub version: String,
    pub source_url: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct BuiltKernel {
    pub version: String,
    pub architecture: String,
    pub source_url: String,
    pub source_sha256: String,
    pub image_sha256: String,
    pub build_revision: String,
}

pub(super) fn configured_kernel_override() -> Option<PathBuf> {
    std::env::var("FIRECRAB_OCI_KERNEL_PATH")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub(crate) fn configured_releases_url() -> Option<String> {
    let value = std::env::var("FIRECRAB_KERNEL_RELEASES_URL")
        .unwrap_or_else(|_| DEFAULT_RELEASES_URL.to_owned());
    let value = value.trim();
    (!matches!(value, "" | "none" | "-")).then(|| value.to_owned())
}

/// Only final numeric releases can become source URLs or filesystem names.
pub(crate) fn is_source_version(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    (2..=3).contains(&parts.len())
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 5
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
        && parts[0] != "0"
}

pub(crate) fn source_url(version: &str) -> Option<String> {
    is_source_version(version).then(|| {
        format!(
            "https://cdn.kernel.org/pub/linux/kernel/v{}.x/linux-{version}.tar.xz",
            version.split('.').next().expect("validated version")
        )
    })
}

pub(crate) fn image_name(architecture: Architecture, version: &str) -> Option<String> {
    if !is_source_version(version) && kernel_legacy::image_digest(architecture, version).is_none() {
        return None;
    }
    let prefix = match architecture {
        Architecture::X86_64 => "vmlinux",
        Architecture::Aarch64 => "Image",
    };
    Some(format!("{prefix}-{version}-{}", architecture.as_str()))
}

pub(crate) fn cache_relative(architecture: Architecture, version: &str) -> Option<PathBuf> {
    Some(
        Path::new(".oci/kernel")
            .join(architecture.as_str())
            .join(image_name(architecture, version)?),
    )
}

pub(crate) fn version_for_path(architecture: Architecture, path: &Path) -> Option<String> {
    let filename = path.file_name()?.to_str()?;
    let prefix = match architecture {
        Architecture::X86_64 => "vmlinux-",
        Architecture::Aarch64 => "Image-",
    };
    let version = filename
        .strip_prefix(prefix)?
        .strip_suffix(&format!("-{}", architecture.as_str()))?;
    (cache_relative(architecture, version)?.as_path() == path).then(|| version.to_owned())
}

fn receipt_path(image_root: &Path, architecture: Architecture, version: &str) -> Option<PathBuf> {
    Some(
        image_root
            .join(cache_relative(architecture, version)?)
            .with_added_extension("json"),
    )
}

pub(crate) async fn built_kernel(
    image_root: &Path,
    architecture: Architecture,
    version: &str,
) -> Option<BuiltKernel> {
    let bytes = tokio::fs::read(receipt_path(image_root, architecture, version)?)
        .await
        .ok()?;
    let receipt: BuiltKernel = serde_json::from_slice(&bytes).ok()?;
    if receipt.version != version
        || receipt.architecture != architecture.as_str()
        || receipt.build_revision != BUILD_REVISION
        || Some(&receipt.source_url) != source_url(version).as_ref()
        || Sha256Digest::parse(&format!("sha256:{}", receipt.source_sha256)).is_err()
        || Sha256Digest::parse(&format!("sha256:{}", receipt.image_sha256)).is_err()
    {
        return None;
    }
    Some(receipt)
}

pub(crate) fn parse_stable_release(bytes: &[u8]) -> Result<StableRelease, ResolveError> {
    let document: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| {
        ResolveError::Malformed(format!("kernel.org release metadata: {error}"))
    })?;
    let release = document["releases"]
        .as_array()
        .and_then(|releases| {
            releases
                .iter()
                .find(|release| release["moniker"] == "stable" && release["iseol"] != true)
        })
        .ok_or_else(|| {
            ResolveError::Malformed("kernel.org has no active stable release".to_owned())
        })?;
    let version = release["version"].as_str().unwrap_or_default();
    let expected = source_url(version)
        .ok_or_else(|| ResolveError::Malformed("invalid kernel.org stable version".to_owned()))?;
    if release["source"].as_str() != Some(expected.as_str()) {
        return Err(ResolveError::Malformed(
            "unexpected kernel.org stable source URL".to_owned(),
        ));
    }
    Ok(StableRelease {
        version: version.to_owned(),
        source_url: expected,
    })
}

/// A bounded cache avoids reaching upstream on every dashboard poll.
/// Stale metadata remains useful when a host temporarily loses its network.
pub(crate) async fn latest_stable(
    image_root: &Path,
    releases_url: Option<&str>,
) -> Result<StableRelease, ResolveError> {
    let directory = image_root.join(".oci/kernel");
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|source| cache_io("create kernel cache", directory.clone(), source))?;
    let lock = cache_path_lock(&directory, Path::new("releases.json")).await?;
    let _guard = lock.lock().await;
    let path = directory.join("releases.json");
    let cached = tokio::fs::read(&path)
        .await
        .ok()
        .and_then(|bytes| parse_stable_release(&bytes).ok());
    let fresh = tokio::fs::metadata(&path)
        .await
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age < RELEASE_CACHE_TTL);
    if fresh && let Some(release) = &cached {
        return Ok(release.clone());
    }
    let Some(url) = releases_url else {
        return cached.ok_or_else(|| {
            unavailable(Architecture::HOST, "kernel source downloads are disabled")
        });
    };
    let fetched = async {
        let bytes = fetch_bytes(url, METADATA_MAX_BYTES).await?;
        let release = parse_stable_release(&bytes)?;
        write_atomic(&path, &bytes).await?;
        Ok(release)
    }
    .await;
    match (fetched, cached) {
        (Ok(release), _) => Ok(release),
        (Err(error), Some(release)) => {
            tracing::warn!(%error, "using cached kernel.org stable metadata");
            Ok(release)
        }
        (Err(error), None) => Err(error),
    }
}

fn http_client() -> Result<reqwest::Client, ResolveError> {
    reqwest::Client::builder()
        .user_agent(concat!("firecrab-api/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| download_failed(DEFAULT_RELEASES_URL, error.to_string()))
}

async fn fetch_bytes(url: &str, limit: u64) -> Result<Vec<u8>, ResolveError> {
    let mut response = http_client()?
        .get(url)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| download_failed(url, error.to_string()))?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| download_failed(url, error.to_string()))?
    {
        if bytes.len() as u64 + chunk.len() as u64 > limit {
            return Err(download_failed(
                url,
                format!("response exceeds {limit} bytes"),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(crate) fn source_checksum(bytes: &[u8], version: &str) -> Result<Sha256Digest, ResolveError> {
    let filename = format!("linux-{version}.tar.xz");
    let text =
        std::str::from_utf8(bytes).map_err(|error| ResolveError::Malformed(error.to_string()))?;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let (Some(digest), Some(name), None) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        if name.trim_start_matches('*') == filename {
            return Sha256Digest::parse(&format!("sha256:{digest}")).map_err(|error| {
                ResolveError::Malformed(format!("kernel source checksum: {error}"))
            });
        }
    }
    Err(ResolveError::Malformed(format!(
        "kernel.org checksums do not contain {filename}"
    )))
}

pub(crate) async fn cached_kernel_is_valid(
    image_root: &Path,
    architecture: Architecture,
    version: &str,
) -> bool {
    let Some(relative) = cache_relative(architecture, version) else {
        return false;
    };
    let expected = if let Some(receipt) = built_kernel(image_root, architecture, version).await {
        format!("sha256:{}", receipt.image_sha256)
    } else if let Some(legacy) = kernel_legacy::image_digest(architecture, version) {
        legacy.to_owned()
    } else {
        return false;
    };
    let Ok(actual) = inspect_kernel(&image_root.join(relative), architecture).await else {
        return false;
    };
    actual.as_str() == expected
}

pub(crate) async fn installed_versions(
    image_root: &Path,
    architecture: Architecture,
) -> Vec<String> {
    let mut versions = Vec::new();
    let directory = image_root.join(".oci/kernel").join(architecture.as_str());
    if let Ok(mut entries) = tokio::fs::read_dir(directory).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(relative) = entry.path().strip_prefix(image_root).map(Path::to_owned) else {
                continue;
            };
            if let Some(version) = version_for_path(architecture, &relative) {
                versions.push(version);
            }
        }
    }
    versions.sort_by_key(|version| {
        version
            .split(['.', '-'])
            .map(|part| part.parse::<u32>().unwrap_or_default())
            .collect::<Vec<_>>()
    });
    versions.reverse();
    versions
}

pub(crate) async fn cache_lock(
    image_root: &Path,
    architecture: Architecture,
    version: &str,
) -> Result<Arc<AsyncMutex<()>>, ResolveError> {
    let relative = cache_relative(architecture, version)
        .ok_or_else(|| unavailable(architecture, "invalid kernel version"))?;
    let directory = image_root.join(".oci/kernel").join(architecture.as_str());
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|source| cache_io("create kernel cache", directory.clone(), source))?;
    cache_path_lock(
        &directory,
        relative.file_name().expect("cache filename").as_ref(),
    )
    .await
}

pub(crate) async fn ensure_managed_kernel(
    image_root: &Path,
    architecture: Architecture,
    requested: &str,
    releases_url: Option<&str>,
    tracker: Option<&ImageInstallTracker>,
) -> Result<PathBuf, ResolveError> {
    let version = if requested == "stable" {
        latest_stable(image_root, releases_url).await?.version
    } else {
        requested.to_owned()
    };
    let relative = cache_relative(architecture, &version)
        .ok_or_else(|| unavailable(architecture, "invalid kernel version"))?;
    let destination = image_root.join(&relative);
    let directory = destination.parent().expect("cache image has parent");
    let lock = cache_lock(image_root, architecture, &version).await?;
    let _guard = lock.lock().await;
    if cached_kernel_is_valid(image_root, architecture, &version).await {
        return Ok(relative);
    }
    let url = source_url(&version).ok_or_else(|| {
        unavailable(
            architecture,
            "legacy kernels can only be reused from a verified cache",
        )
    })?;
    if releases_url.is_none() {
        return Err(unavailable(
            architecture,
            "kernel source downloads are disabled",
        ));
    }
    if !cfg!(target_os = "linux") || architecture != Architecture::HOST {
        return Err(unavailable(
            architecture,
            "kernel compilation requires a Linux host with a matching CPU architecture",
        ));
    }
    log(
        tracker,
        requested,
        format!("resolving Linux {version} source checksum"),
    );
    let checksum_url = format!(
        "{}/sha256sums.asc",
        url.rsplit_once('/').expect("source directory").0
    );
    let checksum = source_checksum(
        &fetch_bytes(&checksum_url, METADATA_MAX_BYTES).await?,
        &version,
    )?;
    let sources = image_root.join(".oci/kernel/sources");
    tokio::fs::create_dir_all(&sources)
        .await
        .map_err(|source| cache_io("create source cache", sources.clone(), source))?;
    let archive = sources.join(format!("linux-{version}.tar.xz"));
    let source_lock = cache_path_lock(
        &sources,
        archive.file_name().expect("source filename").as_ref(),
    )
    .await?;
    {
        let _source_guard = source_lock.lock().await;
        log(
            tracker,
            requested,
            format!("downloading and verifying {url}"),
        );
        download_source(
            &url,
            &archive,
            &checksum,
            tracker.map(|tracker| (tracker, requested)),
        )
        .await?;
    }
    // Different requested versions still share the host's compiler capacity.
    let build_lock = cache_path_lock(directory, Path::new(".native-build")).await?;
    log(
        tracker,
        requested,
        "waiting for the host kernel builder".to_owned(),
    );
    let _build_guard = build_lock.lock().await;
    let work = directory.join(format!(".build-{version}-{}", Uuid::new_v4()));
    tokio::fs::create_dir(&work)
        .await
        .map_err(|source| cache_io("create build directory", work.clone(), source))?;
    let staged = work.join("kernel");
    let result = async {
        log(
            tracker,
            requested,
            format!("building Linux {version} for {architecture}"),
        );
        tokio::time::timeout(
            Duration::from_secs(2 * 60 * 60),
            run_build(
                architecture,
                &version,
                &archive,
                &work,
                &staged,
                tracker.map(|tracker| (tracker, requested)),
            ),
        )
        .await
        .map_err(|_| unavailable(architecture, "kernel build exceeded two hours"))??;
        let image_digest = inspect_kernel(&staged, architecture).await?;
        let receipt = BuiltKernel {
            version: version.clone(),
            architecture: architecture.as_str().to_owned(),
            source_url: url,
            source_sha256: checksum.encoded().to_owned(),
            image_sha256: image_digest.encoded().to_owned(),
            build_revision: BUILD_REVISION.to_owned(),
        };
        tokio::fs::rename(
            staged.with_added_extension("config"),
            destination.with_added_extension("config"),
        )
        .await
        .map_err(|source| cache_io("publish kernel config", destination.clone(), source))?;
        tokio::fs::rename(&staged, &destination)
            .await
            .map_err(|source| cache_io("publish built kernel", destination.clone(), source))?;
        write_atomic(
            &destination.with_added_extension("json"),
            &serde_json::to_vec_pretty(&receipt).expect("serialize kernel receipt"),
        )
        .await?;
        Ok(relative)
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&work).await;
    result
}

pub(crate) async fn download_source(
    url: &str,
    destination: &Path,
    expected: &Sha256Digest,
    progress: Option<(&ImageInstallTracker, &str)>,
) -> Result<(), ResolveError> {
    if let Ok(digest) = source_digest(destination).await
        && digest == *expected
    {
        return Ok(());
    }
    let partial = destination.with_extension(format!("{}-partial", Uuid::new_v4()));
    let result = async {
        let mut response = http_client()?
            .get(url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|error| download_failed(url, error.to_string()))?;
        let total = response.content_length();
        if total.is_some_and(|length| length > SOURCE_MAX_BYTES) {
            return Err(download_failed(
                url,
                "source archive is too large".to_owned(),
            ));
        }
        let mut file = tokio::fs::File::create(&partial)
            .await
            .map_err(|source| cache_io("stage kernel source", partial.clone(), source))?;
        let mut hasher = Sha256::new();
        let mut written = 0_u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| download_failed(url, error.to_string()))?
        {
            written += chunk.len() as u64;
            if written > SOURCE_MAX_BYTES {
                return Err(download_failed(
                    url,
                    "source archive is too large".to_owned(),
                ));
            }
            file.write_all(&chunk)
                .await
                .map_err(|source| cache_io("write kernel source", partial.clone(), source))?;
            hasher.update(&chunk);
            if let Some((tracker, job)) = progress {
                tracker.set_download_progress(job, written, total);
            }
        }
        file.flush()
            .await
            .map_err(|source| cache_io("flush kernel source", partial.clone(), source))?;
        drop(file);
        let actual = digest_from(hasher);
        if actual != *expected {
            return Err(ResolveError::DigestMismatch {
                subject: url.to_owned(),
                expected: expected.clone(),
                actual,
            });
        }
        tokio::fs::rename(&partial, destination)
            .await
            .map_err(|source| cache_io("publish kernel source", destination.to_owned(), source))?;
        Ok(())
    }
    .await;
    let _ = tokio::fs::remove_file(&partial).await;
    result
}

async fn source_digest(path: &Path) -> Result<Sha256Digest, ResolveError> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
            .map_err(|source| cache_io("open cached kernel source", path.clone(), source))?;
        let metadata = file
            .metadata()
            .map_err(|source| cache_io("inspect cached kernel source", path.clone(), source))?;
        if !metadata.is_file() || metadata.len() > SOURCE_MAX_BYTES {
            return Err(download_failed(
                &path.display().to_string(),
                "invalid cached source archive".to_owned(),
            ));
        }
        let mut hasher = Sha256::new();
        let copied = io::copy(&mut file.take(SOURCE_MAX_BYTES + 1), &mut hasher)
            .map_err(|source| cache_io("hash cached kernel source", path.clone(), source))?;
        if copied > SOURCE_MAX_BYTES {
            return Err(download_failed(
                &path.display().to_string(),
                "source archive is too large".to_owned(),
            ));
        }
        Ok(digest_from(hasher))
    })
    .await
    .map_err(|error| ResolveError::Malformed(format!("hash cached kernel source: {error}")))?
}

/// Kill the complete compiler process group if the task is cancelled.
struct BuildGroup(u32);
impl Drop for BuildGroup {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
    }
}

async fn run_build(
    architecture: Architecture,
    version: &str,
    archive: &Path,
    work: &Path,
    output: &Path,
    progress: Option<(&ImageInstallTracker, &str)>,
) -> Result<(), ResolveError> {
    let script = work.join("build-kernel.sh");
    tokio::fs::write(&script, BUILD_SCRIPT)
        .await
        .map_err(|source| cache_io("write kernel builder", script.clone(), source))?;
    let mut command = std::process::Command::new("bash");
    command.process_group(0);
    let mut command = tokio::process::Command::from(command);
    let mut child = command
        .arg(&script)
        .arg(architecture.as_str())
        .arg(version)
        .arg(archive)
        .arg(work)
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| unavailable(architecture, format!("start kernel builder: {error}")))?;
    let _group = BuildGroup(child.id().expect("new child has pid"));
    let stdout = child.stdout.take().expect("piped builder stdout");
    let stderr = child.stderr.take().expect("piped builder stderr");
    let (stdout, stderr) = tokio::join!(build_log(stdout, progress), build_log(stderr, progress));
    let status = child
        .wait()
        .await
        .map_err(|error| unavailable(architecture, format!("wait for kernel builder: {error}")))?;
    if !status.success() {
        return Err(unavailable(
            architecture,
            format!("kernel build failed ({status}):\n{}\n{}", stdout?, stderr?),
        ));
    }
    stdout?;
    stderr?;
    Ok(())
}

async fn build_log(
    reader: impl AsyncRead + Unpin,
    progress: Option<(&ImageInstallTracker, &str)>,
) -> Result<String, ResolveError> {
    let mut lines = BufReader::new(reader).lines();
    let mut tail = std::collections::VecDeque::new();
    while let Some(line) = lines.next_line().await.map_err(|error| {
        unavailable(
            Architecture::HOST,
            format!("read kernel builder log: {error}"),
        )
    })? {
        let line: String = line.chars().take(2000).collect();
        if let Some((tracker, job)) = progress {
            tracker.append_log(job, &line);
        }
        tail.push_back(line);
        if tail.len() > 10 {
            tail.pop_front();
        }
    }
    Ok(tail.into_iter().collect::<Vec<_>>().join("\n"))
}

pub(super) async fn ensure_host_kernel(
    image_root: &Path,
    architecture: Architecture,
    override_path: Option<&Path>,
    releases_url: Option<&str>,
) -> Result<KernelBootPair, ResolveError> {
    if let Some(path) = override_path {
        return operator_kernel(image_root, architecture, path).await;
    }
    let result = async {
        let release = latest_stable(image_root, releases_url).await?;
        let relative = ensure_managed_kernel(
            image_root,
            architecture,
            &release.version,
            releases_url,
            None,
        )
        .await?;
        Ok(boot_pair(architecture, relative, &release.version))
    }
    .await;
    let error = match result {
        Ok(pair) => return Ok(pair),
        Err(error) => error,
    };
    // An upstream outage must not prevent using an already verified kernel.
    for version in installed_versions(image_root, architecture).await {
        if cached_kernel_is_valid(image_root, architecture, &version).await {
            tracing::warn!(%error, %version, "using a previously installed kernel for OCI import");
            return Ok(boot_pair(
                architecture,
                cache_relative(architecture, &version).expect("installed cache path"),
                &version,
            ));
        }
    }
    Err(error)
}

async fn operator_kernel(
    image_root: &Path,
    architecture: Architecture,
    supplied: &Path,
) -> Result<KernelBootPair, ResolveError> {
    let directory = image_root.join(".oci/kernel").join(architecture.as_str());
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|source| cache_io("create kernel cache", directory.clone(), source))?;
    let staged = directory.join(format!(".operator-{}", Uuid::new_v4()));
    let result = async {
        let source = tokio::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(supplied)
            .await
            .map_err(|source| kernel_io("open operator kernel", supplied.to_owned(), source))?;
        let metadata = source
            .metadata()
            .await
            .map_err(|source| kernel_io("inspect operator kernel", supplied.to_owned(), source))?;
        if !metadata.is_file() || metadata.len() > KERNEL_MAX_BYTES {
            return Err(unavailable(
                architecture,
                "operator kernel must be a regular file smaller than 256 MiB",
            ));
        }
        let mut destination = tokio::fs::File::create(&staged)
            .await
            .map_err(|source| kernel_io("stage operator kernel", staged.clone(), source))?;
        let copied = tokio::io::copy(&mut source.take(KERNEL_MAX_BYTES + 1), &mut destination)
            .await
            .map_err(|source| kernel_io("copy operator kernel", staged.clone(), source))?;
        if copied > KERNEL_MAX_BYTES {
            return Err(unavailable(architecture, "operator kernel is too large"));
        }
        drop(destination);
        let digest = inspect_kernel(&staged, architecture).await?;
        let relative = Path::new(".oci/kernel")
            .join(architecture.as_str())
            .join(format!("custom-{}", digest.encoded()));
        tokio::fs::rename(&staged, image_root.join(&relative))
            .await
            .map_err(|source| kernel_io("publish operator kernel", staged.clone(), source))?;
        Ok(boot_pair(architecture, relative, "operator kernel"))
    }
    .await;
    let _ = tokio::fs::remove_file(staged).await;
    result
}

fn boot_pair(architecture: Architecture, kernel: PathBuf, version: &str) -> KernelBootPair {
    KernelBootPair {
        architecture,
        source_alias: format!("Linux {version}"),
        kernel,
        initrd: None,
        boot_args: match architecture {
            Architecture::X86_64 => X86_64_BOOT_ARGS,
            Architecture::Aarch64 => AARCH64_BOOT_ARGS,
        }
        .to_owned(),
    }
}

pub(crate) async fn inspect_kernel(
    path: &Path,
    architecture: Architecture,
) -> Result<Sha256Digest, ResolveError> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
            .map_err(|source| kernel_io("open kernel", path.clone(), source))?;
        let metadata = file
            .metadata()
            .map_err(|source| kernel_io("inspect kernel", path.clone(), source))?;
        if !metadata.is_file() || metadata.len() > KERNEL_MAX_BYTES {
            return Err(unavailable(
                architecture,
                "kernel must be a regular file smaller than 256 MiB",
            ));
        }
        let mut hasher = Sha256::new();
        let mut header = Vec::new();
        let mut buffer = vec![0_u8; 128 * 1024];
        let mut total = 0_u64;
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|source| kernel_io("read kernel", path.clone(), source))?;
            if read == 0 {
                break;
            }
            total += read as u64;
            if total > KERNEL_MAX_BYTES {
                return Err(unavailable(architecture, "kernel is too large"));
            }
            hasher.update(&buffer[..read]);
            if header.len() < 64 {
                header.extend_from_slice(&buffer[..read.min(64 - header.len())]);
            }
        }
        verify_kernel_architecture(&path, &header, architecture)?;
        Ok(digest_from(hasher))
    })
    .await
    .map_err(|error| unavailable(architecture, format!("verify kernel: {error}")))?
}

async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ResolveError> {
    let partial = path.with_extension(format!("{}-partial", Uuid::new_v4()));
    let result = async {
        tokio::fs::write(&partial, bytes)
            .await
            .map_err(|source| cache_io("write kernel metadata", partial.clone(), source))?;
        tokio::fs::rename(&partial, path)
            .await
            .map_err(|source| cache_io("publish kernel metadata", path.to_owned(), source))
    }
    .await;
    let _ = tokio::fs::remove_file(partial).await;
    result
}

fn log(tracker: Option<&ImageInstallTracker>, job: &str, message: String) {
    if let Some(tracker) = tracker {
        tracker.append_log(job, message);
    }
}
fn digest_from(hasher: Sha256) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize())).expect("valid sha256")
}
fn unavailable(architecture: Architecture, reason: impl Into<String>) -> ResolveError {
    ResolveError::KernelUnavailable {
        architecture,
        reason: reason.into(),
    }
}
fn download_failed(url: &str, message: String) -> ResolveError {
    ResolveError::KernelDownloadFailed {
        url: url.to_owned(),
        message,
    }
}
fn kernel_io(operation: &'static str, path: PathBuf, source: io::Error) -> ResolveError {
    ResolveError::KernelIo {
        operation,
        path,
        source,
    }
}

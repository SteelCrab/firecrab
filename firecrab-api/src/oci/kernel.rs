//! Supplies the kernel an imported container tree boots under.
//!
//! A container image carries no kernel, and a container tree has no
//! `/lib/modules`, so the kernel must have `virtio_blk`, `virtio_net`,
//! `virtio_mmio`, and `ext4` built in and must need no initrd — an initrd
//! would take PID 1 away from the injected guest init. Firecrab publishes one
//! such kernel per architecture on the MicroRegistry, so importing a 50 MB
//! container no longer means installing a full distro image first.
//!
//! The guest toolbox (`busybox.rs`) already establishes the shape: a
//! digest-pinned artifact, downloaded once, re-verified on every reuse, cached
//! under the image root, with an operator override for mirrors and air-gapped
//! hosts. The pin names digests rather than a version tag, so a repointed
//! object cannot change what a host boots.

use super::*;

use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt as _;

use super::boot::{KernelBootPair, verify_kernel_architecture};

/// Operator override naming a kernel image already on this host.
const KERNEL_PATH_ENV: &str = "FIRECRAB_OCI_KERNEL_PATH";
/// Ceiling on the published package, so a mirrored base URL cannot stream an
/// unbounded body onto the image volume before any digest is checked.
const PACKAGE_MAX_BYTES: u64 = 512 * 1024 * 1024;
/// Ceiling on the kernel lifted out of that package.
const KERNEL_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// Archive directory the packaged kernel lives in.
const PACKAGE_KERNEL_DIRECTORY: &str = "kernel";
/// Ceiling on reaching the registry, matching the OCI registry session.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Ceiling on the gap between two body chunks, not on the whole download.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Firecracker command line an imported rootfs boots with on x86_64.
const X86_64_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw";
/// Firecracker command line an imported rootfs boots with on aarch64.
const AARCH64_BOOT_ARGS: &str =
    "keep_bootcon console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw";

/// One published kernel artifact this build trusts.
///
/// Borrowed rather than owned so the compiled pins are `'static` constants
/// while tests can pin a fixture built at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PinnedKernel<'a> {
    /// Registry alias, which is also the package filename stem.
    pub alias: &'a str,
    /// Upstream kernel version, which is a registry path segment.
    pub version: &'a str,
    /// Kernel filename inside the package and under the cache.
    pub image: &'a str,
    /// `sha256:…` of the published `.tar.zst`.
    pub package_digest: &'a str,
    /// `sha256:…` of the kernel file inside that package.
    pub image_digest: &'a str,
    /// Firecracker command line this kernel boots an OCI rootfs with.
    pub boot_args: &'a str,
}

/// The kernel this build pins for `architecture`.
///
/// Every supported architecture carries the latest stable kernel that was
/// published when this release was built. `None` remains the signal for a
/// future architecture that has no dedicated artifact yet.
pub(crate) fn pinned_kernel(architecture: Architecture) -> Option<PinnedKernel<'static>> {
    supported_kernels(architecture).into_iter().next()
}

/// Kernels this release can install for one host architecture, newest first.
///
/// The first entry is the default used by OCI import. Older entries stay in
/// the catalog so an operator can keep an image on a known-good kernel while
/// testing a newer one, or roll an image back without downloading a custom
/// artifact. Retention policy: keep at most 3 versions per `major.minor`
/// branch (e.g. `7.1.x`, `7.2.x`); when the MicroRegistry publishes a 4th
/// patch release on a branch, drop that branch's oldest entry here.
pub(crate) fn supported_kernels(architecture: Architecture) -> Vec<PinnedKernel<'static>> {
    match architecture {
        Architecture::X86_64 => vec![
            PinnedKernel {
                alias: "vmlinux-7.2.2",
                version: "7.2.2",
                image: "vmlinux-7.2.2-x86_64",
                package_digest: "sha256:716d7bcfcf9118a76d0c9f0b7ab06ba167f78b77cb16f928612809c75ebeffae",
                image_digest: "sha256:351b23784e5e53de5af970ff6af7a074a916a55327ff802645f3583b39f3a4f1",
                boot_args: X86_64_BOOT_ARGS,
            },
            PinnedKernel {
                alias: "vmlinux-7.1.12",
                version: "7.1.12",
                image: "vmlinux-7.1.12-x86_64",
                package_digest: "sha256:b4666cdc2ded25c6e929c9b4a4d4bcad0bff58c6fcae831d9514d17819fac31e",
                image_digest: "sha256:004c3031dfcc2deec974b55714a2ed1052903662531e8df2a3cb3380bda5674b",
                boot_args: X86_64_BOOT_ARGS,
            },
            PinnedKernel {
                alias: "vmlinux-7.1.9",
                version: "7.1.9",
                image: "vmlinux-7.1.9-x86_64",
                package_digest: "sha256:fd058e64d2173b3911ad09a15aa6bcf15531941254ce44ba6e935b876662ba65",
                image_digest: "sha256:079d2149a9378f5705da46232e99886187fecdd5517428d2c294b6bd1e0dca6b",
                boot_args: X86_64_BOOT_ARGS,
            },
            PinnedKernel {
                alias: "vmlinux-7.1.8",
                version: "7.1.8",
                image: "vmlinux-7.1.8-x86_64",
                package_digest: "sha256:eb2efc87a8b64b9bcf5c4b7365193c578e6cf203c4773060ec157d6f34c11f53",
                image_digest: "sha256:1d693ebb340ee418127aacf679080671679e455302a8f89ff8d4a45b6d293cdb",
                boot_args: X86_64_BOOT_ARGS,
            },
        ],
        Architecture::Aarch64 => vec![
            PinnedKernel {
                alias: "vmlinux-7.2.2",
                version: "7.2.2",
                image: "Image-7.2.2-aarch64",
                package_digest: "sha256:885550806de246892df6f08b6d37a3062ac987228729ee50b9421d5caa98621f",
                image_digest: "sha256:f3acb729ded8213e4b157fd80746f6034cc4994888f73b82b1e34cea5813dbc0",
                boot_args: AARCH64_BOOT_ARGS,
            },
            PinnedKernel {
                alias: "vmlinux-7.1.12",
                version: "7.1.12",
                image: "Image-7.1.12-aarch64",
                package_digest: "sha256:3cdf98523c4688ba6061cb025394931c696e67988d8367280639941f8a833c48",
                image_digest: "sha256:6d6205c7dccbadcd74fa0bf84082af2f13bd8492d7a489ee0e2a6891c00c10bd",
                boot_args: AARCH64_BOOT_ARGS,
            },
            PinnedKernel {
                alias: "vmlinux-7.1.9",
                version: "7.1.9",
                image: "Image-7.1.9-aarch64",
                package_digest: "sha256:3a8576656d45eb051874a4b1cf4b8838d54d96aa268bc53706118e0bbeb727f7",
                image_digest: "sha256:6d60d06429aa5e244cc5a0eb60383a97fde61f4aa40453d5cc6fecb59a64b58f",
                boot_args: AARCH64_BOOT_ARGS,
            },
            PinnedKernel {
                alias: "vmlinux-7.1.8",
                version: "7.1.8",
                image: "Image-7.1.8-aarch64",
                package_digest: "sha256:bddbe93f83fc3180a23c1c55a0a40946e5906592ec6331969bd64d68a302a507",
                image_digest: "sha256:f1ded792a87f2b2d798566d22485fe73367dc8f348670827638f1a6b43375a44",
                boot_args: AARCH64_BOOT_ARGS,
            },
        ],
    }
}

/// Finds a digest-pinned kernel by catalog version for this architecture.
pub(crate) fn kernel_for_version(
    architecture: Architecture,
    version: &str,
) -> Option<PinnedKernel<'static>> {
    supported_kernels(architecture)
        .into_iter()
        .find(|kernel| kernel.version == version)
}

/// Reads the operator's kernel override, if one is set.
pub(super) fn configured_kernel_override() -> Option<PathBuf> {
    let value = std::env::var(KERNEL_PATH_ENV).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| PathBuf::from(value))
}

/// The MicroRegistry base the kernel is fetched from.
///
/// This is the same `FIRECRAB_IMAGE_BASE_URL` that serves catalog images, so a
/// private mirror or an air-gapped `none` already covers the kernel too.
pub(super) fn configured_base_url() -> Option<String> {
    ImageInstallTracker::from_env()
        .base_url()
        .map(str::to_owned)
}

/// Registry object key of a pinned package.
pub(crate) fn package_key(architecture: Architecture, pinned: &PinnedKernel<'_>) -> String {
    format!(
        "{PACKAGE_KERNEL_DIRECTORY}/{}/{}/{}.tar.zst",
        pinned.version,
        architecture.as_str(),
        pinned.alias
    )
}

/// Image-root-relative cache path a registered `TemplateSpec` records.
pub(crate) fn cache_relative(architecture: Architecture, pinned: &PinnedKernel<'_>) -> PathBuf {
    Path::new(".oci/kernel")
        .join(architecture.as_str())
        .join(pinned.image)
}

/// Ensures a catalog kernel is present in the shared OCI kernel cache.
///
/// Kernel management intentionally uses the same cache and verification path
/// as OCI import. That keeps one copy per architecture/version and means a
/// kernel installed from the dashboard is immediately usable by a later OCI
/// import without a second download.
pub(crate) async fn ensure_managed_kernel(
    image_root: &Path,
    architecture: Architecture,
    version: &str,
    base_url: Option<&str>,
) -> Result<PathBuf, ResolveError> {
    let pinned = kernel_for_version(architecture, version).ok_or_else(|| {
        ResolveError::KernelUnavailable {
            architecture,
            reason: format!("kernel version {version} is not in the release catalog"),
        }
    })?;
    let pair = ensure_pinned_kernel(image_root, architecture, &pinned, None, base_url).await?;
    Ok(pair.kernel)
}

/// Returns whether the cached bytes still match the catalog digest and host
/// architecture. A corrupt cache is reported as not installed so the next
/// install can replace it instead of exposing a false-ready state.
pub(crate) async fn cached_kernel_is_valid(
    image_root: &Path,
    architecture: Architecture,
    version: &str,
) -> bool {
    let Some(pinned) = kernel_for_version(architecture, version) else {
        return false;
    };
    let relative = cache_relative(architecture, &pinned);
    let path = image_root.join(&relative);
    if !path.is_file() {
        return false;
    }
    let pair = KernelBootPair {
        architecture,
        source_alias: pinned.alias.to_owned(),
        kernel: relative,
        initrd: None,
        boot_args: pinned.boot_args.to_owned(),
    };
    verify_kernel(&path, &pair, &pinned).await.is_ok()
}

/// Archive member the kernel is lifted from.
fn package_member(pinned: &PinnedKernel<'_>) -> String {
    format!("{PACKAGE_KERNEL_DIRECTORY}/{}", pinned.image)
}

/// Acquires the pinned kernel, reaching the registry only when the cache
/// cannot serve it.
///
/// The cached file is re-verified against the pinned digest and the ELF or
/// arm64 header on every call, so a truncated or repointed cache entry is
/// refetched instead of booted.
pub(super) async fn ensure_pinned_kernel(
    image_root: &Path,
    architecture: Architecture,
    pinned: &PinnedKernel<'_>,
    override_path: Option<&Path>,
    base_url: Option<&str>,
) -> Result<KernelBootPair, ResolveError> {
    let relative = cache_relative(architecture, pinned);
    let cached = image_root.join(&relative);
    let directory = cached
        .parent()
        .expect("kernel cache paths are built with a parent");
    tokio::fs::create_dir_all(directory)
        .await
        .map_err(|source| cache_io("create kernel cache", directory.to_owned(), source))?;
    let lock = cache_path_lock(directory, Path::new(pinned.image)).await?;
    let _guard = lock.lock().await;

    let pair = KernelBootPair {
        architecture,
        source_alias: pinned.alias.to_owned(),
        kernel: relative,
        initrd: None,
        boot_args: pinned.boot_args.to_owned(),
    };
    if verify_kernel(&cached, &pair, pinned).await.is_ok() {
        return Ok(pair);
    }
    // A cache entry that fails verification is stale or corrupt, never trusted.
    let _ = tokio::fs::remove_file(&cached).await;

    let staged = directory.join(format!(".{}-{}.partial", pinned.image, Uuid::new_v4()));
    let result = stage_kernel(
        image_root,
        architecture,
        pinned,
        override_path,
        base_url,
        &staged,
    )
    .await;
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&staged).await;
        return Err(error);
    }
    if let Err(error) = verify_kernel(&staged, &pair, pinned).await {
        let _ = tokio::fs::remove_file(&staged).await;
        return Err(error);
    }
    tokio::fs::rename(&staged, &cached)
        .await
        .map_err(|source| cache_io("publish OCI kernel", cached.clone(), source))?;
    Ok(pair)
}

/// Writes the kernel to `staged` from whichever source is configured.
async fn stage_kernel(
    image_root: &Path,
    architecture: Architecture,
    pinned: &PinnedKernel<'_>,
    override_path: Option<&Path>,
    base_url: Option<&str>,
    staged: &Path,
) -> Result<(), ResolveError> {
    if let Some(supplied) = override_path {
        return copy_operator_kernel(supplied, staged).await;
    }
    let Some(base_url) = base_url else {
        return Err(ResolveError::KernelUnavailable {
            architecture,
            reason: format!("no {KERNEL_PATH_ENV} override and remote image install is disabled"),
        });
    };
    let key = package_key(architecture, pinned);
    let url = format!("{}/{key}", base_url.trim_end_matches('/'));
    let package = image_root.join(".oci/kernel").join(format!(
        ".{}-{}.tar.zst",
        pinned.alias,
        Uuid::new_v4()
    ));
    let result = match download_package(&url, &package, pinned).await {
        Ok(()) => lift_kernel(&package, &key, pinned, staged).await,
        Err(error) => Err(error),
    };
    let _ = tokio::fs::remove_file(&package).await;
    result
}

/// Copies an operator-supplied kernel into the cache staging path.
///
/// The bytes still have to match the pinned digest, so an override is a
/// mirror of the published artifact rather than a way around it.
async fn copy_operator_kernel(supplied: &Path, staged: &Path) -> Result<(), ResolveError> {
    let metadata = tokio::fs::symlink_metadata(supplied)
        .await
        .map_err(|source| kernel_io("inspect operator kernel", supplied.to_owned(), source))?;
    if !metadata.file_type().is_file() {
        return Err(kernel_io(
            "inspect operator kernel",
            supplied.to_owned(),
            io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"),
        ));
    }
    if metadata.len() > KERNEL_MAX_BYTES {
        return Err(kernel_io(
            "inspect operator kernel",
            supplied.to_owned(),
            io::Error::other(format!(
                "{} bytes exceeds the {KERNEL_MAX_BYTES}-byte limit",
                metadata.len()
            )),
        ));
    }
    tokio::fs::copy(supplied, staged)
        .await
        .map_err(|source| kernel_io("copy operator kernel", supplied.to_owned(), source))?;
    Ok(())
}

/// Streams the published package to disk, hashing it as it lands.
///
/// The digest is calculated from the bytes actually written, and the body is
/// cut off at [`PACKAGE_MAX_BYTES`], so neither a repointed object nor an
/// endless response can be unpacked.
async fn download_package(
    url: &str,
    destination: &Path,
    pinned: &PinnedKernel<'_>,
) -> Result<(), ResolveError> {
    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|source| cache_io("create kernel cache", parent.to_owned(), source))?;
    }
    let client = reqwest::Client::builder()
        .user_agent(concat!("firecrab-api/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(CONNECT_TIMEOUT)
        // Unlike a total request timeout, this resets whenever another chunk
        // arrives, so a slow link may take as long as the package needs while
        // a registry that stops answering still fails an import predictably.
        .read_timeout(READ_TIMEOUT)
        .build()
        .map_err(|error| download_failed(url, error.to_string()))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| download_failed(url, error.to_string()))?;
    if !response.status().is_success() {
        return Err(download_failed(
            url,
            format!("HTTP {}", response.status().as_u16()),
        ));
    }

    let mut file = tokio::fs::File::create(destination)
        .await
        .map_err(|source| cache_io("stage kernel package", destination.to_owned(), source))?;
    let mut hasher = Sha256::new();
    let mut written = 0_u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| download_failed(url, error.to_string()))?;
        written = written.saturating_add(chunk.len() as u64);
        if written > PACKAGE_MAX_BYTES {
            return Err(download_failed(
                url,
                format!("body exceeds the {PACKAGE_MAX_BYTES}-byte limit"),
            ));
        }
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|source| cache_io("write kernel package", destination.to_owned(), source))?;
    }
    file.flush()
        .await
        .map_err(|source| cache_io("flush kernel package", destination.to_owned(), source))?;

    let actual = digest_from(hasher);
    let expected = parse_pin(pinned.package_digest)?;
    if actual != expected {
        return Err(ResolveError::DigestMismatch {
            subject: url.to_owned(),
            expected,
            actual,
        });
    }
    Ok(())
}

/// Lifts the pinned kernel out of a verified package.
async fn lift_kernel(
    package: &Path,
    key: &str,
    pinned: &PinnedKernel<'_>,
    staged: &Path,
) -> Result<(), ResolveError> {
    let reported = package.to_owned();
    let package = package.to_owned();
    let key = key.to_owned();
    let member = package_member(pinned);
    let staged = staged.to_owned();
    tokio::task::spawn_blocking(move || lift_kernel_blocking(&package, &key, &member, &staged))
        .await
        .map_err(|error| {
            cache_io(
                "join kernel unpack worker",
                reported,
                io::Error::other(error),
            )
        })?
}

/// Blocking half of [`lift_kernel`].
fn lift_kernel_blocking(
    package: &Path,
    key: &str,
    member: &str,
    staged: &Path,
) -> Result<(), ResolveError> {
    let file = std::fs::File::open(package)
        .map_err(|source| cache_io("open kernel package", package.to_owned(), source))?;
    let decoder = zstd::stream::read::Decoder::new(file)
        .map_err(|source| cache_io("decompress kernel package", package.to_owned(), source))?;
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|source| cache_io("read kernel package", package.to_owned(), source))?;
    for entry in entries {
        let mut entry =
            entry.map_err(|source| cache_io("read kernel package", package.to_owned(), source))?;
        let path = entry
            .path()
            .map_err(|source| cache_io("read kernel package", package.to_owned(), source))?;
        if path.to_string_lossy().replace('\\', "/") != member {
            continue;
        }
        let mut target = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .custom_flags(libc::O_CLOEXEC)
            .open(staged)
            .map_err(|source| cache_io("stage OCI kernel", staged.to_owned(), source))?;
        let copied = io::copy(&mut entry.by_ref().take(KERNEL_MAX_BYTES + 1), &mut target)
            .map_err(|source| cache_io("write OCI kernel", staged.to_owned(), source))?;
        if copied > KERNEL_MAX_BYTES {
            return Err(cache_io(
                "write OCI kernel",
                staged.to_owned(),
                io::Error::other(format!(
                    "member {member} exceeds the {KERNEL_MAX_BYTES}-byte limit"
                )),
            ));
        }
        target
            .sync_all()
            .map_err(|source| cache_io("write OCI kernel", staged.to_owned(), source))?;
        return Ok(());
    }
    Err(ResolveError::KernelPackageMemberMissing {
        package: key.to_owned(),
        member: member.to_owned(),
    })
}

/// Proves a file is the pinned kernel and one this host can boot.
async fn verify_kernel(
    path: &Path,
    pair: &KernelBootPair,
    pinned: &PinnedKernel<'_>,
) -> Result<(), ResolveError> {
    let expected = parse_pin(pinned.image_digest)?;
    let subject = pair.kernel.clone();
    let architecture = pair.architecture;
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        verify_kernel_blocking(&path, &subject, architecture, expected)
    })
    .await
    .map_err(|error| {
        cache_io(
            "join kernel verification worker",
            pair.kernel.clone(),
            io::Error::other(error),
        )
    })?
}

/// Blocking half of [`verify_kernel`].
fn verify_kernel_blocking(
    path: &Path,
    subject: &Path,
    architecture: Architecture,
    expected: Sha256Digest,
) -> Result<(), ResolveError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| kernel_io("open kernel", subject.to_owned(), source))?;
    let mut hasher = Sha256::new();
    let mut header = Vec::new();
    let mut buffer = vec![0_u8; 128 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| kernel_io("read kernel", subject.to_owned(), source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        if header.len() < 64 {
            let wanted = (64 - header.len()).min(read);
            header.extend_from_slice(&buffer[..wanted]);
        }
    }
    let actual = digest_from(hasher);
    if actual != expected {
        return Err(ResolveError::DigestMismatch {
            subject: subject.display().to_string(),
            expected,
            actual,
        });
    }
    verify_kernel_architecture(subject, &header, architecture)
}

/// Parses a compiled pin, which is a constant and must always be well formed.
fn parse_pin(value: &str) -> Result<Sha256Digest, ResolveError> {
    Sha256Digest::parse(value).map_err(|error| {
        ResolveError::Malformed(format!("pinned OCI kernel digest {value:?}: {error}"))
    })
}

/// Wraps a finished hasher as a descriptor digest.
fn digest_from(hasher: Sha256) -> Sha256Digest {
    Sha256Digest::parse(&format!("sha256:{:x}", hasher.finalize()))
        .expect("a hex sha256 digest is a valid descriptor digest")
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

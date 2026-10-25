//! Pairs a packed OCI ext4 with the kernel and boot args TemplateSpec needs.
//!
//! An OCI image carries neither. A container tree also has no `/lib/modules`,
//! so the kernel must have the Firecracker boot path built in — `virtio_blk`,
//! `virtio_net`, `virtio_mmio`, and `ext4`. Distro kernels that keep those as
//! modules need their matching initrd, and that initrd would take PID 1 away
//! from the injected guest init.
//!
//! Firecrab builds the latest kernel.org stable source on this host. Verified
//! cached kernels and installed catalog images keep offline hosts importing.
//! This stage records the pair and registers nothing.

use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt as _;

use super::*;

use crate::m2image_manifest;
use crate::templates::{KernelFormat, kernel_architecture};

/// Kernel, optional initrd, and command line later registration can copy
/// into a [`crate::templates::TemplateSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KernelBootPair {
    /// Architecture the kernel boots, which must be this host's.
    pub architecture: Architecture,
    /// Upstream release, operator override, or catalog image named in errors.
    pub source_alias: String,
    /// Image-root-relative kernel path.
    pub kernel: PathBuf,
    /// Image-root-relative initrd path; always `None` for an OCI import.
    pub initrd: Option<PathBuf>,
    /// Firecracker command line the paired rootfs boots with.
    pub boot_args: String,
}

/// The architecture-matched pair this host will boot imported images with.
pub(super) async fn host_kernel_pair(image_root: &Path) -> Result<KernelBootPair, ResolveError> {
    resolve_kernel_pair(
        image_root,
        Architecture::HOST,
        kernel::configured_kernel_override().as_deref(),
        kernel::configured_releases_url().as_deref(),
    )
    .await
}

/// Builds the current stable kernel, with verified local kernels as fallback.
pub(super) async fn resolve_kernel_pair(
    image_root: &Path,
    architecture: Architecture,
    override_path: Option<&Path>,
    releases_url: Option<&str>,
) -> Result<KernelBootPair, ResolveError> {
    let error =
        match kernel::ensure_host_kernel(image_root, architecture, override_path, releases_url)
            .await
        {
            Ok(pair) => return Ok(pair),
            Err(error) => error,
        };
    let Some(installed) = installed_catalog_pair(image_root, architecture) else {
        return Err(error);
    };
    tracing::warn!(
        error = %error,
        alias = installed.source_alias,
        kernel = %installed.kernel.display(),
        "falling back to an installed catalog kernel for this OCI import"
    );
    Ok(installed)
}

/// The catalog pair for `architecture` when its kernel is actually installed.
fn installed_catalog_pair(image_root: &Path, architecture: Architecture) -> Option<KernelBootPair> {
    let pair = catalog_kernel_pair(architecture).ok()?;
    image_root.join(&pair.kernel).is_file().then_some(pair)
}

/// First catalog artifact for `architecture` that needs no initrd.
pub(super) fn catalog_kernel_pair(
    architecture: Architecture,
) -> Result<KernelBootPair, ResolveError> {
    m2image_manifest::load()
        .images
        .into_iter()
        .find_map(|image| {
            let artifact = image.artifacts.get(architecture.as_str())?;
            if artifact.initrd.is_some() {
                return None;
            }
            Some(KernelBootPair {
                architecture,
                source_alias: image.alias,
                kernel: PathBuf::from(&artifact.kernel),
                initrd: None,
                boot_args: artifact.boot_args.clone(),
            })
        })
        .ok_or(ResolveError::NoHostKernel { architecture })
}

/// Records a resolved kernel and its boot args on a packed ext4.
///
/// The kernel file must already exist under `image_root`. Its header is
/// classified with the same rules registration uses. A mismatch is refused
/// here so a later TemplateSpec is never pointed at a silent-hang kernel.
/// The ext4 is not moved or deleted, and nothing is registered.
pub(super) fn pair_ext4_with_kernel(
    image: OciExt4Image,
    image_root: &Path,
    pair: &KernelBootPair,
) -> Result<OciBootableImage, ResolveError> {
    let kernel_path = image_root.join(&pair.kernel);
    let header = read_kernel_header(&kernel_path, pair)?;
    verify_kernel_architecture(&pair.kernel, &header, pair.architecture)?;
    Ok(OciBootableImage {
        rootfs: image,
        kernel: pair.kernel.clone(),
        initrd: pair.initrd.clone(),
        boot_args: pair.boot_args.clone(),
        architecture: pair.architecture,
    })
}

/// Refuses a kernel header this host cannot boot.
///
/// `path` only names the subject in the error, so both the pairing stage and
/// the kernel cache report the same rejections for the same bytes.
pub(super) fn verify_kernel_architecture(
    path: &Path,
    header: &[u8],
    architecture: Architecture,
) -> Result<(), ResolveError> {
    match kernel_architecture(header) {
        KernelFormat::Bootable(found) if found == architecture => Ok(()),
        KernelFormat::Bootable(found) => Err(ResolveError::KernelArchitectureMismatch {
            path: path.to_owned(),
            found,
            host: architecture,
        }),
        KernelFormat::Foreign { machine } => Err(ResolveError::UnsupportedKernelArchitecture {
            path: path.to_owned(),
            machine,
        }),
        KernelFormat::Unrecognized => Err(ResolveError::KernelUnrecognized {
            path: path.to_owned(),
        }),
    }
}

fn read_kernel_header(path: &Path, pair: &KernelBootPair) -> Result<Vec<u8>, ResolveError> {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(ResolveError::KernelMissing {
                path: pair.kernel.clone(),
                hint: pair.source_alias.clone(),
                architecture: pair.architecture,
            });
        }
        Err(source) => {
            return Err(ResolveError::KernelIo {
                operation: "open kernel",
                path: pair.kernel.clone(),
                source,
            });
        }
    };
    let mut header = Vec::new();
    file.take(64)
        .read_to_end(&mut header)
        .map_err(|source| ResolveError::KernelIo {
            operation: "read kernel header",
            path: pair.kernel.clone(),
            source,
        })?;
    Ok(header)
}

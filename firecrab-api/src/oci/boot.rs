//! Pairs a packed OCI ext4 with the kernel and boot args TemplateSpec needs.
//!
//! An OCI image carries neither. A container tree also has no `/lib/modules`,
//! so the kernel must have the Firecracker boot path built in — `virtio_blk`,
//! `virtio_net`, `virtio_mmio`, and `ext4`. Distro kernels that keep those as
//! modules need their matching initrd, and that initrd would take PID 1 away
//! from the injected guest init. The compiled-in catalog's first host
//! artifact without an initrd is the Ubuntu kernel; this stage records that
//! pair and does not register a template.

use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt as _;

use super::*;

use crate::m2image_manifest;
use crate::templates::{KernelFormat, kernel_architecture};

/// Kernel, optional initrd, and command line later registration can copy
/// into a [`crate::templates::TemplateSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KernelBootPair {
    pub architecture: Architecture,
    pub source_alias: String,
    pub kernel: PathBuf,
    pub initrd: Option<PathBuf>,
    pub boot_args: String,
}

/// The architecture-matched pair this host will boot imported images with.
pub(super) fn host_kernel_pair() -> Result<KernelBootPair, ResolveError> {
    kernel_pair_for(Architecture::HOST)
}

/// First catalog artifact for `architecture` that needs no initrd.
pub(super) fn kernel_pair_for(architecture: Architecture) -> Result<KernelBootPair, ResolveError> {
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

/// Records the host catalog kernel and its boot args on a packed ext4.
///
/// The kernel file must already exist under `image_root`. Its header is
/// classified with the same rules registration uses. A mismatch is refused
/// here so a later TemplateSpec is never pointed at a silent-hang kernel.
/// The ext4 is not moved or deleted, and nothing is registered.
pub(super) fn pair_ext4_with_host_kernel(
    image: OciExt4Image,
    image_root: &Path,
) -> Result<OciBootableImage, ResolveError> {
    let pair = host_kernel_pair()?;
    let kernel_path = image_root.join(&pair.kernel);
    let header = read_kernel_header(&kernel_path, &pair)?;
    match kernel_architecture(&header) {
        KernelFormat::Bootable(found) if found == pair.architecture => Ok(OciBootableImage {
            rootfs: image,
            kernel: pair.kernel,
            initrd: pair.initrd,
            boot_args: pair.boot_args,
            architecture: found,
        }),
        KernelFormat::Bootable(found) => Err(ResolveError::KernelArchitectureMismatch {
            path: pair.kernel,
            found,
            host: pair.architecture,
        }),
        KernelFormat::Foreign { machine } => Err(ResolveError::UnsupportedKernelArchitecture {
            path: pair.kernel,
            machine,
        }),
        KernelFormat::Unrecognized => Err(ResolveError::KernelUnrecognized { path: pair.kernel }),
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

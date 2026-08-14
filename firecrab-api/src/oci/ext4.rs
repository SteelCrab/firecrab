//! Sizes and writes an ext4 image from a provisioned OCI tree.

use super::*;

use std::fs::{self, OpenOptions};
use std::os::unix::fs::MetadataExt as _;
use std::sync::atomic::{AtomicU8, Ordering};

pub(super) const HEADROOM_BYTES: u64 = 32 * 1024 * 1024;
pub(super) const MIN_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
/// Free space that must remain after `mkfs.ext4 -d`.
///
/// Smaller than [`HEADROOM_BYTES`] on purpose: that constant is a sizing
/// allowance, and the formatter spends some of it on the journal and inode
/// table. Requiring the full 32 MiB back would reject every image whose
/// planned size is only slightly above 32 MiB. One mebibyte still means the
/// filesystem is not full.
pub(super) const MIN_FREE_AFTER_PACK: u64 = 1024 * 1024;
const METADATA_NUMERATOR: u64 = 25;
const METADATA_DENOMINATOR: u64 = 100;
const ALIGN_BYTES: u64 = 1024 * 1024;
pub(super) const DEFAULT_MAX_ROOTFS_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const MAX_ROOTFS_BYTES_ENV: &str = "FIRECRAB_OCI_MAX_ROOTFS_BYTES";
const WRITE_ACTIVE: u8 = 0;
const WRITE_PUBLISHING: u8 = 1;
const WRITE_CANCELLED: u8 = 2;
const WRITE_FINISHED: u8 = 3;

pub(super) fn plan_ext4_size(payload_bytes: u64) -> u64 {
    let metadata = payload_bytes.saturating_mul(METADATA_NUMERATOR) / METADATA_DENOMINATOR;
    let needed = payload_bytes
        .saturating_add(metadata)
        .saturating_add(HEADROOM_BYTES);
    let aligned = needed
        .saturating_add(ALIGN_BYTES - 1)
        .saturating_div(ALIGN_BYTES)
        .saturating_mul(ALIGN_BYTES);
    aligned.max(MIN_IMAGE_BYTES)
}

pub(super) fn measure_tree_payload(tree: &Path) -> Result<u64, ResolveError> {
    let mut total = 0_u64;
    let mut seen_inodes = std::collections::HashSet::new();
    let mut pending = vec![tree.to_owned()];
    while let Some(path) = pending.pop() {
        let entries = std::fs::read_dir(&path)
            .map_err(|source| ext4_io("read tree", path.clone(), source))?;
        for entry in entries {
            let entry = entry.map_err(|source| ext4_io("read tree entry", path.clone(), source))?;
            let child = entry.path();
            let metadata = std::fs::symlink_metadata(&child)
                .map_err(|source| ext4_io("stat tree entry", child.clone(), source))?;
            if metadata.file_type().is_dir() {
                pending.push(child);
                continue;
            }
            if metadata.file_type().is_symlink() {
                let target = std::fs::read_link(&child)
                    .map_err(|source| ext4_io("read tree symlink", child.clone(), source))?;
                total = total.saturating_add(target.as_os_str().len() as u64);
                continue;
            }
            if !metadata.file_type().is_file() {
                continue;
            }
            let inode = (metadata.dev(), metadata.ino());
            if seen_inodes.insert(inode) {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(total)
}

fn ext4_io(operation: &'static str, path: PathBuf, source: io::Error) -> ResolveError {
    ResolveError::Ext4Io {
        operation,
        path,
        source,
    }
}

/// Shared cancellation state consulted between ext4 write steps.
struct Ext4Control {
    /// Current phase, shared with the caller's cancellation guard.
    state: std::sync::Arc<AtomicU8>,
    /// Destination reported by cancellation errors.
    destination: PathBuf,
}

impl Ext4Control {
    /// Fails once the caller stopped waiting, so a long pack stops early.
    fn check(&self) -> Result<(), ResolveError> {
        if self.state.load(Ordering::Acquire) == WRITE_ACTIVE {
            Ok(())
        } else {
            Err(ResolveError::Ext4Cancelled {
                path: self.destination.clone(),
            })
        }
    }

    /// Claims the right to publish, locking cancellation out from here on.
    fn begin_publish(&self) -> Result<(), ResolveError> {
        self.state
            .compare_exchange(
                WRITE_ACTIVE,
                WRITE_PUBLISHING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|_| ResolveError::Ext4Cancelled {
                path: self.destination.clone(),
            })
    }

    /// Records that publishing finished and the image is the caller's.
    fn finish(&self) {
        self.state.store(WRITE_FINISHED, Ordering::Release);
    }
}

/// Marks an abandoned write cancelled when the caller's future is dropped.
struct CancelExt4OnDrop {
    /// Shared phase, flipped to cancelled while still armed.
    state: std::sync::Arc<AtomicU8>,
    /// Cleared once the blocking worker has returned.
    armed: bool,
}

impl Drop for CancelExt4OnDrop {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.state.compare_exchange(
                WRITE_ACTIVE,
                WRITE_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }
}

pub(super) async fn write_provisioned_ext4(
    rootfs: &ProvisionedRootfs,
    destination: &Path,
) -> Result<OciExt4Image, ResolveError> {
    write_provisioned_ext4_with_limit(rootfs, destination, configured_max_rootfs_bytes()).await
}

pub(super) async fn write_provisioned_ext4_with_limit(
    rootfs: &ProvisionedRootfs,
    destination: &Path,
    limit: u64,
) -> Result<OciExt4Image, ResolveError> {
    let payload = measure_tree_payload(rootfs.path())?;
    let size = plan_ext4_size(payload);
    if size > limit {
        return Err(ResolveError::Ext4TooLarge {
            path: destination.to_owned(),
            size_bytes: size,
            limit,
        });
    }
    write_measured_ext4(rootfs, destination, size, payload).await
}

pub(super) async fn write_provisioned_ext4_with_size(
    rootfs: &ProvisionedRootfs,
    destination: &Path,
    image_bytes: u64,
) -> Result<OciExt4Image, ResolveError> {
    let payload = measure_tree_payload(rootfs.path())?;
    write_measured_ext4(rootfs, destination, image_bytes, payload).await
}

async fn write_measured_ext4(
    rootfs: &ProvisionedRootfs,
    destination: &Path,
    image_bytes: u64,
    payload_bytes: u64,
) -> Result<OciExt4Image, ResolveError> {
    match fs::symlink_metadata(destination) {
        Ok(_) => {
            return Err(ResolveError::Ext4DestinationExists {
                path: destination.to_owned(),
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(ext4_io(
                "inspect destination",
                destination.to_owned(),
                source,
            ));
        }
    }

    let state = std::sync::Arc::new(AtomicU8::new(WRITE_ACTIVE));
    let control = Ext4Control {
        state: state.clone(),
        destination: destination.to_owned(),
    };
    let mut cancel_on_drop = CancelExt4OnDrop { state, armed: true };
    let tree = rootfs.path().to_owned();
    let dest = destination.to_owned();
    let toolbox = rootfs.toolbox_digest().clone();
    let result = tokio::task::spawn_blocking(move || {
        write_blocking(&tree, &dest, image_bytes, payload_bytes, toolbox, &control)
    })
    .await
    .map_err(|error| {
        ext4_io(
            "join worker",
            destination.to_owned(),
            io::Error::other(error),
        )
    });
    cancel_on_drop.armed = false;
    result?
}

fn write_blocking(
    tree: &Path,
    destination: &Path,
    image_bytes: u64,
    payload_bytes: u64,
    toolbox: Sha256Digest,
    control: &Ext4Control,
) -> Result<OciExt4Image, ResolveError> {
    control.check()?;
    if let Some(parent) = destination.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|source| ext4_io("create destination parent", parent.to_owned(), source))?;
    }

    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let partial = parent.join(format!(".firecrab-oci-ext4-{}.partial", Uuid::new_v4()));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .map_err(|source| ext4_io("create partial image", partial.clone(), source))?;
    file.set_len(image_bytes)
        .map_err(|source| ext4_io("size partial image", partial.clone(), source))?;
    drop(file);
    let mut cleanup = PartialCleanup::new(partial.clone());

    control.check()?;
    run_mkfs(tree, &partial, destination)?;
    control.check()?;
    let free_bytes = inspect_free_bytes(&partial, destination)?;
    if free_bytes < MIN_FREE_AFTER_PACK {
        return Err(ResolveError::Ext4Full {
            path: destination.to_owned(),
            size_bytes: image_bytes,
            free_bytes,
            required_bytes: MIN_FREE_AFTER_PACK,
        });
    }

    control.begin_publish()?;
    fs::rename(&partial, destination)
        .map_err(|source| ext4_io("publish ext4 image", destination.to_owned(), source))?;
    cleanup.published = true;
    control.finish();
    Ok(OciExt4Image {
        path: destination.to_owned(),
        size_bytes: image_bytes,
        payload_bytes,
        free_bytes,
        toolbox,
    })
}

fn run_mkfs(tree: &Path, image: &Path, destination: &Path) -> Result<(), ResolveError> {
    let mut command = std::process::Command::new("mkfs.ext4");
    command.args(["-F", "-q", "-m", "0", "-L", "rootfs"]);
    command.args(orphan_file_args());
    command.arg("-d").arg(tree).arg(image);
    let output = command
        .output()
        .map_err(|source| ext4_io("run mkfs.ext4", image.to_owned(), source))?;
    if output.status.success() {
        return Ok(());
    }
    let mut detail = String::from_utf8_lossy(&output.stderr).into_owned();
    if detail.trim().is_empty() {
        detail = String::from_utf8_lossy(&output.stdout).into_owned();
    }
    if detail.trim().is_empty() {
        detail = format!("mkfs.ext4 exited with status {}", output.status);
    }
    Err(ResolveError::Ext4Build {
        path: destination.to_owned(),
        detail: detail.trim().to_owned(),
    })
}

fn inspect_free_bytes(image: &Path, destination: &Path) -> Result<u64, ResolveError> {
    let output = std::process::Command::new("tune2fs")
        .arg("-l")
        .arg(image)
        .output()
        .map_err(|source| ext4_io("run tune2fs", image.to_owned(), source))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(ResolveError::Ext4Build {
            path: destination.to_owned(),
            detail: format!("tune2fs failed: {}", detail.trim()),
        });
    }
    parse_tune2fs_free_bytes(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
        ResolveError::Ext4Build {
            path: destination.to_owned(),
            detail: "tune2fs did not report free space".to_owned(),
        }
    })
}

fn parse_tune2fs_free_bytes(text: &str) -> Option<u64> {
    let mut block_size = None;
    let mut free_blocks = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key.trim() {
            "Block size" => block_size = value.trim().parse::<u64>().ok(),
            "Free blocks" => free_blocks = value.trim().parse::<u64>().ok(),
            _ => {}
        }
    }
    Some(block_size? * free_blocks?)
}

fn orphan_file_args() -> Vec<&'static str> {
    match std::fs::read_to_string("/etc/mke2fs.conf") {
        Ok(text)
            if text
                .split_whitespace()
                .any(|word| word.contains("orphan_file")) =>
        {
            vec!["-O", "^orphan_file"]
        }
        _ => Vec::new(),
    }
}

fn configured_max_rootfs_bytes() -> u64 {
    match std::env::var(MAX_ROOTFS_BYTES_ENV) {
        Ok(value) => match value.trim().parse::<u64>() {
            Ok(limit) if limit > 0 => limit,
            _ => {
                tracing::warn!(
                    variable = MAX_ROOTFS_BYTES_ENV,
                    value,
                    default = DEFAULT_MAX_ROOTFS_BYTES,
                    "invalid OCI ext4 image limit; using default"
                );
                DEFAULT_MAX_ROOTFS_BYTES
            }
        },
        Err(std::env::VarError::NotPresent) => DEFAULT_MAX_ROOTFS_BYTES,
        Err(std::env::VarError::NotUnicode(_)) => {
            tracing::warn!(
                variable = MAX_ROOTFS_BYTES_ENV,
                default = DEFAULT_MAX_ROOTFS_BYTES,
                "non-Unicode OCI ext4 image limit; using default"
            );
            DEFAULT_MAX_ROOTFS_BYTES
        }
    }
}

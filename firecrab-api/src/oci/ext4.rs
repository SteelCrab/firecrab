//! Sizes and writes an ext4 image from a provisioned OCI tree.

use super::*;

use std::os::unix::fs::MetadataExt as _;

pub(super) const HEADROOM_BYTES: u64 = 32 * 1024 * 1024;
pub(super) const MIN_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const METADATA_NUMERATOR: u64 = 25;
const METADATA_DENOMINATOR: u64 = 100;
const ALIGN_BYTES: u64 = 1024 * 1024;
pub(super) const DEFAULT_MAX_ROOTFS_BYTES: u64 = 32 * 1024 * 1024 * 1024;

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

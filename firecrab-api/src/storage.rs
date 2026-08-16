//! Admin-registered storage roots for VM disks.
//!
//! Paths come only from `FIRECRAB_STORAGE_ROOTS` (never from user input).
//! Create requests select a root by **id**; the resolved path is always
//! `{root}/vms/{vm-id}/`. Unset env → a single `default` root at `data/`
//! (so disks stay at `data/vms/…`, matching every prior release).

use std::env;
use std::ffi::CString;
use std::path::{Path, PathBuf};

use firecrab_api_types::{StorageDeviceResponse, StorageRootResponse};
use libc;
use thiserror::Error;

/// Id of the implicit single root when `FIRECRAB_STORAGE_ROOTS` is unset.
pub const DEFAULT_ROOT_ID: &str = "default";
/// Directory name under each storage root that holds per-VM state.
const VMS_SUBDIR: &str = "vms";

/// One admin-registered place VMs may put their disks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageRoot {
    /// Stable id clients send in `CreateVmRequest.storage_root`.
    pub id: String,
    /// Absolute or cwd-relative mount path registered by the operator.
    pub path: PathBuf,
}

/// All registered storage roots. Never empty after construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageRegistry {
    roots: Vec<StorageRoot>,
}

/// Why a free-space or id lookup failed.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum StorageError {
    /// Client asked for an id that isn't in the registry.
    #[error("unknown storage root {0:?}")]
    UnknownRoot(String),
    /// `statvfs` failed (path missing, permission, …).
    #[error("could not measure free space on {path}: {detail}")]
    FreeSpace {
        /// Path that was probed.
        path: PathBuf,
        /// OS / libc detail.
        detail: String,
    },
}

impl StorageRegistry {
    /// Parses `FIRECRAB_STORAGE_ROOTS` or falls back to the single default root.
    pub fn from_env() -> Self {
        match env::var("FIRECRAB_STORAGE_ROOTS") {
            Ok(value) if !value.trim().is_empty() => Self::parse(&value).unwrap_or_else(|error| {
                tracing::warn!(
                    error = %error,
                    "invalid FIRECRAB_STORAGE_ROOTS; using default root only"
                );
                Self::default_single()
            }),
            _ => Self::default_single(),
        }
    }

    /// One root whose `vms/` child is [`crate::rootfs::default_vms_dir`]
    /// (`data/vms`), so existing installs keep the same on-disk layout.
    pub fn default_single() -> Self {
        let vms = crate::rootfs::default_vms_dir();
        let path = vms
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("data"));
        Self::single(DEFAULT_ROOT_ID, path)
    }

    /// Builds a registry with exactly one root (tests, overrides).
    pub fn single(id: impl Into<String>, path: PathBuf) -> Self {
        Self {
            roots: vec![StorageRoot {
                id: id.into(),
                path,
            }],
        }
    }

    /// `id=path` pairs separated by `:`. Path may contain `=` only after the
    /// first one; `:` in a path is not supported (document as such).
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut roots = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for entry in spec.split(':') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let (id, path) = entry
                .split_once('=')
                .ok_or_else(|| format!("expected id=path, got {entry:?}"))?;
            let id = id.trim();
            let path = path.trim();
            if id.is_empty() || path.is_empty() {
                return Err(format!("empty id or path in {entry:?}"));
            }
            if !valid_root_id(id) {
                return Err(format!(
                    "storage root id {id:?} must be 1-64 alphanumeric, '.', '_' or '-'"
                ));
            }
            if !seen.insert(id.to_owned()) {
                return Err(format!("duplicate storage root id {id:?}"));
            }
            roots.push(StorageRoot {
                id: id.to_owned(),
                path: PathBuf::from(path),
            });
        }
        if roots.is_empty() {
            return Err("no storage roots in FIRECRAB_STORAGE_ROOTS".to_owned());
        }
        Ok(Self { roots })
    }

    /// Every registered root, in registration order.
    pub fn roots(&self) -> &[StorageRoot] {
        &self.roots
    }

    /// Id used when `CreateVmRequest.storage_root` is omitted.
    pub fn default_id(&self) -> &str {
        &self.roots[0].id
    }

    /// `{root}/vms` for the default root (host-status, legacy callers).
    pub fn default_vms_dir(&self) -> PathBuf {
        self.roots[0].path.join(VMS_SUBDIR)
    }

    /// Looks up a root by id.
    pub fn get(&self, id: &str) -> Option<&StorageRoot> {
        self.roots.iter().find(|root| root.id == id)
    }

    /// `{root}/vms` for `id`, or an error if the id is unknown.
    pub fn vms_dir(&self, id: &str) -> Result<PathBuf, StorageError> {
        self.get(id)
            .map(|root| root.path.join(VMS_SUBDIR))
            .ok_or_else(|| StorageError::UnknownRoot(id.to_owned()))
    }

    /// Wire list with live free-space numbers (best-effort zeros if unreadable).
    pub fn list_responses(&self) -> Vec<StorageRootResponse> {
        self.roots
            .iter()
            .map(|root| {
                let (total_gib, available_gib) =
                    available_and_total_gib(&root.path).unwrap_or((0, 0));
                let kind = if root.id == DEFAULT_ROOT_ID && self.roots.len() == 1 {
                    "default"
                } else if root.id == DEFAULT_ROOT_ID {
                    "default"
                } else {
                    "env"
                };
                StorageRootResponse {
                    id: root.id.clone(),
                    name: root.id.clone(),
                    path: root.path.display().to_string(),
                    available_gib,
                    total_gib,
                    kind: kind.to_owned(),
                }
            })
            .collect()
    }

    /// Live free/total GiB for an arbitrary path (MicroStorage rows).
    pub fn space_for(path: &Path) -> (u64, u64) {
        available_and_total_gib(path).unwrap_or((0, 0))
    }

    /// Free-space check against an explicit path (MicroStorage).
    pub fn ensure_capacity_at(path: &Path, need_bytes: u64) -> Result<(), StorageError> {
        let available = available_bytes(path)?;
        if available < need_bytes {
            return Err(StorageError::FreeSpace {
                path: path.to_owned(),
                detail: format!("need {need_bytes} bytes free, have {available}"),
            });
        }
        Ok(())
    }

    /// Ensures `id` is registered and has at least `need_bytes` free.
    pub fn ensure_capacity(&self, id: &str, need_bytes: u64) -> Result<(), StorageError> {
        let root = self
            .get(id)
            .ok_or_else(|| StorageError::UnknownRoot(id.to_owned()))?;
        let available = available_bytes(&root.path)?;
        if available < need_bytes {
            return Err(StorageError::FreeSpace {
                path: root.path.clone(),
                detail: format!("need {} bytes free, have {available}", need_bytes),
            });
        }
        Ok(())
    }
}

/// Same rules as VM names — short, filesystem-safe ids.
fn valid_root_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Whether `path` is an absolute host directory path safe to register as a
/// MicroStorage (no `..`, not empty). Does not require the path to exist yet.
pub fn validate_storage_path(path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("must not be empty".to_owned());
    }
    let p = PathBuf::from(trimmed);
    if !p.is_absolute() {
        return Err("must be an absolute path".to_owned());
    }
    if p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("must not contain '..'".to_owned());
    }
    Ok(p)
}

/// Free bytes on the filesystem that holds `path` (or its nearest existing
/// ancestor). Creates nothing.
pub fn available_bytes(path: &Path) -> Result<u64, StorageError> {
    let probe = nearest_existing(path);
    statvfs_available(&probe).map_err(|detail| StorageError::FreeSpace {
        path: path.to_owned(),
        detail,
    })
}

/// Mounted host filesystems that operators can register as MicroStorage.
///
/// Reads `/proc/mounts` (no root required). Does **not** create, format, or
/// partition disks — only discovers already-mounted paths. Virtual/pseudo
/// filesystems are filtered out.
pub fn list_mounted_devices() -> Vec<StorageDeviceResponse> {
    let text = match std::fs::read_to_string("/proc/mounts") {
        Ok(text) => text,
        Err(_) => return Vec::new(),
    };
    let lsblk = lsblk_index();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();

    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let device = fields.next().unwrap_or("");
        let mountpoint = fields.next().unwrap_or("");
        let fstype = fields.next().unwrap_or("");
        if mountpoint.is_empty() || !seen.insert(mountpoint.to_owned()) {
            continue;
        }
        if !is_registerable_fstype(fstype) {
            continue;
        }
        // Skip bind-mount style sources that aren't block devices and aren't
        // absolute paths we care about (e.g. `none`, `tmpfs` already filtered).
        if mountpoint == "/" {
            // Root is always present and usually already covered by `default`.
            // Still include it so an operator can see free space; they may
            // register a subdirectory rather than `/` itself.
        }
        let mount = PathBuf::from(mountpoint);
        if !mount.is_absolute() {
            continue;
        }
        let (total_gib, available_gib) = available_and_total_gib(&mount).unwrap_or((0, 0));
        // Skip tiny / unusable mounts (e.g. empty stub mounts).
        if total_gib == 0 && available_gib == 0 {
            continue;
        }
        let (dev_name, kind) = lsblk
            .get(mountpoint)
            .cloned()
            .unwrap_or_else(|| (device_basename(device), String::new()));
        out.push(StorageDeviceResponse {
            device: dev_name,
            mountpoint: mountpoint.to_owned(),
            fstype: fstype.to_owned(),
            size_gib: total_gib,
            available_gib,
            kind,
        });
    }
    out.sort_by(|a, b| a.mountpoint.cmp(&b.mountpoint));
    out
}

fn is_registerable_fstype(fstype: &str) -> bool {
    !matches!(
        fstype,
        "proc"
            | "sysfs"
            | "devtmpfs"
            | "devpts"
            | "tmpfs"
            | "cgroup"
            | "cgroup2"
            | "pstore"
            | "bpf"
            | "tracefs"
            | "debugfs"
            | "securityfs"
            | "hugetlbfs"
            | "mqueue"
            | "fusectl"
            | "configfs"
            | "autofs"
            | "binfmt_misc"
            | "rpc_pipefs"
            | "overlay"
            | "nsfs"
            | "squashfs"
            | "iso9660"
            | "udf"
    )
}

fn device_basename(source: &str) -> String {
    // `/dev/nvme0n1p1` → `nvme0n1p1`; `UUID=…` / `none` stay as-is short form.
    Path::new(source)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(source)
        .to_owned()
}

/// Optional enrichment from `lsblk -J -b -o NAME,TYPE,MOUNTPOINT` when present.
fn lsblk_index() -> std::collections::HashMap<String, (String, String)> {
    let output = match std::process::Command::new("lsblk")
        .args(["-J", "-b", "-o", "NAME,TYPE,MOUNTPOINT"])
        .output()
    {
        Ok(output) if output.status.success() => output.stdout,
        _ => return std::collections::HashMap::new(),
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output) else {
        return std::collections::HashMap::new();
    };
    let mut map = std::collections::HashMap::new();
    collect_lsblk(&value["blockdevices"], &mut map);
    map
}

fn collect_lsblk(
    nodes: &serde_json::Value,
    map: &mut std::collections::HashMap<String, (String, String)>,
) {
    let Some(arr) = nodes.as_array() else {
        return;
    };
    for node in arr {
        let name = node["name"].as_str().unwrap_or("").to_owned();
        let kind = node["type"].as_str().unwrap_or("").to_owned();
        if let Some(mp) = node["mountpoint"].as_str().filter(|s| !s.is_empty()) {
            map.insert(mp.to_owned(), (name.clone(), kind.clone()));
        }
        // lsblk may put mountpoints in an array on newer util-linux.
        if let Some(mps) = node["mountpoints"].as_array() {
            for mp in mps {
                if let Some(path) = mp.as_str().filter(|s| !s.is_empty()) {
                    map.insert(path.to_owned(), (name.clone(), kind.clone()));
                }
            }
        }
        collect_lsblk(&node["children"], map);
    }
}

fn available_and_total_gib(path: &Path) -> Option<(u64, u64)> {
    let probe = nearest_existing(path);
    let (avail, total) = statvfs_bytes(&probe).ok()?;
    const GIB: u64 = 1024 * 1024 * 1024;
    Some((total / GIB, avail / GIB))
}

fn nearest_existing(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    loop {
        if current.exists() {
            return current;
        }
        if !current.pop() {
            return PathBuf::from(".");
        }
    }
}

fn statvfs_available(path: &Path) -> Result<u64, String> {
    statvfs_bytes(path).map(|(avail, _)| avail)
}

fn statvfs_bytes(path: &Path) -> Result<(u64, u64), String> {
    let c_path = CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| "path contains interior NUL".to_owned())?;
    // SAFETY: `c_path` is a valid C string; `buf` is fully overwritten by
    // a successful `statvfs` call before we read it.
    unsafe {
        let mut buf: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut buf) != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let frsize = buf.f_frsize as u64;
        let available = buf.f_bavail as u64 * frsize;
        let total = buf.f_blocks as u64 * frsize;
        Ok((available, total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::assert_matches;

    #[test]
    fn parse_accepts_id_path_pairs() {
        let reg = StorageRegistry::parse("disk-a=/mnt/a:disk-b=/mnt/b").unwrap();
        assert_eq!(reg.roots().len(), 2);
        assert_eq!(reg.default_id(), "disk-a");
        assert_eq!(reg.vms_dir("disk-b").unwrap(), PathBuf::from("/mnt/b/vms"));
    }

    #[test]
    fn parse_rejects_duplicate_ids_and_bad_shape() {
        assert!(StorageRegistry::parse("a=/x:a=/y").is_err());
        assert!(StorageRegistry::parse("nopath").is_err());
        assert!(StorageRegistry::parse("=/path").is_err());
        assert!(StorageRegistry::parse("bad/id=/path").is_err());
    }

    /// Blank entries are skipped (trailing/doubled `:`), but a spec that is
    /// *only* separators leaves nothing to register and must fail rather
    /// than yield a registry with no default root.
    #[test]
    fn parse_skips_blank_entries_and_rejects_an_all_blank_spec() {
        let reg = StorageRegistry::parse("disk-a=/mnt/a::  :disk-b=/mnt/b:").unwrap();
        assert_eq!(reg.roots().len(), 2);

        let error = StorageRegistry::parse(" :: ").unwrap_err();
        assert!(error.contains("no storage roots"), "{error}");
    }

    #[test]
    fn list_responses_labels_the_default_root_alongside_env_roots() {
        let reg = StorageRegistry::parse("default=/mnt/default:disk-b=/mnt/b").unwrap();
        let kinds: Vec<_> = reg
            .list_responses()
            .into_iter()
            .map(|root| (root.id, root.kind))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("default".to_owned(), "default".to_owned()),
                ("disk-b".to_owned(), "env".to_owned()),
            ]
        );
    }

    #[test]
    fn device_basename_shortens_dev_paths_and_leaves_labels_alone() {
        assert_eq!(device_basename("/dev/nvme0n1p1"), "nvme0n1p1");
        assert_eq!(device_basename("none"), "none");
        assert_eq!(device_basename(""), "");
    }

    #[test]
    fn nearest_existing_walks_up_and_falls_back_to_the_cwd() {
        let directory = tempfile::tempdir().unwrap();
        let deep = directory.path().join("a/b/c");
        assert_eq!(nearest_existing(&deep), directory.path());
        // A relative path with no existing component pops itself empty.
        assert_eq!(
            nearest_existing(Path::new("no-such-relative-dir")),
            PathBuf::from(".")
        );
    }

    #[test]
    fn statvfs_reports_an_error_for_a_path_that_does_not_exist() {
        let error = statvfs_bytes(Path::new("/no-such-path-for-firecrab-tests")).unwrap_err();
        assert!(!error.is_empty(), "the libc detail must be surfaced");
    }

    #[test]
    fn default_single_keeps_legacy_data_vms_layout() {
        let reg = StorageRegistry::default_single();
        assert_eq!(reg.default_id(), DEFAULT_ROOT_ID);
        assert_eq!(reg.default_vms_dir(), PathBuf::from("data/vms"));
    }

    #[test]
    fn ensure_capacity_rejects_unknown_root() {
        let reg = StorageRegistry::default_single();
        let result = reg.ensure_capacity("nope", 1);
        assert_matches!(result, Err(StorageError::UnknownRoot(_)));
    }

    #[test]
    fn list_mounted_devices_includes_root_or_something_usable() {
        let devices = list_mounted_devices();
        // Linux CI/dev hosts always have at least one real mount.
        assert!(
            devices
                .iter()
                .any(|d| d.mountpoint == "/" || d.size_gib > 0),
            "{devices:?}"
        );
        assert!(devices.iter().all(|d| d.mountpoint.starts_with('/')));
        assert!(
            !devices
                .iter()
                .any(|d| d.fstype == "proc" || d.fstype == "sysfs")
        );
    }

    #[test]
    fn validate_storage_path_rejects_relative_and_dotdot() {
        assert!(validate_storage_path("relative").is_err());
        assert!(validate_storage_path("/ok/../evil").is_err());
        assert!(validate_storage_path("/tmp/firecrab-pool").is_ok());
    }

    #[test]
    fn available_bytes_reads_real_filesystem() {
        let free = available_bytes(Path::new("/")).unwrap();
        assert!(free > 0);
    }

    #[test]
    fn ensure_capacity_accepts_a_tiny_request_on_root() {
        let reg = StorageRegistry::single("r", PathBuf::from("/"));
        reg.ensure_capacity("r", 1).unwrap();
    }

    #[test]
    fn ensure_capacity_rejects_absurd_size() {
        let reg = StorageRegistry::single("r", PathBuf::from("/"));
        let err = reg.ensure_capacity("r", u64::MAX / 2).unwrap_err();
        assert_matches!(err, StorageError::FreeSpace { .. });
    }
}

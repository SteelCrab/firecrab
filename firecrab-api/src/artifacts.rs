//! Per-VM artifact layout: durable disk generations under `disks/`, and a
//! fresh runtime directory (config, API socket, console log) for every start.
//!
//! Host paths are always derived from the configured vms root +
//! server-generated UUIDs — never from user-supplied absolute paths
//! (`public-docs/storage.md`).

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use thiserror::Error;
use uuid::Uuid;

/// Failure modes for creating runtime directories.
#[derive(Debug, Error)]
pub enum ArtifactError {
    /// Couldn't create a directory under the VM artifact tree.
    #[error("failed to create artifact directory {path}: {source}")]
    CreateDirectory {
        /// Directory that failed.
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Path exists but is not a plain directory (e.g. a symlink).
    #[error("artifact path is not a safe directory: {0}")]
    UnsafeDirectory(PathBuf),
}

/// Per-VM on-disk layout rooted at `{vms_root}/{vm_id}/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmArtifactPaths {
    /// VM's top-level directory.
    pub dir: PathBuf,
    /// Durable writable disks (`{generation}.ext4`).
    pub disks: PathBuf,
    /// Per-start runtime dirs (config / socket / console).
    pub runtimes: PathBuf,
}

/// One start's host-side Firecracker files under `runtimes/{runtime_id}/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRuntimePaths {
    /// Runtime directory itself.
    pub dir: PathBuf,
    /// `firecracker.json`.
    pub config: PathBuf,
    /// API Unix socket.
    pub api_socket: PathBuf,
    /// Tee'd guest console log.
    pub console_log: PathBuf,
}

impl VmArtifactPaths {
    /// Derives the layout for `vm_id` under a storage root's `vms/` directory.
    /// Directory names use the hyphen-free UUID form so paths stay compact and
    /// never embed user-controlled path segments.
    pub fn for_vm(vms_root: &Path, vm_id: Uuid) -> Self {
        let dir = vms_root.join(uuid_dir(vm_id));
        // Short directory names keep the API socket path under Linux's ~108
        // byte AF_UNIX limit once nested under /tmp + UUIDs.
        Self {
            disks: dir.join("d"),
            runtimes: dir.join("r"),
            dir,
        }
    }

    /// Final published rootfs path for a disk generation.
    pub fn rootfs(&self, generation: Uuid) -> PathBuf {
        self.disks.join(format!("{}.ext4", uuid_dir(generation)))
    }

    /// In-progress copy path for a generation (atomic publish target).
    pub fn rootfs_tmp(&self, generation: Uuid) -> PathBuf {
        self.disks.join(format!(".{}.tmp", uuid_dir(generation)))
    }

    /// Paths for one start's runtime identity.
    pub fn runtime(&self, runtime_id: Uuid) -> HostRuntimePaths {
        let dir = self.runtimes.join(uuid_dir(runtime_id));
        HostRuntimePaths {
            // Short names: the socket path must fit AF_UNIX's ~108-byte cap.
            config: dir.join("fc.json"),
            api_socket: dir.join("fc.sock"),
            console_log: dir.join("console.log"),
            dir,
        }
    }

    /// Ensures `dir`, `disks`, and `runtimes` exist as real directories (mode 0700).
    pub fn ensure_directories(&self) -> Result<(), ArtifactError> {
        for path in [&self.dir, &self.disks, &self.runtimes] {
            ensure_directory(path)?;
        }
        Ok(())
    }

    /// Creates a new runtime directory with `create_new` semantics (0700).
    pub fn create_runtime(&self, runtime_id: Uuid) -> Result<HostRuntimePaths, ArtifactError> {
        self.ensure_directories()?;
        let paths = self.runtime(runtime_id);
        fs::create_dir(&paths.dir).map_err(|source| ArtifactError::CreateDirectory {
            path: paths.dir.clone(),
            source,
        })?;
        fs::set_permissions(&paths.dir, fs::Permissions::from_mode(0o700)).map_err(|source| {
            ArtifactError::CreateDirectory {
                path: paths.dir.clone(),
                source,
            }
        })?;
        Ok(paths)
    }
}

/// Compact, path-safe form of a UUID (no hyphens).
pub fn uuid_dir(id: Uuid) -> String {
    id.as_simple().to_string()
}

fn ensure_directory(path: &Path) -> Result<(), ArtifactError> {
    // create_dir_all so `{vms_root}` itself may not exist yet (fresh data root).
    fs::create_dir_all(path).map_err(|source| ArtifactError::CreateDirectory {
        path: path.to_owned(),
        source,
    })?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        ArtifactError::CreateDirectory {
            path: path.to_owned(),
            source,
        }
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|source| ArtifactError::CreateDirectory {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ArtifactError::UnsafeDirectory(path.to_owned()));
    }
    Ok(())
}

/// Removes a VM's entire artifact tree (runtimes + disks). Missing is ok.
pub fn remove_vm_artifacts(paths: &VmArtifactPaths) -> io::Result<()> {
    match fs::remove_dir_all(&paths.dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::assert_matches;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn paths_are_uuid_scoped_and_unique_per_vm() {
        let root = Path::new("/var/lib/firecrab/vms");
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let paths_a = VmArtifactPaths::for_vm(root, a);
        let paths_b = VmArtifactPaths::for_vm(root, b);
        assert_ne!(paths_a.dir, paths_b.dir);
        assert!(paths_a.dir.ends_with(uuid_dir(a)));
        assert!(paths_a.disks.ends_with("d"));
        assert!(paths_a.runtimes.ends_with("r"));

        let generation = Uuid::new_v4();
        assert!(
            paths_a
                .rootfs(generation)
                .ends_with(format!("{}.ext4", uuid_dir(generation)))
        );
        assert!(
            paths_a
                .rootfs_tmp(generation)
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with('.')
        );
    }

    #[test]
    fn each_runtime_id_gets_its_own_directory() {
        let directory = tempfile::tempdir().unwrap();
        let paths = VmArtifactPaths::for_vm(directory.path(), Uuid::new_v4());
        let first = paths.create_runtime(Uuid::new_v4()).unwrap();
        let second = paths.create_runtime(Uuid::new_v4()).unwrap();
        assert_ne!(first.dir, second.dir);
        assert!(first.config.ends_with("fc.json"));
        assert!(first.api_socket.ends_with("fc.sock"));
        assert!(first.console_log.ends_with("console.log"));
        assert!(
            first.api_socket.as_os_str().len() < 108,
            "socket path must fit AF_UNIX: {}",
            first.api_socket.display()
        );
    }

    #[test]
    fn concurrent_vms_get_distinct_disk_inodes_when_prepared() {
        use std::fs::File;
        use std::io::Write;

        let directory = tempfile::tempdir().unwrap();
        let vms_root = directory.path().join("vms");
        fs::create_dir_all(&vms_root).unwrap();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let gen_a = Uuid::new_v4();
        let gen_b = Uuid::new_v4();
        let paths_a = VmArtifactPaths::for_vm(&vms_root, a);
        let paths_b = VmArtifactPaths::for_vm(&vms_root, b);
        paths_a.ensure_directories().unwrap();
        paths_b.ensure_directories().unwrap();

        let mut template = tempfile::NamedTempFile::new().unwrap();
        template.write_all(b"template-bytes").unwrap();
        template.as_file_mut().sync_all().unwrap();

        let mut file_a = File::open(template.path()).unwrap();
        let mut file_b = File::open(template.path()).unwrap();
        let root_a = crate::rootfs::prepare_rootfs(
            &paths_a,
            gen_a,
            &mut file_a,
            b"template-bytes".len() as u64,
        )
        .unwrap();
        let root_b = crate::rootfs::prepare_rootfs(
            &paths_b,
            gen_b,
            &mut file_b,
            b"template-bytes".len() as u64,
        )
        .unwrap();
        assert_ne!(root_a, root_b);
        let meta_a = fs::metadata(&root_a).unwrap();
        let meta_b = fs::metadata(&root_b).unwrap();
        assert_ne!(meta_a.ino(), meta_b.ino());
    }

    #[test]
    fn ensure_directories_fails_when_the_vms_root_is_a_file() {
        let directory = tempfile::tempdir().unwrap();
        let blocker = directory.path().join("vms");
        fs::write(&blocker, b"not a directory").unwrap();

        let paths = VmArtifactPaths::for_vm(&blocker, Uuid::new_v4());
        let error = paths.ensure_directories().unwrap_err();
        assert_matches!(error, ArtifactError::CreateDirectory { ref path, .. } if *path == paths.dir, "{error}");
    }

    /// A symlinked VM directory is refused rather than followed: the tree is
    /// always derived from the storage root, never redirected out of it.
    #[test]
    fn ensure_directories_refuses_a_symlinked_vm_directory() {
        let directory = tempfile::tempdir().unwrap();
        let vms_root = directory.path().join("vms");
        let elsewhere = directory.path().join("elsewhere");
        fs::create_dir_all(&vms_root).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();

        let id = Uuid::new_v4();
        let paths = VmArtifactPaths::for_vm(&vms_root, id);
        std::os::unix::fs::symlink(&elsewhere, &paths.dir).unwrap();

        let error = paths.ensure_directories().unwrap_err();
        assert_matches!(error, ArtifactError::UnsafeDirectory(ref path) if *path == paths.dir, "{error}");
    }

    #[test]
    fn create_runtime_refuses_to_reuse_an_existing_runtime_id() {
        let directory = tempfile::tempdir().unwrap();
        let paths = VmArtifactPaths::for_vm(directory.path(), Uuid::new_v4());
        let runtime_id = Uuid::new_v4();
        paths.create_runtime(runtime_id).unwrap();

        let error = paths.create_runtime(runtime_id).unwrap_err();
        let expected = paths.runtime(runtime_id).dir;
        assert_matches!(error, ArtifactError::CreateDirectory { ref path, .. } if *path == expected, "{error}");
    }

    #[test]
    fn remove_vm_artifacts_ignores_a_missing_tree_but_reports_real_errors() {
        let directory = tempfile::tempdir().unwrap();
        let missing = VmArtifactPaths::for_vm(directory.path(), Uuid::new_v4());
        remove_vm_artifacts(&missing).expect("removing a tree that was never created is fine");

        // A regular file where the VM directory belongs is a real error
        // (ENOTDIR), not the "already gone" case.
        let blocked = VmArtifactPaths::for_vm(directory.path(), Uuid::new_v4());
        fs::write(&blocked.dir, b"not a directory").unwrap();
        let error = remove_vm_artifacts(&blocked).unwrap_err();
        assert_ne!(error.kind(), io::ErrorKind::NotFound, "{error}");
    }
}

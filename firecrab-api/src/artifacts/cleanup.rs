//! Cleanup remains bound to the physical directories observed before admission.

use super::VmArtifactPaths;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use uuid::Uuid;

fn changed() -> io::Error {
    io::Error::other("original VM cleanup storage is unavailable or changed")
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Directory {
    path: PathBuf,
    device: u64,
    inode: u64,
    created_seconds: u64,
    created_nanoseconds: u32,
}

impl Directory {
    fn capture(path: &Path) -> io::Result<Self> {
        let path = fs::canonicalize(path)?;
        let metadata = fs::metadata(&path)?;
        if !metadata.is_dir() || metadata.ino() == 0 {
            return Err(changed());
        }
        let created = metadata
            .created()?
            .duration_since(UNIX_EPOCH)
            .map_err(|_| changed())?;
        if created.is_zero() {
            return Err(changed());
        }
        Ok(Self {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            created_seconds: created.as_secs(),
            created_nanoseconds: created.subsec_nanos(),
        })
    }

    fn optional(path: &Path) -> io::Result<Option<Self>> {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
            Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                Err(changed())
            }
            Ok(_) => Self::capture(path).map(Some),
        }
    }
}

/// Private immutable input. Public observations expose a hash, never the host path.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VmCleanupStorage {
    schema: u8,
    root: Directory,
    vms: Option<Directory>,
    target: Option<Directory>,
    vm: Uuid,
    generation: Option<Uuid>,
}

impl VmCleanupStorage {
    /// Observe existing directories without creating files or consulting a fallback root.
    pub(crate) fn capture(root: &Path, vm: Uuid, generation: Option<Uuid>) -> io::Result<Self> {
        if vm.is_nil() || generation.is_some_and(|id| id.is_nil()) {
            return Err(changed());
        }
        let root = Directory::capture(root)?;
        let vms_path = root.path.join("vms");
        let vms = Directory::optional(&vms_path)?;
        let target = Directory::optional(&VmArtifactPaths::for_vm(&vms_path, vm).dir)?;
        Ok(Self {
            root,
            vms,
            target,
            vm,
            generation,
            schema: 1,
        })
    }

    /// The persistent VM record must still identify the same disk generation.
    pub(crate) fn disk_generation(&self) -> Option<Uuid> {
        self.generation
    }

    /// This physical proof cannot be attached to another cleanup identity.
    pub(crate) fn vm_id(&self) -> Uuid {
        self.vm
    }

    /// This records the observation point, not a live assertion after deletion.
    pub(crate) fn artifacts_existed(&self) -> bool {
        self.target.is_some()
    }

    /// A bounded public review selector contains no path or credential value.
    pub(crate) fn fingerprint(&self) -> io::Result<String> {
        if self.schema != 1 {
            return Err(changed());
        }
        let bytes = serde_json::to_vec(self).map_err(|_| changed())?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }

    /// A missing original tree is a no-op; a replacement tree is never removed.
    fn original_paths(&self, current_root: &Path) -> io::Result<Option<VmArtifactPaths>> {
        if self.schema != 1
            || self.vm.is_nil()
            || self.generation.is_some_and(|id| id.is_nil())
            || Directory::capture(current_root)? != self.root
            || (self.target.is_some() && self.vms.is_none())
        {
            return Err(changed());
        }
        let vms_path = self.root.path.join("vms");
        let current_vms = Directory::optional(&vms_path)?;
        if let (Some(original), Some(current)) = (&self.vms, &current_vms)
            && original != current
        {
            return Err(changed());
        }
        let paths = VmArtifactPaths::for_vm(&vms_path, self.vm);
        let current = Directory::optional(&paths.dir)?;
        match (&self.target, current) {
            (_, None) => Ok(None),
            (Some(original), Some(current)) if original == &current => Ok(Some(paths)),
            _ => Err(changed()),
        }
    }

    /// Validate before any runtime teardown, including when artifacts will be retained.
    pub(crate) fn check(&self, current_root: &Path) -> io::Result<()> {
        self.original_paths(current_root).map(|_| ())
    }

    /// Resource locks and durable admission exclude API writers during this physical check.
    pub(crate) fn remove(&self, current_root: &Path) -> io::Result<()> {
        if let Some(paths) = self.original_paths(current_root)? {
            super::remove_vm_artifacts(&paths)?;
        }
        self.check(current_root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iac_vm_cleanup_storage_preserves_peers_and_original_proof_after_removal() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path();
        let vm = Uuid::new_v4();
        let peer = Uuid::new_v4();
        for id in [vm, peer] {
            let paths = VmArtifactPaths::for_vm(&root.join("vms"), id);
            paths.ensure_directories().unwrap();
            fs::write(paths.dir.join("private-data"), id.as_bytes()).unwrap();
        }
        let pin = VmCleanupStorage::capture(root, vm, Some(Uuid::new_v4())).unwrap();
        let restored: VmCleanupStorage =
            serde_json::from_slice(&serde_json::to_vec(&pin).unwrap()).unwrap();
        assert_eq!(restored.fingerprint().unwrap(), pin.fingerprint().unwrap());
        restored.remove(root).unwrap();
        restored.remove(root).unwrap();
        assert!(!VmArtifactPaths::for_vm(&root.join("vms"), vm).dir.exists());
        assert_eq!(
            fs::read(
                VmArtifactPaths::for_vm(&root.join("vms"), peer)
                    .dir
                    .join("private-data")
            )
            .unwrap(),
            peer.as_bytes()
        );
    }

    #[test]
    fn iac_vm_cleanup_storage_rejects_remapped_root_and_replaced_directory() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("root");
        let other = base.path().join("other");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&other).unwrap();
        let vm = Uuid::new_v4();
        let paths = VmArtifactPaths::for_vm(&root.join("vms"), vm);
        paths.ensure_directories().unwrap();
        let pin = VmCleanupStorage::capture(&root, vm, None).unwrap();
        assert!(pin.remove(&other).is_err());
        fs::rename(&paths.dir, root.join("original-vm")).unwrap();
        fs::create_dir(&paths.dir).unwrap();
        fs::write(paths.dir.join("replacement"), b"retain").unwrap();
        assert!(pin.remove(&root).is_err());
        assert_eq!(fs::read(paths.dir.join("replacement")).unwrap(), b"retain");
    }

    #[test]
    fn iac_vm_cleanup_storage_absent_target_does_not_claim_later_directories() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path();
        let vm = Uuid::new_v4();
        let pin = VmCleanupStorage::capture(root, vm, None).unwrap();
        let peer = VmArtifactPaths::for_vm(&root.join("vms"), Uuid::new_v4());
        peer.ensure_directories().unwrap();
        pin.remove(root).unwrap();
        assert!(peer.dir.exists());
        let paths = VmArtifactPaths::for_vm(&root.join("vms"), vm);
        paths.ensure_directories().unwrap();
        assert!(pin.remove(root).is_err());
        assert!(paths.dir.exists());
    }
}

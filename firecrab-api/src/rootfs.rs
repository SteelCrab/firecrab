use std::fs::{self, File};
use std::io::{self, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use thiserror::Error;
use uuid::Uuid;

const VMS_DIR: &str = "data/vms";
const ROOTFS_FILE_NAME: &str = "rootfs.ext4";
const ROOTFS_TMP_FILE_NAME: &str = "rootfs.ext4.tmp";

#[derive(Debug, Error)]
pub enum RootfsError {
    #[error("failed to create VM directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to inspect rootfs at {path}: {source}")]
    Inspect {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to copy template rootfs into {path}: {source}")]
    Copy {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to publish rootfs at {path}: {source}")]
    Publish {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub fn default_vms_dir() -> PathBuf {
    PathBuf::from(VMS_DIR)
}

pub fn rootfs_path(vms_dir: &Path, id: Uuid) -> PathBuf {
    vms_dir.join(id.to_string()).join(ROOTFS_FILE_NAME)
}

/// Copies the verified template rootfs into the VM's writable disk at
/// `{vms_dir}/{id}/rootfs.ext4`. An existing disk is reused as-is so a
/// stopped VM keeps its data across restarts.
pub fn prepare_rootfs(
    vms_dir: &Path,
    id: Uuid,
    template: &mut File,
) -> Result<PathBuf, RootfsError> {
    let vm_dir = vms_dir.join(id.to_string());
    let rootfs = rootfs_path(vms_dir, id);
    match fs::metadata(&rootfs) {
        Ok(_) => return Ok(rootfs),
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(RootfsError::Inspect {
                path: rootfs,
                source,
            });
        }
    }

    fs::create_dir_all(&vm_dir).map_err(|source| RootfsError::CreateDirectory {
        path: vm_dir.clone(),
        source,
    })?;

    let tmp = vm_dir.join(ROOTFS_TMP_FILE_NAME);
    if let Err(error) = publish(template, &tmp, &rootfs) {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(rootfs)
}

fn publish(template: &mut File, tmp: &Path, rootfs: &Path) -> Result<(), RootfsError> {
    let copy_error = |source| RootfsError::Copy {
        path: tmp.to_owned(),
        source,
    };

    // The registry's hash verification shares the descriptor offset, so the
    // template handle arrives at EOF.
    template
        .seek(SeekFrom::Start(0))
        .map_err(copy_error)?;
    let mut out = File::create(tmp).map_err(copy_error)?;
    io::copy(template, &mut out).map_err(copy_error)?;
    out.sync_all().map_err(copy_error)?;

    fs::rename(tmp, rootfs).map_err(|source| RootfsError::Publish {
        path: rootfs.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;

    use tempfile::tempdir;

    use super::*;

    fn template_file(directory: &Path, content: &[u8]) -> File {
        let path = directory.join("template.ext4");
        fs::write(&path, content).unwrap();
        let mut file = File::open(&path).unwrap();
        // Match open_verified's post-hash cursor position.
        file.seek(SeekFrom::End(0)).unwrap();
        file
    }

    #[test]
    fn copies_template_into_place() {
        let directory = tempdir().unwrap();
        let vms_dir = directory.path().join("vms");
        let mut template = template_file(directory.path(), b"template-bytes");
        let id = Uuid::new_v4();

        let rootfs = prepare_rootfs(&vms_dir, id, &mut template).unwrap();

        assert_eq!(rootfs, vms_dir.join(id.to_string()).join("rootfs.ext4"));
        assert_eq!(fs::read(&rootfs).unwrap(), b"template-bytes");
        assert!(!vms_dir.join(id.to_string()).join("rootfs.ext4.tmp").exists());
    }

    #[test]
    fn reuses_an_existing_rootfs_without_recopying() {
        let directory = tempdir().unwrap();
        let vms_dir = directory.path().join("vms");
        let mut template = template_file(directory.path(), b"fresh-template");
        let id = Uuid::new_v4();
        let vm_dir = vms_dir.join(id.to_string());
        fs::create_dir_all(&vm_dir).unwrap();
        fs::write(vm_dir.join("rootfs.ext4"), b"existing-disk").unwrap();

        let rootfs = prepare_rootfs(&vms_dir, id, &mut template).unwrap();

        assert_eq!(fs::read(&rootfs).unwrap(), b"existing-disk");
    }

    #[test]
    fn failed_copy_leaves_no_tmp_file() {
        let directory = tempdir().unwrap();
        let vms_dir = directory.path().join("vms");
        let template_path = directory.path().join("template.ext4");
        let mut unreadable = OpenOptions::new()
            .create(true)
            .write(true)
            .open(&template_path)
            .unwrap();
        unreadable.write_all(b"template-bytes").unwrap();
        let id = Uuid::new_v4();

        let error = prepare_rootfs(&vms_dir, id, &mut unreadable).unwrap_err();

        assert!(matches!(error, RootfsError::Copy { .. }));
        let vm_dir = vms_dir.join(id.to_string());
        assert!(!vm_dir.join("rootfs.ext4.tmp").exists());
        assert!(!vm_dir.join("rootfs.ext4").exists());
    }
}

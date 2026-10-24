//! Data disk files, validated guest mount paths, and boot-time mount injection.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use firecrab_api_types::MicroBlockStorageResponse;

use crate::rootfs::{self, RootfsError};
use crate::state::AppState;

/// Bounds both the guest manifest and the number of additional devices.
pub(crate) const MAX_VOLUMES_PER_VM: usize = 8;
const MANIFEST: &str = "/etc/firecrab/volumes";
const RUNNER: &str = "/etc/firecrab/mount-volumes";
const BEGIN: &str = "# BEGIN FIRECRAB BLOCK STORAGE";
const END: &str = "# END FIRECRAB BLOCK STORAGE";

/// Rejects paths that could replace boot tools, runtime mounts, or configuration.
pub(crate) fn valid_mount_path(path: &str) -> bool {
    if !path.starts_with('/')
        || path.len() > 1024
        || path.ends_with('/')
        || !path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte))
        || path
            .split('/')
            .skip(1)
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return false;
    }
    ![
        "/etc",
        "/dev",
        "/proc",
        "/sys",
        "/run",
        "/bin",
        "/sbin",
        "/lib",
        "/lib64",
        "/usr",
        "/boot",
        "/var/run",
        "/var/lock",
        "/var/lib/firecrab",
    ]
    .iter()
    .any(|reserved| paths_overlap(reserved, path))
}

/// A parent and child mount are also conflicting, not just identical paths.
pub(crate) fn paths_overlap(a: &str, b: &str) -> bool {
    a == b
        || a.strip_prefix(b).is_some_and(|tail| tail.starts_with('/'))
        || b.strip_prefix(a).is_some_and(|tail| tail.starts_with('/'))
}

/// Resolves a volume strictly; an unavailable pool never falls back to another disk.
pub(crate) fn disk_path(
    state: &AppState,
    volume: &MicroBlockStorageResponse,
) -> Result<PathBuf, String> {
    Ok(state
        .storage_path_for(&volume.storage_root)
        .map_err(|error| error.to_string())?
        .join("volumes")
        .join(volume.id.to_string())
        .join("data.ext4"))
}

/// Creates a new empty filesystem. Existing files are never formatted.
pub(crate) fn create_disk(
    state: &AppState,
    volume: &MicroBlockStorageResponse,
) -> Result<(), String> {
    let path = disk_path(state, volume)?;
    let directory = path.parent().expect("volume path has a parent");
    fs::create_dir_all(directory.parent().expect("volume directory has a parent"))
        .map_err(|error| error.to_string())?;
    fs::create_dir(directory).map_err(|error| error.to_string())?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    let file = OpenOptions::new()
        .write(true)
        .read(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|error| error.to_string())?;
    file.set_len(u64::from(volume.size_gib) << 30)
        .map_err(|error| error.to_string())?;
    let result = Command::new("mkfs.ext4")
        .args([
            "-q",
            "-F",
            "-U",
            &volume.id.to_string(),
            "-E",
            "lazy_itable_init=0,lazy_journal_init=0",
        ])
        .arg(&path)
        .output()
        .map_err(|error| format!("mkfs.ext4: {error}"))?;
    if !result.status.success() {
        return Err(format!(
            "mkfs.ext4: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    verify_disk(state, volume)?;
    file.sync_all().map_err(|error| error.to_string())?;
    sync_parent_directories(&path)
}

/// Persist the file entry, volume directory, and pool's volumes directory together.
fn sync_parent_directories(path: &Path) -> Result<(), String> {
    for directory in path.ancestors().skip(1).take(3) {
        File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Verifies size, ext4 signature, and filesystem UUID before exposing a data disk.
pub(crate) fn verify_disk(
    state: &AppState,
    volume: &MicroBlockStorageResponse,
) -> Result<PathBuf, String> {
    let path = disk_path(state, volume)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|error| format!("volume {}: {error}", volume.name))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() != (u64::from(volume.size_gib) << 30) {
        return Err(format!(
            "volume {} has an unexpected disk size or file type",
            volume.name
        ));
    }
    let mut superblock = [0_u8; 120];
    file.seek(SeekFrom::Start(1024))
        .and_then(|_| file.read_exact(&mut superblock))
        .map_err(|error| error.to_string())?;
    if superblock[56..58] != [0x53, 0xef] || superblock[104..120] != *volume.id.as_bytes() {
        return Err(format!(
            "volume {} has an unexpected filesystem or UUID",
            volume.name
        ));
    }
    Ok(path)
}

/// Removes only this disk and its empty directory, preserving unexpected files.
pub(crate) fn remove_disk(
    state: &AppState,
    volume: &MicroBlockStorageResponse,
) -> Result<(), String> {
    let path = disk_path(state, volume)?;
    for result in [
        fs::remove_file(&path),
        fs::remove_dir(path.parent().expect("disk parent")),
    ] {
        if let Err(error) = result
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error.to_string());
        }
    }
    Ok(())
}

/// Completes interrupted file operations before the API accepts new mutations.
pub(crate) fn recover(state: &AppState) -> Result<(), String> {
    for volume in state
        .store
        .block_storages()
        .map_err(|error| error.to_string())?
    {
        match volume.state.as_str() {
            "creating" if completed_creation(state, &volume)? => {
                state
                    .store
                    .finish_block_storage_create(volume.id)
                    .map_err(|error| error.to_string())?;
            }
            "creating" | "deleting" => {
                state
                    .store
                    .begin_block_storage_delete(volume.id)
                    .map_err(|error| error.to_string())?;
                if let Err(error) = remove_disk(state, &volume) {
                    tracing::warn!(volume_id = %volume.id, %error, "volume cleanup requires a retry");
                    continue;
                }
                state
                    .store
                    .finish_block_storage_delete(volume.id)
                    .map_err(|error| error.to_string())?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// A crash may leave the superblock written before mkfs finished its metadata.
fn completed_creation(
    state: &AppState,
    volume: &MicroBlockStorageResponse,
) -> Result<bool, String> {
    let Ok(path) = verify_disk(state, volume) else {
        return Ok(false);
    };
    let output = Command::new("e2fsck")
        .args(["-f", "-n"])
        .arg(&path)
        .output()
        .map_err(|error| format!("cannot verify interrupted volume creation: {error}"))?;
    if !output.status.success() {
        return Ok(false);
    }
    File::open(&path)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())?;
    sync_parent_directories(&path)?;
    Ok(true)
}

/// Replaces only Firecrab's fstab section; a detached disk cannot be mounted again.
fn fstab_contents(previous: &str, volumes: &[MicroBlockStorageResponse]) -> String {
    let mut managed = false;
    let mut output = String::new();
    for line in previous.lines() {
        if line == BEGIN {
            managed = true;
            continue;
        }
        if line == END {
            managed = false;
            continue;
        }
        if !managed {
            output.push_str(line);
            output.push('\n');
        }
    }
    if !volumes.is_empty() {
        output.push_str(BEGIN);
        output.push('\n');
        for volume in volumes {
            output.push_str(&format!(
                "UUID={} {} ext4 defaults 0 0\n",
                volume.id,
                volume
                    .mount_path
                    .as_deref()
                    .expect("attached volume has a path")
            ));
        }
        output.push_str(END);
        output.push('\n');
    }
    output
}

fn guest_error(rootfs: &Path, detail: impl Into<String>) -> RootfsError {
    RootfsError::Specialize {
        path: rootfs.to_owned(),
        detail: detail.into(),
    }
}

/// Creates guest directories without following a guest-controlled symlink.
fn ensure_mount_directory(rootfs: &Path, path: &str) -> Result<(), RootfsError> {
    let mut current = String::new();
    for component in path.split('/').filter(|part| !part.is_empty()) {
        current.push('/');
        current.push_str(component);
        if !rootfs::guest_path_exists(rootfs, &current) {
            rootfs::run_debugfs(rootfs, &format!("mkdir {current}"))?;
        }
        let output = rootfs::run_debugfs(rootfs, &format!("stat {current}"))?;
        if !String::from_utf8_lossy(&output.stdout).contains("Type: directory") {
            return Err(guest_error(
                rootfs,
                format!("mount path component {current} must be a directory, not a symlink"),
            ));
        }
    }
    Ok(())
}

/// Installs UUID-based mounts for systemd/OpenRC and the managed OCI boot path.
pub(crate) fn install_guest(
    rootfs: &Path,
    volumes: &[MicroBlockStorageResponse],
) -> Result<(), RootfsError> {
    if volumes.is_empty() && !rootfs::guest_path_exists(rootfs, MANIFEST) {
        return Ok(());
    }
    let oci = rootfs::guest_path_exists(rootfs, "/etc/firecrab/rc.boot");
    if !volumes.is_empty()
        && !oci
        && !rootfs::guest_path_exists(rootfs, "/etc/systemd/system")
        && !rootfs::guest_path_exists(rootfs, "/etc/runlevels/default")
    {
        return Err(guest_error(
            rootfs,
            "volumes require the Firecrab OCI runtime, systemd, or OpenRC",
        ));
    }
    let mut manifest = String::new();
    for volume in volumes {
        let path = volume
            .mount_path
            .as_deref()
            .ok_or_else(|| guest_error(rootfs, "missing volume mount path"))?;
        if !valid_mount_path(path) {
            return Err(guest_error(rootfs, "saved volume mount path is invalid"));
        }
        ensure_mount_directory(rootfs, path)?;
        manifest.push_str(&format!("{} {path}\n", volume.id));
    }
    let previous = rootfs::run_debugfs(rootfs, "cat /etc/fstab")?;
    let previous = std::str::from_utf8(&previous.stdout)
        .map_err(|_| guest_error(rootfs, "guest fstab must be UTF-8"))?;
    rootfs::write_into_image(
        rootfs,
        "/etc/fstab",
        fstab_contents(previous, if oci { &[] } else { volumes }).as_bytes(),
    )?;
    let _ = rootfs::run_debugfs(rootfs, "mkdir /etc/firecrab");
    rootfs::write_into_image(rootfs, MANIFEST, manifest.as_bytes())?;
    rootfs::write_into_image(rootfs, RUNNER, MOUNT_SCRIPT.as_bytes())?;
    rootfs::set_guest_file_mode(rootfs, RUNNER, "0100755");
    if oci {
        let init = rootfs::run_debugfs(rootfs, "cat /etc/firecrab/init-system")?;
        let fallback = init.stdout.is_empty() || init.stdout == b"busybox\n";
        rootfs::write_into_image(
            rootfs,
            "/etc/firecrab/rc.boot",
            crate::oci::provision::boot_script_for(fallback).as_bytes(),
        )?;
    }
    Ok(())
}

const MOUNT_SCRIPT: &str = include_str!("guest/mount-volumes.sh");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_storage_mount_paths_reject_boot_paths_traversal_and_shell_syntax() {
        for path in [
            "/",
            "/etc/data",
            "/var",
            "/var/run/app",
            "/usr/local/data",
            "/proc",
            "/data/../etc",
            "/data//nested",
            "/data/",
            "/data;id",
            "/data name",
            "/data\nname",
            "/var/lib/firecrab/state",
            "data",
        ] {
            assert!(!valid_mount_path(path), "{path:?}");
        }
        for path in [
            "/data",
            "/srv/uploads",
            "/var/lib/postgresql/data",
            "/app-data",
        ] {
            assert!(valid_mount_path(path), "{path:?}");
        }
        assert!(paths_overlap("/data", "/data/nested"));
        assert!(!paths_overlap("/data", "/database"));
    }

    #[tokio::test]
    async fn block_storage_guest_mounts_replace_detach_and_reject_guest_symlinks() {
        let temp = tempfile::Builder::new()
            .prefix("v247-")
            .tempdir_in("/tmp")
            .unwrap();
        let state = crate::handlers::vms::test_support::test_state(temp.path()).await;
        let rootfs = temp.path().join("rootfs");
        rootfs::run_debugfs(&rootfs, "mkdir /etc/systemd").unwrap();
        rootfs::run_debugfs(&rootfs, "mkdir /etc/systemd/system").unwrap();
        rootfs::write_into_image(&rootfs, "/etc/fstab", b"# user mount retained\n").unwrap();
        let volume = MicroBlockStorageResponse {
            id: uuid::Uuid::new_v4(),
            name: "data".into(),
            size_gib: 1,
            storage_root: "default".into(),
            state: "attached".into(),
            attached_vm_id: Some(uuid::Uuid::new_v4()),
            mount_path: Some("/data".into()),
        };
        install_guest(&rootfs, std::slice::from_ref(&volume)).unwrap();
        install_guest(&rootfs, std::slice::from_ref(&volume)).unwrap();
        let fstab = rootfs::run_debugfs(&rootfs, "cat /etc/fstab").unwrap();
        let text = String::from_utf8(fstab.stdout).unwrap();
        assert!(text.starts_with("# user mount retained\n"));
        assert_eq!(text.matches(&volume.id.to_string()).count(), 1);
        assert!(text.contains("/data ext4 defaults 0 0"));
        install_guest(&rootfs, &[]).unwrap();
        let cleaned = rootfs::run_debugfs(&rootfs, "cat /etc/fstab").unwrap();
        assert_eq!(cleaned.stdout, b"# user mount retained\n");
        rootfs::run_debugfs(&rootfs, "symlink /unsafe /etc").unwrap();
        let invalid = MicroBlockStorageResponse {
            mount_path: Some("/unsafe/files".into()),
            ..volume
        };
        assert!(install_guest(&rootfs, &[invalid]).is_err());
        assert!(state.store.block_storages().unwrap().is_empty());
    }
}

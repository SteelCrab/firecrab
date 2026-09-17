//! Write operator authorized_keys and host keys into the guest rootfs.

use std::fs;
use std::path::Path;

use crate::artifacts::VmArtifactPaths;

use super::keys::ensure_vm_ssh_keys;
use super::service::{GUEST_SSHD_DROPIN, GUEST_SSHD_SERVICE, sshd_dropin, sshd_service_script};

/// Writes authorized_keys, host keys, the sshd drop-in, and the `services.d`
/// launcher into the guest disk. Call after [`crate::rootfs::specialize_guest`]
/// so template host keys stay stripped and this VM's copies replace them.
///
/// The launcher is written on every start, not only at import: a disk
/// imported before Firecrab grew an sshd service would otherwise never get
/// one, and nothing in the guest would ever listen on port 22.
pub fn install_on_guest(
    rootfs: &Path,
    paths: &VmArtifactPaths,
) -> Result<(), crate::rootfs::RootfsError> {
    let ssh =
        ensure_vm_ssh_keys(paths).map_err(|error| crate::rootfs::RootfsError::Specialize {
            path: rootfs.to_owned(),
            detail: error.to_string(),
        })?;
    let authorized = fs::read(&ssh.operator_public).map_err(|source| {
        crate::rootfs::RootfsError::Specialize {
            path: rootfs.to_owned(),
            detail: format!("read operator public key: {source}"),
        }
    })?;
    let host_priv =
        fs::read(&ssh.host_private).map_err(|source| crate::rootfs::RootfsError::Specialize {
            path: rootfs.to_owned(),
            detail: format!("read host private key: {source}"),
        })?;
    let host_pub =
        fs::read(&ssh.host_public).map_err(|source| crate::rootfs::RootfsError::Specialize {
            path: rootfs.to_owned(),
            detail: format!("read host public key: {source}"),
        })?;

    let _ = crate::rootfs::run_debugfs(rootfs, "mkdir /root");
    let _ = crate::rootfs::run_debugfs(rootfs, "mkdir /root/.ssh");
    let _ = crate::rootfs::run_debugfs(rootfs, "mkdir /etc/ssh");
    let _ = crate::rootfs::run_debugfs(rootfs, "mkdir /etc/ssh/sshd_config.d");
    let _ = crate::rootfs::run_debugfs(rootfs, "mkdir /etc/firecrab");
    let _ = crate::rootfs::run_debugfs(rootfs, "mkdir /etc/firecrab/services.d");

    // debugfs writes the type bits along with the permission bits, so a
    // directory needs `04…`; `0100700` would leave sshd a regular file where
    // it looks for a directory. Done before the write so a disk already
    // flipped that way by an earlier start becomes a directory again.
    crate::rootfs::set_guest_file_mode(rootfs, "/root/.ssh", "040700");
    // An OCI tree extracted unprivileged keeps the host user's uid, and
    // sshd's StrictModes refuses a key under a home directory root does
    // not own. The guest service repeats this; the disk should not depend
    // on a working `chown` inside a stripped-down image.
    let _ = crate::rootfs::run_debugfs(rootfs, "set_inode_field /root uid 0");
    let _ = crate::rootfs::run_debugfs(rootfs, "set_inode_field /root gid 0");
    crate::rootfs::write_into_image(rootfs, "/root/.ssh/authorized_keys", &authorized)?;
    crate::rootfs::set_guest_file_mode(rootfs, "/root/.ssh/authorized_keys", "0100600");

    crate::rootfs::write_into_image(rootfs, "/etc/ssh/ssh_host_ed25519_key", &host_priv)?;
    crate::rootfs::write_into_image(rootfs, "/etc/ssh/ssh_host_ed25519_key.pub", &host_pub)?;
    crate::rootfs::set_guest_file_mode(rootfs, "/etc/ssh/ssh_host_ed25519_key", "0100600");

    crate::rootfs::write_into_image(rootfs, GUEST_SSHD_DROPIN, sshd_dropin())?;

    crate::rootfs::write_into_image(rootfs, GUEST_SSHD_SERVICE, sshd_service_script().as_bytes())?;
    crate::rootfs::set_guest_file_mode(rootfs, GUEST_SSHD_SERVICE, "0100755");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;
    use uuid::Uuid;

    /// A small real ext4 image; `/root` and `/etc` exist the way a guest
    /// disk has them before install runs.
    fn guest_image(path: &Path) {
        let status = Command::new("mkfs.ext4")
            .args(["-q", "-F"])
            .arg(path)
            .arg("8M")
            .status()
            .expect("mkfs.ext4 must be installed for this test");
        assert!(status.success(), "mkfs.ext4 failed");
        for dir in ["/root", "/etc", "/etc/ssh", "/etc/firecrab"] {
            crate::rootfs::run_debugfs(path, &format!("mkdir {dir}")).unwrap();
        }
    }

    fn artifacts(dir: &Path) -> VmArtifactPaths {
        let paths = VmArtifactPaths::for_vm(dir, Uuid::from_u128(7));
        paths.ensure_directories().unwrap();
        paths
    }

    fn stat(rootfs: &Path, guest_path: &str) -> String {
        let output = crate::rootfs::run_debugfs(rootfs, &format!("stat {guest_path}")).unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn cat(rootfs: &Path, guest_path: &str) -> String {
        let output = crate::rootfs::run_debugfs(rootfs, &format!("cat {guest_path}")).unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// `set_inode_field mode` writes the type bits too, so a regular-file
    /// mode on `/root/.ssh` leaves a directory sshd can no longer read.
    #[test]
    fn install_on_guest_leaves_root_dot_ssh_a_directory() {
        let directory = tempdir().unwrap();
        let rootfs = directory.path().join("rootfs.ext4");
        guest_image(&rootfs);
        let paths = artifacts(directory.path());

        install_on_guest(&rootfs, &paths).unwrap();

        let stat = stat(&rootfs, "/root/.ssh");
        assert!(stat.contains("Type: directory"), "{stat}");
        let listing = crate::rootfs::run_debugfs(&rootfs, "ls /root/.ssh").unwrap();
        assert!(
            String::from_utf8_lossy(&listing.stdout).contains("authorized_keys"),
            "authorized_keys must stay reachable under the directory"
        );
    }

    /// A disk from an earlier start arrives with `/root/.ssh` already
    /// flipped to a regular file; install has to put the type back.
    #[test]
    fn install_on_guest_repairs_a_root_dot_ssh_flipped_to_a_regular_file() {
        let directory = tempdir().unwrap();
        let rootfs = directory.path().join("rootfs.ext4");
        guest_image(&rootfs);
        let paths = artifacts(directory.path());
        crate::rootfs::run_debugfs(&rootfs, "mkdir /root/.ssh").unwrap();
        crate::rootfs::set_guest_file_mode(&rootfs, "/root/.ssh", "0100700");
        assert!(stat(&rootfs, "/root/.ssh").contains("Type: regular"));

        install_on_guest(&rootfs, &paths).unwrap();

        assert!(stat(&rootfs, "/root/.ssh").contains("Type: directory"));
    }

    /// An OCI tree extracted by an unprivileged import carries the host
    /// user's uid, and sshd's StrictModes refuses a key under a home
    /// directory somebody other than root owns.
    #[test]
    fn install_on_guest_gives_root_back_its_home_directory() {
        let directory = tempdir().unwrap();
        let rootfs = directory.path().join("rootfs.ext4");
        guest_image(&rootfs);
        let paths = artifacts(directory.path());
        crate::rootfs::run_debugfs(&rootfs, "set_inode_field /root uid 1000").unwrap();
        crate::rootfs::run_debugfs(&rootfs, "set_inode_field /root gid 1000").unwrap();

        install_on_guest(&rootfs, &paths).unwrap();

        let stat = stat(&rootfs, "/root");
        assert!(stat.contains("User:     0"), "{stat}");
        assert!(stat.contains("Group:     0"), "{stat}");
    }

    /// Nothing starts sshd unless the launcher is on the disk, and it is
    /// written at import only — a disk imported before that never gets one.
    #[test]
    fn install_on_guest_writes_the_sshd_service_script() {
        let directory = tempdir().unwrap();
        let rootfs = directory.path().join("rootfs.ext4");
        guest_image(&rootfs);
        let paths = artifacts(directory.path());

        install_on_guest(&rootfs, &paths).unwrap();

        let script = cat(&rootfs, super::super::GUEST_SSHD_SERVICE);
        assert!(script.contains("exec /usr/sbin/sshd -D"), "{script}");
        let stat = stat(&rootfs, super::super::GUEST_SSHD_SERVICE);
        assert!(stat.contains("Mode:  0755"), "{stat}");
    }
}

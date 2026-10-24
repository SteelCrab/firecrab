//! Installs the small static resize helper beside the existing metrics agent.
use std::path::Path;

use crate::rootfs::{RootfsError, set_guest_file_mode, write_into_image};

pub(crate) const GUEST_PATH: &str = "/usr/local/sbin/firecrab-console-resize";

pub(crate) fn install(rootfs: &Path) -> Result<(), RootfsError> {
    let path = std::env::var_os("FIRECRAB_GUEST_CONSOLE_BIN")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.join("firecrab-guest-console")))
        });
    let Some(path) = path else {
        return Ok(());
    };
    install_binary(rootfs, &path)
}

fn install_binary(rootfs: &Path, path: &Path) -> Result<(), RootfsError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(RootfsError::Specialize {
                path: rootfs.to_owned(),
                detail: format!("read guest console helper: {error}"),
            });
        }
    };
    if !bytes.starts_with(b"\x7fELF") || bytes.len() > 1024 * 1024 {
        return Err(RootfsError::Specialize {
            path: rootfs.to_owned(),
            detail: "guest console helper must be a small static Linux executable".into(),
        });
    }
    write_into_image(rootfs, GUEST_PATH, &bytes)?;
    set_guest_file_mode(rootfs, GUEST_PATH, "0100755");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installs_and_refreshes_the_helper_inside_the_guest_with_executable_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("guest.ext4");
        assert!(
            std::process::Command::new("mkfs.ext4")
                .args(["-q", "-F"])
                .arg(&image)
                .arg("8M")
                .status()
                .unwrap()
                .success()
        );
        for path in ["/usr", "/usr/local", "/usr/local/sbin"] {
            crate::rootfs::run_debugfs(&image, &format!("mkdir {path}")).unwrap();
        }
        let payload = dir.path().join("helper");
        for bytes in [b"\x7fELFfirst".as_slice(), b"\x7fELFsecond".as_slice()] {
            std::fs::write(&payload, bytes).unwrap();
            install_binary(&image, &payload).unwrap();
            let output = crate::rootfs::run_debugfs(&image, &format!("cat {GUEST_PATH}")).unwrap();
            assert_eq!(output.stdout, bytes);
            let output = crate::rootfs::run_debugfs(&image, &format!("stat {GUEST_PATH}")).unwrap();
            assert!(String::from_utf8_lossy(&output.stdout).contains("0755"));
        }
        std::fs::write(&payload, b"not an ELF").unwrap();
        assert!(install_binary(&image, &payload).is_err());
    }
}

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;

use firecrab_api_types::{ManagerHostCapacity, ManagerResources};
use fs2::FileExt;

use super::super::resource::{self, Error};
use super::{daemon, lifecycle::Layout};

fn helper() -> std::path::PathBuf {
    super::helper_path(
        std::env::var_os(super::HELPER_ENV),
        std::env::current_exe().ok().as_deref(),
    )
}

pub fn host_capacity(home: &Path) -> Result<ManagerHostCapacity, Error> {
    let output = Command::new(helper())
        .arg("resource-host")
        .env("FIRECRAB_MICROMANAGER_HOME", home)
        .output()?;
    if !output.status.success() {
        return Err(Error::Invalid(
            String::from_utf8_lossy(&output.stderr).trim().into(),
        ));
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

pub fn validate_configuration(home: &Path, settings: &ManagerResources) -> Result<(), Error> {
    let mut input = tempfile::NamedTempFile::new()?;
    input.write_all(&serde_json::to_vec(settings)?)?;
    let output = Command::new(helper())
        .arg("validate-resources")
        .arg(input.path())
        .env("FIRECRAB_MICROMANAGER_HOME", home)
        .output()?;
    if !output.status.success() {
        return Err(Error::Invalid(
            String::from_utf8_lossy(&output.stderr).trim().into(),
        ));
    }
    Ok(())
}

pub fn apply(home: &Path, settings: &ManagerResources) -> Result<(), Error> {
    let bytes = settings
        .disk_gib
        .checked_mul(1 << 30)
        .ok_or_else(|| Error::Invalid("data disk size overflows the byte limit".into()))?;
    validate_configuration(home, settings)?;
    let mut layout =
        Layout::from_process_env().map_err(|error| Error::Invalid(error.to_string()))?;
    layout.managed_home = home.to_owned();
    let ssh_settings = super::ssh::load(home).map_err(|error| Error::Invalid(error.to_string()))?;
    let disk = home.join("data/firecrab-data.raw");
    let original = fs::symlink_metadata(&disk)?;
    if !original.file_type().is_file() || original.nlink() != 1 {
        return Err(Error::Invalid(
            "the data disk must be an unaliased regular file".into(),
        ));
    }
    if bytes < fs::metadata(&disk)?.len() {
        return Err(Error::Invalid(
            "shrinking the data disk is not supported".into(),
        ));
    }
    daemon::stop(&layout).map_err(|error| Error::Invalid(error.to_string()))?;
    let prepared = (|| {
        // This is the same flock the Swift VM and OS repair use. Hold it while
        // changing the disk, then release it before booting the VM.
        let vm_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(home.join("runtime/vm.lock"))?;
        vm_lock.try_lock_exclusive().map_err(|_| Error::Busy)?;
        let file = OpenOptions::new().write(true).open(&disk)?;
        let opened = file.metadata()?;
        if opened.dev() != original.dev() || opened.ino() != original.ino() || opened.nlink() != 1 {
            return Err(Error::Invalid(
                "the data disk changed before growth; retry after checking its path".into(),
            ));
        }
        file.set_len(bytes)?;
        file.sync_all()?;
        drop(vm_lock);
        daemon::install(&layout, &helper(), &home.join("runtime/manager_ed25519"))
            .map_err(|error| Error::Invalid(error.to_string()))?;
        Ok(())
    })();
    if let Err(error) = prepared {
        let _ = daemon::start(&layout);
        return Err(error);
    }
    let status = daemon::start(&layout).map_err(|error| Error::Invalid(error.to_string()))?;
    if !status.success() {
        return Err(Error::Invalid(
            "management VM did not become healthy; inspect service debug --logs".into(),
        ));
    }
    let marker = fs::read_to_string(home.join("runtime/manager-ready"))?;
    let ip = super::manager_ip(&marker)
        .ok_or_else(|| Error::Invalid("management VM address is missing".into()))?;
    let output = Command::new("/usr/bin/ssh")
        .arg("-i")
        .arg(home.join("runtime/manager_ed25519"))
        .args(super::ssh::arguments(
            &home.join("runtime"),
            &ssh_settings,
            super::ssh::Session::Command,
        ))
        .args(["-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes"])
        .arg("-o")
        .arg(super::known_hosts_option(&home.join("runtime/known_hosts")))
        .arg(format!("root@{ip}"))
        .arg("resize2fs /dev/vdb")
        .output()?;
    if !output.status.success() {
        return Err(Error::Invalid(format!(
            "disk image grew, but filesystem growth failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    resource::write(&home.join("runtime/resources-active.json"), settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflowing_disk_size_is_rejected_before_native_operations() {
        let dir = tempfile::tempdir().unwrap();
        let settings = ManagerResources {
            cpu: 2,
            memory_mib: 4096,
            disk_gib: (u64::MAX >> 30) + 1,
            network_mode: firecrab_api_types::ManagerNetworkMode::Nat,
            network_interface: None,
        };
        assert!(
            matches!(apply(dir.path(), &settings), Err(Error::Invalid(message)) if message.contains("overflows"))
        );
    }
}

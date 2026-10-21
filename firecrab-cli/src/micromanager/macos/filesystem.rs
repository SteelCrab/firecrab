//! Check a private OS copy in an initramfs VM and install only verified repairs.
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::super::{managed_home, os_repair::CheckResult, report, snapshot};
use super::{lifecycle::Layout, provision};
use fs2::FileExt;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("OS filesystem repair: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    ManagedHome(#[from] managed_home::Error),
    #[error(
        "OS repair needs a signed helper with `repair-os`; build or update {0} before retrying"
    )]
    Helper(PathBuf),
    #[error(
        "OS filesystem repair failed: {detail}; no unverified disk was installed; backup: {directory}; logs: {logs}"
    )]
    Recovery {
        directory: PathBuf,
        logs: PathBuf,
        detail: String,
    },
}

pub fn require_helper(helper: &Path) -> Result<(), Error> {
    let output = Command::new(helper)
        .args(["repair-os", "--help"])
        .output()?;
    if !output.status.success() || !String::from_utf8_lossy(&output.stdout).contains("--reference")
    {
        return Err(Error::Helper(helper.to_owned()));
    }
    Ok(())
}

fn unused(path: &Path) -> io::Result<()> {
    let output = Command::new("/usr/sbin/lsof")
        .arg("-t")
        .arg("--")
        .arg(path)
        .output()?;
    if output.status.code() == Some(1) && output.stdout.is_empty() && output.stderr.is_empty() {
        return Ok(());
    }
    if output.status.success() && !output.stdout.is_empty() {
        return Err(io::Error::other(format!(
            "disk still in use: {}; stop its VM before repair",
            path.display()
        )));
    }
    Err(io::Error::other(format!(
        "could not establish that {} is unused",
        path.display()
    )))
}

fn check_copy(
    helper: &Path,
    layout: &Layout,
    working: &Path,
    logs: &Path,
    reference: Option<&Path>,
    timeout: Duration,
) -> io::Result<CheckResult> {
    let stdout = File::create(logs.join("result.json"))?;
    let stderr = File::create(logs.join("console.log"))?;
    let mut command = Command::new(helper);
    command.arg("repair-os").arg(working);
    if let Some(reference) = reference {
        command.arg("--reference").arg(reference);
    }
    let mut child = command
        .env("FIRECRAB_MICROMANAGER_HOME", &layout.managed_home)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()?;
    let started = Instant::now();
    let mut next_report = Duration::ZERO;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            child.kill()?;
            child.wait()?;
            return Err(io::Error::other(format!(
                "offline filesystem check timed out after {} seconds",
                timeout.as_secs()
            )));
        }
        if elapsed >= next_report {
            report!(
                "[WAIT] OS filesystem: {}s/{}s; console: {}",
                elapsed.as_secs(),
                timeout.as_secs(),
                logs.join("console.log").display()
            );
            next_report = elapsed + Duration::from_secs(10);
        }
        thread::sleep(Duration::from_millis(100));
    };
    let result = serde_json::from_slice::<CheckResult>(&fs::read(logs.join("result.json"))?);
    if !status.success() || result.as_ref().is_ok_and(|result| !result.verified()) {
        let detail = match &result {
            Ok(result) => format!(
                "check_exit={}, repair_exit={:?}, verify_exit={}",
                result.check_exit, result.repair_exit, result.verify_exit
            ),
            Err(error) => format!("invalid repair result: {error}"),
        };
        return Err(io::Error::other(format!(
            "offline repair helper did not verify the filesystem ({status}); {detail}"
        )));
    }
    result.map_err(io::Error::other)
}

pub fn repair(layout: &Layout, helper: &Path) -> Result<snapshot::Repair, Error> {
    repair_with_timeout(layout, helper, Duration::from_secs(120))
}

fn repair_with_timeout(
    layout: &Layout,
    helper: &Path,
    timeout: Duration,
) -> Result<snapshot::Repair, Error> {
    let os_disk = layout.managed_home.join("system/debian-system.raw");
    let data_disk = layout.managed_home.join("data/firecrab-data.raw");
    let lock_path = layout.managed_home.join("runtime/vm.lock");
    for path in [&os_disk, &data_disk, &lock_path] {
        managed_home::reject_symlink_components(path)?;
    }
    let original = fs::metadata(&os_disk)?;
    let data = fs::metadata(&data_disk)?;
    if original.dev() == data.dev() && original.ino() == data.ino() {
        return Err(io::Error::other("OS and data disks must be different files").into());
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path)?;
    FileExt::try_lock_exclusive(&lock).map_err(|_| {
        io::Error::other("management VM is still running or another repair owns its lock")
    })?;
    unused(&os_disk)?;
    unused(&data_disk)?;
    let repairs = layout.managed_home.join("runtime/repairs");
    let retention = snapshot::Repair::begin(&repairs, "os-")?;
    let directory = retention.directory().to_path_buf();
    let logs = retention.logs().to_path_buf();
    let backup = directory.join("debian-system.before.raw");
    let working = directory.join("debian-system.repaired.raw");
    let attempt = || -> io::Result<Option<u64>> {
        report!("[BACKUP] OS disk: {}", backup.display());
        let backup_snapshot = snapshot::create(&os_disk, &backup)?;
        let working_snapshot = snapshot::create(&backup, &working)?;
        retention.record(&backup_snapshot, &working_snapshot)?;
        let staged = fs::metadata(&working)?;
        let reference = directory.join("debian-system.reference.raw");
        report!("[CHECK] cached Debian image for missing OS files");
        let cached = provision::cached_os_reference(&layout.managed_home, &reference)
            .map_err(io::Error::other)?;
        if !cached {
            report!("[WARN] Debian image cache unavailable; checking filesystem only");
        }
        let result = check_copy(
            helper,
            layout,
            &working,
            &logs,
            cached.then_some(reference.as_path()),
            timeout,
        )?;
        unused(&working)?;
        if !result.changed {
            return Ok(None);
        }
        // Keep the original until the detached copy has passed verification.
        unused(&os_disk)?;
        let current = fs::metadata(&os_disk)?;
        if current.dev() != original.dev()
            || current.ino() != original.ino()
            || current.len() != original.len()
            || current.mtime() != original.mtime()
            || current.mtime_nsec() != original.mtime_nsec()
        {
            return Err(io::Error::other(
                "OS disk changed during recovery; refusing to replace it",
            ));
        }
        let verified = fs::symlink_metadata(&working)?;
        if !verified.is_file()
            || verified.nlink() != 1
            || verified.dev() != staged.dev()
            || verified.ino() != staged.ino()
        {
            return Err(io::Error::other(
                "recovery copy was replaced or aliased; refusing to install it",
            ));
        }
        let repaired = OpenOptions::new().read(true).write(true).open(&working)?;
        let opened = repaired.metadata()?;
        if opened.dev() != staged.dev() || opened.ino() != staged.ino() || opened.nlink() != 1 {
            return Err(io::Error::other("recovery copy changed before commit"));
        }
        repaired.sync_all()?;
        fs::rename(&working, &os_disk)?;
        if let Err(error) = File::open(os_disk.parent().expect("OS disk has a parent"))
            .and_then(|directory| directory.sync_all())
        {
            return Err(io::Error::other(format!(
                "verified OS disk installed, but directory sync failed: {error}"
            )));
        }
        Ok(Some(result.restored_files))
    };
    match attempt() {
        Ok(Some(restored)) => {
            fs::remove_file(directory.join("debian-system.reference.raw")).or_else(|error| {
                if error.kind() == io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(error)
                }
            })?;
            report!(
                "[PASS] OS filesystem verified; restored OS files={}; backup: {}",
                restored,
                backup.display()
            );
        }
        Ok(None) => {
            report!(
                "[PASS] OS filesystem and available packaged files clean; original disk preserved"
            );
            return Ok(retention.discard()?);
        }
        Err(error) => {
            let _ = fs::write(logs.join("error.log"), error.to_string());
            return Err(Error::Recovery {
                directory,
                logs,
                detail: error.to_string(),
            });
        }
    }
    Ok(retention)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fixture(script: &str) -> (tempfile::TempDir, Layout, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let layout = Layout {
            install_dir: root.path().join("bin"),
            managed_home: root.path().join("home"),
        };
        for file in [
            "system/debian-system.raw",
            "data/firecrab-data.raw",
            "runtime/manager_ed25519",
        ] {
            let path = layout.managed_home.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"original").unwrap();
        }
        let helper = root.path().join("helper");
        fs::write(&helper, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        (root, layout, helper)
    }

    #[test]
    fn clean_check_keeps_original_disk_identity() {
        let (_root, layout, helper) = fixture(
            "echo '{\"check_exit\":0,\"repair_exit\":null,\"verify_exit\":0,\"changed\":false}'",
        );
        let path = layout.managed_home.join("system/debian-system.raw");
        let inode = fs::metadata(&path).unwrap().ino();
        repair(&layout, &helper).unwrap();
        assert_eq!(fs::metadata(path).unwrap().ino(), inode);
        assert_eq!(
            fs::read_dir(layout.managed_home.join("runtime/repairs"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn only_a_verified_repair_replaces_os_and_keeps_backup_and_data() {
        let (_root, layout, helper) = fixture(
            "printf repaired >\"$2\"\necho '{\"check_exit\":4,\"repair_exit\":1,\"verify_exit\":0,\"changed\":true}'",
        );
        repair(&layout, &helper).unwrap();
        assert_eq!(
            fs::read(layout.managed_home.join("system/debian-system.raw")).unwrap(),
            b"repaired"
        );
        assert_eq!(
            fs::read(layout.managed_home.join("data/firecrab-data.raw")).unwrap(),
            b"original"
        );
        assert_eq!(
            fs::read(layout.managed_home.join("runtime/manager_ed25519")).unwrap(),
            b"original"
        );
        let directory = fs::read_dir(layout.managed_home.join("runtime/repairs"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            fs::read(directory.join("debian-system.before.raw")).unwrap(),
            b"original"
        );
    }

    #[test]
    fn verified_missing_file_restoration_commits_even_when_the_filesystem_was_clean() {
        let (_root, layout, helper) = fixture(
            r#"printf restored >"$2"
echo '{"check_exit":0,"repair_exit":null,"verify_exit":0,"changed":true,"restored_files":3}'"#,
        );
        repair(&layout, &helper).unwrap();
        assert_eq!(
            fs::read(layout.managed_home.join("system/debian-system.raw")).unwrap(),
            b"restored"
        );
        assert_eq!(
            fs::read(layout.managed_home.join("data/firecrab-data.raw")).unwrap(),
            b"original"
        );
    }

    #[test]
    fn aborted_read_only_check_requires_successful_repair_verification_and_helper_exit() {
        for (repair_exit, verify_exit, helper_exit, verified) in [
            (Some(1), 0, 0, true),
            (None, 0, 0, false),
            (Some(4), 0, 0, false),
            (Some(1), 4, 0, false),
            (Some(1), 0, 1, false),
        ] {
            let result = serde_json::json!({
                "check_exit": 12,
                "repair_exit": repair_exit,
                "verify_exit": verify_exit,
                "changed": repair_exit.is_some(),
            });
            let script = format!("printf repaired >\"$2\"\necho '{result}'\nexit {helper_exit}");
            let (_root, layout, helper) = fixture(&script);
            let outcome = repair(&layout, &helper);
            assert_eq!(outcome.is_ok(), verified, "{result}");
            if let Err(error) = outcome {
                let message = error.to_string();
                assert!(message.contains("check_exit=12"), "{message}");
                assert!(
                    message.contains(&format!("verify_exit={verify_exit}")),
                    "{message}"
                );
            }
            assert_eq!(
                fs::read(layout.managed_home.join("system/debian-system.raw")).unwrap(),
                if verified { b"repaired" } else { b"original" }
            );
            for file in ["data/firecrab-data.raw", "runtime/manager_ed25519"] {
                assert_eq!(
                    fs::read(layout.managed_home.join(file)).unwrap(),
                    b"original"
                );
            }
        }
    }

    #[test]
    fn unrelated_check_failures_are_rejected_even_with_clean_verification() {
        for check_exit in [-1, 8, 9, 10, 11, 13, 16, 32, 128, 255] {
            assert!(
                !CheckResult {
                    check_exit,
                    repair_exit: Some(1),
                    verify_exit: 0,
                    changed: true,
                    restored_files: 0,
                }
                .verified()
            );
        }
    }

    #[test]
    fn failed_verification_nonzero_helper_and_timeout_leave_original_unchanged() {
        for script in [
            "printf broken >\"$2\"\necho '{\"check_exit\":4,\"repair_exit\":1,\"verify_exit\":4,\"changed\":true}'",
            "printf broken >\"$2\"\nexit 1",
            "exec sleep 60",
        ] {
            let (_root, layout, helper) = fixture(script);
            assert!(repair_with_timeout(&layout, &helper, Duration::from_millis(200)).is_err());
            assert_eq!(
                fs::read(layout.managed_home.join("system/debian-system.raw")).unwrap(),
                b"original"
            );
            let directory = fs::read_dir(layout.managed_home.join("runtime/repairs"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            assert_eq!(
                fs::read(directory.join("debian-system.before.raw")).unwrap(),
                b"original"
            );
        }
    }

    #[test]
    fn replaced_symlinked_or_hardlinked_recovery_copies_are_not_committed() {
        for replace in [
            "ln -s \"$FIRECRAB_MICROMANAGER_HOME/data/firecrab-data.raw\" \"$2\"",
            "ln \"$FIRECRAB_MICROMANAGER_HOME/runtime/manager_ed25519\" \"$2\"",
            "cp \"$FIRECRAB_MICROMANAGER_HOME/runtime/manager_ed25519\" \"$2\"",
        ] {
            let script = format!(
                "mv \"$2\" \"$2.checked\"\n{replace}\necho '{{\"check_exit\":4,\"repair_exit\":1,\"verify_exit\":0,\"changed\":true}}'"
            );
            let (_root, layout, helper) = fixture(&script);
            assert!(repair(&layout, &helper).is_err());
            for name in [
                "system/debian-system.raw",
                "data/firecrab-data.raw",
                "runtime/manager_ed25519",
            ] {
                assert_eq!(
                    fs::read(layout.managed_home.join(name)).unwrap(),
                    b"original"
                );
            }
            let directory = fs::read_dir(layout.managed_home.join("runtime/repairs"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            assert_eq!(
                fs::read(directory.join("debian-system.before.raw")).unwrap(),
                b"original"
            );
        }
    }

    #[test]
    fn an_invalid_or_symlinked_cache_leaves_the_original_untouched() {
        let (_root, layout, helper) = fixture("exit 99");
        let downloads = layout.managed_home.join("downloads");
        fs::create_dir(&downloads).unwrap();
        let archive = downloads.join(provision::DEBIAN_ARCHIVE);
        fs::write(&archive, b"invalid cache").unwrap();
        assert!(repair(&layout, &helper).is_err());
        fs::remove_file(&archive).unwrap();
        std::os::unix::fs::symlink(layout.managed_home.join("data/firecrab-data.raw"), &archive)
            .unwrap();
        assert!(repair(&layout, &helper).is_err());
        assert_eq!(
            fs::read(layout.managed_home.join("system/debian-system.raw")).unwrap(),
            b"original"
        );
        assert_eq!(
            fs::read(layout.managed_home.join("data/firecrab-data.raw")).unwrap(),
            b"original"
        );
    }

    #[test]
    fn a_running_vm_lock_and_aliased_disks_block_recovery() {
        let (_root, layout, helper) = fixture("exit 0");
        let lock = File::create(layout.managed_home.join("runtime/vm.lock")).unwrap();
        FileExt::lock_exclusive(&lock).unwrap();
        assert!(repair(&layout, &helper).is_err());
        assert!(!layout.managed_home.join("runtime/repairs").exists());
        FileExt::unlock(&lock).unwrap();
        let data = layout.managed_home.join("data/firecrab-data.raw");
        fs::remove_file(&data).unwrap();
        fs::hard_link(layout.managed_home.join("system/debian-system.raw"), data).unwrap();
        assert!(repair(&layout, &helper).is_err());
    }
}

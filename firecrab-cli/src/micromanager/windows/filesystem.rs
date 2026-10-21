//! Repair only a detached VHD copy; keep the installed disk until verification.
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use fs2::FileExt;

use super::super::{managed_home, os_repair::CheckResult, report, snapshot};
use super::{lifecycle::Layout, wsl};

const BLOCK_DEVICES: &str = "lsblk -dnpo NAME";
const FSCK: &str = include_str!("fsck.sh");

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("WSL filesystem repair prerequisites: {0}")]
    Prerequisite(String),
    #[error("WSL filesystem repair: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    ManagedHome(#[from] managed_home::Error),
    #[error(
        "WSL filesystem repair failed: {detail}; no unverified disk was installed; backup: {directory}; logs: {logs}"
    )]
    Recovery {
        directory: PathBuf,
        logs: PathBuf,
        detail: String,
    },
}

fn powershell(script: &str) -> Result<String, wsl::Error> {
    wsl::run_program(
        "powershell.exe",
        &[
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ],
    )
}

fn rescue_shell(rescue: &str, script: &str) -> Result<String, wsl::Error> {
    wsl::run(&["-d", rescue, "-u", "root", "--exec", "sh", "-c", script])
}

/// Check elevation and rescue tools before stopping a running guest.
pub fn preflight() -> Result<String, Error> {
    let output = powershell("$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); $principal=[Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent()); if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run firecrab service repair from PowerShell as Administrator' }; $names=@(Get-ChildItem 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Lxss' | Where-Object { $_.GetValue('Version') -eq 2 -and $_.GetValue('DistributionName') -ne 'firecrab-debian' -and $_.GetValue('DistributionName') -notlike 'docker-desktop*' } | ForEach-Object { $_.GetValue('DistributionName') }); ConvertTo-Json -InputObject $names -Compress")
        .map_err(|error| Error::Prerequisite(error.to_string()))?;
    let names: Vec<String> = serde_json::from_str(&output).map_err(|error| {
        Error::Prerequisite(format!("cannot list WSL2 rescue distributions: {error}"))
    })?;
    let rescue = names.into_iter().find(|name| !name.is_empty() && !name.eq_ignore_ascii_case(wsl::DISTRO_NAME)
        && !name.to_ascii_lowercase().starts_with("docker-desktop"))
        .ok_or_else(|| Error::Prerequisite("a separate WSL2 Linux distribution with e2fsprogs is required; install one with `wsl --install -d Debian`, then retry".into()))?;
    rescue_shell(&rescue, "set -eu; for tool in e2fsck lsblk blkid grep sync timeout; do command -v \"$tool\" >/dev/null; done")
        .map_err(|error| Error::Prerequisite(format!("{error}; install e2fsprogs and util-linux in the rescue distribution {rescue}")))?;
    Ok(rescue)
}

fn block_devices(text: &str) -> io::Result<BTreeSet<String>> {
    let mut devices = BTreeSet::new();
    for name in text.lines().map(str::trim).filter(|name| !name.is_empty()) {
        let valid = name.strip_prefix("/dev/sd").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_lowercase())
        });
        if !valid || !devices.insert(name.to_owned()) {
            return Err(io::Error::other(
                "WSL returned an invalid or duplicate block device",
            ));
        }
    }
    if devices.is_empty() {
        return Err(io::Error::other("WSL returned no block devices"));
    }
    Ok(devices)
}

fn open_idle_disk(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    // WSL must release the disk. Allow snapshot readers, but deny writes/deletion.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("the WSL disk must be a regular file"));
    }
    Ok(file)
}

fn check_copy(rescue: &str, working: &Path, logs: &Path) -> io::Result<CheckResult> {
    let before = block_devices(&rescue_shell(rescue, BLOCK_DEVICES).map_err(io::Error::other)?)?;
    let image = working
        .to_str()
        .ok_or_else(|| io::Error::other("invalid VHD path"))?;
    let attempt = || -> io::Result<CheckResult> {
        wsl::run(&["--mount", image, "--vhd", "--bare"]).map_err(io::Error::other)?;
        let after = block_devices(&rescue_shell(rescue, BLOCK_DEVICES).map_err(io::Error::other)?)?;
        let added: Vec<_> = after.difference(&before).collect();
        if !before.is_subset(&after) || added.len() != 1 {
            return Err(io::Error::other(
                "cannot identify exactly one newly attached WSL disk",
            ));
        }
        let script = format!(
            "timeout -s KILL 120 sh -c {} sh {}",
            wsl::shell_quote(FSCK),
            wsl::shell_quote(added[0])
        );
        let output = wsl::run_logged(
            &["-d", rescue, "-u", "root", "--exec", "sh", "-c", &script],
            &logs.join("result.json"),
            &logs.join("console.log"),
        );
        let text = match output {
            Ok(text) => text,
            Err(error) => {
                return Err(io::Error::other(error));
            }
        };
        let result: CheckResult = serde_json::from_str(&text).map_err(io::Error::other)?;
        if !result.verified() || result.restored_files != 0 {
            return Err(io::Error::other(format!(
                "check_exit={}, repair_exit={:?}, verify_exit={}",
                result.check_exit, result.repair_exit, result.verify_exit
            )));
        }
        Ok(result)
    };
    let result = attempt();
    // Always detach this copy, including failed checks. Never detach other disks.
    let detached = wsl::run(&["--unmount", image]);
    match (result, detached) {
        (Ok(result), Ok(_)) => Ok(result),
        (Err(error), Ok(_)) => Err(error),
        (result, Err(error)) => Err(io::Error::other(format!(
            "{}; could not detach repair copy: {error}",
            result
                .err()
                .map(|error| error.to_string())
                .unwrap_or_else(|| "filesystem verified".into())
        ))),
    }
}

pub fn repair(layout: &Layout, rescue: &str) -> Result<snapshot::Repair, Error> {
    if rescue.is_empty() || rescue.eq_ignore_ascii_case(wsl::DISTRO_NAME) {
        return Err(Error::Prerequisite(
            "rescue must use a separate WSL2 distribution".into(),
        ));
    }
    let original = layout.distro().join("ext4.vhdx");
    let repairs = layout.runtime().join("repairs");
    let lock_path = layout.runtime().join("filesystem-repair.lock");
    for path in [&original, &repairs, &lock_path] {
        managed_home::reject_symlink_components(path)?;
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    FileExt::try_lock_exclusive(&lock)?;
    // Fail a WSL query rather than treating an unknown state as stopped.
    let running = wsl::run(&["--list", "--running", "--quiet"])
        .map_err(|error| Error::Prerequisite(error.to_string()))?;
    if wsl::contains(&wsl::parse_list(&running), wsl::DISTRO_NAME) {
        return Err(Error::Prerequisite(
            "firecrab-debian is still running".into(),
        ));
    }
    let source_guard = open_idle_disk(&original)?;
    let retention = snapshot::Repair::begin(&repairs, "vhd-")?;
    let directory = retention.directory().to_path_buf();
    let logs = retention.logs().to_path_buf();
    let backup = directory.join("ext4.before.vhdx");
    let working = directory.join("ext4.repaired.vhdx");
    report!("[BACKUP] WSL disk (OS and data): {}", backup.display());
    let attempt = || -> io::Result<bool> {
        let backup_snapshot = snapshot::create(&original, &backup)?;
        let working_snapshot = snapshot::create(&backup, &working)?;
        retention.record(&backup_snapshot, &working_snapshot)?;
        report!(
            "[CHECK] offline WSL filesystem (up to 120s); results: {}",
            logs.display()
        );
        let result = check_copy(rescue, &working, &logs)?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&working)?
            .sync_all()?;
        Ok(result.changed)
    };
    let result = attempt();
    drop(source_guard);
    match result {
        Ok(true) => {
            if let Err(error) = fs::rename(&working, &original) {
                return Err(Error::Recovery {
                    directory,
                    logs,
                    detail: error.to_string(),
                });
            }
            report!(
                "[PASS] WSL filesystem repaired and verified; backup: {}",
                backup.display()
            );
        }
        Ok(false) => {
            report!("[PASS] WSL filesystem clean; original disk preserved");
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
    use wsl::fake;

    fn installed() -> (tempfile::TempDir, Layout) {
        let root = tempfile::tempdir().unwrap();
        let layout = Layout {
            managed_home: root.path().join("micromanager"),
        };
        fs::create_dir_all(layout.distro()).unwrap();
        fs::create_dir_all(layout.runtime()).unwrap();
        fs::write(
            layout.distro().join("ext4.vhdx"),
            b"original OS and user data",
        )
        .unwrap();
        (root, layout)
    }

    fn staged(layout: &Layout) -> PathBuf {
        fs::read_dir(layout.runtime().join("repairs"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
    }

    fn host(layout: &Layout, result: &'static str, fail: Option<&'static str>) -> fake::Guard {
        let layout = layout.clone();
        let mut attached = false;
        fake::answer(move |line| {
            if fail.is_some_and(|needle| line.contains(needle)) {
                return Err("command failed".into());
            }
            if line.starts_with("wsl.exe --mount") {
                attached = true;
            }
            if line.starts_with("wsl.exe --unmount") {
                attached = false;
            }
            if line.ends_with(BLOCK_DEVICES) {
                return Ok(if attached {
                    "/dev/sda\n/dev/sdb\n/dev/sdc\n"
                } else {
                    "/dev/sda\n/dev/sdb\n"
                }
                .into());
            }
            if line.contains("timeout -s KILL 120") {
                fs::write(
                    staged(&layout).join("ext4.repaired.vhdx"),
                    b"repaired OS and user data",
                )
                .unwrap();
                return Ok(result.into());
            }
            Ok(String::new())
        })
    }

    #[test]
    fn exit_twelve_commits_only_a_verified_detached_copy_and_keeps_the_full_backup() {
        let (_root, layout) = installed();
        let commands = host(
            &layout,
            r#"{"check_exit":12,"repair_exit":1,"verify_exit":0,"changed":true}"#,
            None,
        );
        repair(&layout, "Debian").unwrap();
        assert_eq!(
            fs::read(layout.distro().join("ext4.vhdx")).unwrap(),
            b"repaired OS and user data"
        );
        assert_eq!(
            fs::read(staged(&layout).join("ext4.before.vhdx")).unwrap(),
            b"original OS and user data"
        );
        let calls = commands.calls();
        assert!(
            calls
                .iter()
                .any(|line| line.starts_with("wsl.exe --unmount")
                    && line.ends_with("ext4.repaired.vhdx"))
        );
        assert!(!calls.iter().any(|line| line.contains("--shutdown")
            || line.contains("--unregister")
            || line.contains("--import")));
    }

    #[test]
    fn clean_filesystem_preserves_the_original_file_and_removes_temporary_copies() {
        let (_root, layout) = installed();
        let _host = host(
            &layout,
            r#"{"check_exit":0,"repair_exit":null,"verify_exit":0,"changed":false}"#,
            None,
        );
        repair(&layout, "Debian").unwrap();
        assert_eq!(
            fs::read(layout.distro().join("ext4.vhdx")).unwrap(),
            b"original OS and user data"
        );
        assert_eq!(
            fs::read_dir(layout.runtime().join("repairs"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn failed_check_repair_verification_or_detach_never_replaces_the_installed_disk() {
        for (result, failure) in [
            (
                r#"{"check_exit":12,"repair_exit":null,"verify_exit":0,"changed":false}"#,
                None,
            ),
            (
                r#"{"check_exit":12,"repair_exit":4,"verify_exit":4,"changed":true}"#,
                None,
            ),
            (
                r#"{"check_exit":12,"repair_exit":1,"verify_exit":4,"changed":true}"#,
                None,
            ),
            (
                r#"{"check_exit":8,"repair_exit":1,"verify_exit":0,"changed":true}"#,
                None,
            ),
            (
                r#"{"check_exit":12,"repair_exit":1,"verify_exit":0,"changed":true}"#,
                Some("--unmount"),
            ),
            ("invalid JSON", None),
            ("", Some("timeout -s KILL 120")),
            ("", Some("--mount")),
        ] {
            let (_root, layout) = installed();
            let commands = host(&layout, result, failure);
            assert!(repair(&layout, "Debian").is_err(), "{result}");
            assert_eq!(
                fs::read(layout.distro().join("ext4.vhdx")).unwrap(),
                b"original OS and user data"
            );
            assert_eq!(
                fs::read(staged(&layout).join("ext4.before.vhdx")).unwrap(),
                b"original OS and user data"
            );
            let logs = layout
                .runtime()
                .join("logs/repairs")
                .join(staged(&layout).file_name().unwrap());
            assert!(logs.join("error.log").is_file());
            assert!(!staged(&layout).join("error.log").exists());
            assert!(
                commands
                    .calls()
                    .iter()
                    .any(|line| line.starts_with("wsl.exe --unmount"))
            );
        }
    }

    #[test]
    fn ambiguous_attachment_does_not_run_fsck_and_detaches_only_the_copy() {
        let (_root, layout) = installed();
        let mut attached = false;
        let commands = fake::answer(move |line| {
            if line.starts_with("wsl.exe --mount") {
                attached = true;
            }
            if line.ends_with(BLOCK_DEVICES) {
                return Ok(if attached {
                    "/dev/sda\n/dev/sdb\n/dev/sdc\n"
                } else {
                    "/dev/sda\n"
                }
                .into());
            }
            Ok(String::new())
        });
        assert!(repair(&layout, "Debian").is_err());
        let calls = commands.calls();
        assert!(
            !calls
                .iter()
                .any(|line| line.contains("timeout -s KILL 120"))
        );
        assert!(
            calls
                .iter()
                .any(|line| line.starts_with("wsl.exe --unmount")
                    && line.ends_with("ext4.repaired.vhdx"))
        );
        assert_eq!(
            fs::read(layout.distro().join("ext4.vhdx")).unwrap(),
            b"original OS and user data"
        );
    }

    #[test]
    fn a_running_or_unknown_distribution_and_concurrent_repair_stop_before_copying() {
        for running in [Ok("firecrab-debian\n".into()), Err("query failed".into())] {
            let (_root, layout) = installed();
            let _host = fake::answer(move |_| running.clone());
            assert!(repair(&layout, "Debian").is_err());
            assert!(!layout.runtime().join("repairs").exists());
        }
        let (_root, layout) = installed();
        let lock = File::create(layout.runtime().join("filesystem-repair.lock")).unwrap();
        FileExt::try_lock_exclusive(&lock).unwrap();
        let commands = fake::answer(|line| panic!("concurrent repair touched WSL: {line}"));
        assert!(repair(&layout, "Debian").is_err());
        assert!(commands.calls().is_empty());
    }

    #[test]
    fn malformed_or_duplicate_device_names_are_rejected() {
        for text in [
            "",
            "/dev/sda\n/dev/sda\n",
            "/dev/sda1\n",
            "/dev/sda;reboot\n",
            "/tmp/disk\n",
        ] {
            assert!(block_devices(text).is_err(), "{text}");
        }
    }

    #[test]
    fn missing_or_unsafe_rescue_distributions_do_not_start_the_managed_guest() {
        for names in [
            "[]",
            "[\"firecrab-debian\"]",
            "[\"FIRECRAB-DEBIAN\",\"docker-desktop\"]",
            "invalid JSON",
        ] {
            let commands = fake::answer(move |line| {
                assert!(line.starts_with("powershell.exe"), "{line}");
                Ok(names.into())
            });
            assert!(preflight().is_err());
            assert_eq!(commands.calls().len(), 1);
        }
        let commands = fake::answer(|line| {
            if line.starts_with("powershell.exe") {
                return Ok("[\"Ubuntu\"]".into());
            }
            assert!(
                line.starts_with("wsl.exe -d Ubuntu -u root --exec"),
                "{line}"
            );
            Ok(String::new())
        });
        assert_eq!(preflight().unwrap(), "Ubuntu");
        assert_eq!(commands.calls().len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn fsck_script_handles_exit_twelve_and_refuses_mounted_or_unverifiable_devices() {
        for (check, repair, verify, mounts, probe_exit, sync_exit, verified, calls) in [
            (0, 0, 0, "", 0, 0, true, 1),
            (12, 1, 0, "", 0, 0, true, 3),
            (4, 2, 0, "", 0, 0, true, 3),
            (12, 4, 0, "", 0, 0, false, 2),
            (12, 1, 4, "", 0, 0, false, 3),
            (8, 0, 0, "", 0, 0, false, 1),
            (16, 0, 0, "", 0, 0, false, 1),
            (12, 1, 0, "/mnt/other", 0, 0, false, 0),
            (12, 1, 0, "", 1, 0, false, 0),
            (12, 1, 0, "", 0, 1, false, 3),
        ] {
            let root = tempfile::tempdir().unwrap();
            let trace = root.path().join("calls");
            fs::write(&trace, "").unwrap();
            // Only the block-device stat is replaced; every tool is a shell stub.
            // This exercises the production script without opening a host disk.
            let script = format!(
                r#"count=0
blkid() {{ printf 'ext4\n'; }}
lsblk() {{ printf '%s\n' {mounts}; return {probe_exit}; }}
sync() {{ return {sync_exit}; }}
e2fsck() {{
  printf '%s\n' "$*" >>{trace}
  case $count in 0) result={check};; 1) result={repair};; *) result={verify};; esac
  count=$((count + 1))
  return "$result"
}}
set -- /dev/sdc
{script}"#,
                mounts = wsl::shell_quote(mounts),
                trace = wsl::shell_quote(trace.to_str().unwrap()),
                script = FSCK.replace("[ -b \"$device\" ]", "true"),
            );
            let output = std::process::Command::new("sh")
                .args(["-c", &script])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let result: CheckResult = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(result.verified(), verified, "{result:?}");
            let trace = fs::read_to_string(trace).unwrap();
            assert_eq!(trace.lines().count(), calls, "{trace}");
            if calls == 3 {
                assert_eq!(
                    trace.lines().collect::<Vec<_>>(),
                    ["-f -n /dev/sdc", "-f -y /dev/sdc", "-f -n /dev/sdc"]
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_disk_repair_directory_or_lock_is_rejected_before_host_commands() {
        for relative in [
            "distro/ext4.vhdx",
            "runtime/repairs",
            "runtime/filesystem-repair.lock",
        ] {
            let (root, layout) = installed();
            let path = layout.managed_home.join(relative);
            if path.exists() {
                fs::remove_file(&path).unwrap();
            }
            let outside = root.path().join("outside");
            fs::write(&outside, b"keep").unwrap();
            std::os::unix::fs::symlink(&outside, &path).unwrap();
            let commands = fake::answer(|line| panic!("unsafe path touched WSL: {line}"));
            assert!(repair(&layout, "Debian").is_err());
            assert!(commands.calls().is_empty());
            assert_eq!(fs::read(&outside).unwrap(), b"keep");
        }
    }
}

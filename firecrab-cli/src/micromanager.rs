mod daemon;
mod lifecycle;
mod provision;

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use clap::Subcommand;

const HELPER_NAME: &str = "firecrab-micromanager-macos";
const HELPER_ENV: &str = "FIRECRAB_MICROMANAGER_HELPER";

#[derive(Subcommand)]
pub enum Command {
    /// Install the CLI and native helper for the current user.
    Install,
    /// Replace installed binaries while preserving managed VM data.
    Reinstall,
    /// Remove installed binaries and optionally all managed VM data.
    Uninstall {
        /// Also delete the managed OS and persistent data disks.
        #[arg(long)]
        purge: bool,
    },
    /// Start and register the resident management VM daemon.
    Start,
    /// Stop and unload the resident management VM daemon.
    Stop,
    /// Show launchd, VM readiness, and local API status.
    Status,
    /// Detect Virtualization.framework, entitlement, and nested virtualization support.
    Doctor {
        /// Emit a machine-readable capability report.
        #[arg(long)]
        json: bool,
    },
    /// Show and validate the managed Debian VM configuration.
    Validate,
    /// Boot the managed Debian VM and attach its serial console.
    Run,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not resolve the running firecrab executable: {0}")]
    CurrentExecutable(#[source] std::io::Error),
    #[error("could not find {0}; build or reinstall the signed macOS helper first")]
    MissingHelper(PathBuf),
    #[error(transparent)]
    Lifecycle(#[from] lifecycle::Error),
    #[error(transparent)]
    Daemon(#[from] daemon::Error),
    #[error(transparent)]
    Provision(#[from] provision::Error),
    #[error("could not launch EFI provisioning helper {path}: {source}")]
    ProvisionHelper {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("EFI provisioning helper exited with {0}")]
    ProvisionExit(std::process::ExitStatus),
    #[error("EFI provisioning helper stopped without a success or failure marker")]
    MissingProvisionMarker,
    #[error(
        "could not launch macOS microManager helper {path}: {source}; reinstall the macOS CLI or set {HELPER_ENV}"
    )]
    Spawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub fn run(command: Command) -> Result<i32, Error> {
    match command {
        Command::Install => run_install(false),
        Command::Reinstall => run_install(true),
        Command::Uninstall { purge } => run_uninstall(purge),
        Command::Start => run_start(),
        Command::Stop => run_stop(),
        Command::Status => run_status(),
        helper_command => {
            let helper = helper_path(
                std::env::var_os(HELPER_ENV),
                std::env::current_exe().ok().as_deref(),
            );
            run_with_helper(&helper, &helper_command)
        }
    }
}

fn run_install(reinstall: bool) -> Result<i32, Error> {
    let cli_source = std::env::current_exe().map_err(Error::CurrentExecutable)?;
    let helper_source = resolve_helper_source(
        std::env::var_os(HELPER_ENV),
        Some(&cli_source),
        std::env::var_os("PATH"),
    )?;
    let layout = lifecycle::Layout::from_process_env()?;
    daemon::stop(&layout)?;
    let binaries_in_place = same_file(&cli_source, &layout.cli_path())
        && same_file(&helper_source, &layout.helper_path());
    if !reinstall && binaries_in_place {
        println!("[PASS] binaries: already installed");
    } else {
        lifecycle::install_from(&cli_source, &helper_source, &layout, reinstall)?;
    }
    let artifacts = provision::download_all(&layout.managed_home)?;
    let prepared = provision::prepare(&layout.managed_home, &artifacts)?;
    let guest_result = ensure_guest_provisioned(&layout, &prepared)?;
    let daemon_paths = daemon::install(&layout, &layout.helper_path(), &prepared.ssh_private_key)?;
    let daemon_status = daemon::start(&layout)?;
    if !daemon_status.success() {
        return Err(daemon::Error::InvalidMarker(format!(
            "launchd loaded={}, ready={}, API reachable={}",
            daemon_status.loaded, daemon_status.ready, daemon_status.api_reachable
        ))
        .into());
    }
    println!(
        "microManager {}\n  CLI: {}\n  helper: {}\n  managed data: {}\n  Debian {}: {}\n  Firecracker {}: {}\n  Firecrab {} host: {}\n  guest installer: {}\n  OS disk: {}\n  data disk: {}\n  cloud-init seed: {}\n  EFI variable store: {}\n  provision marker: {}\n  SSH key: {}\n  launchd: {}\n  API: http://127.0.0.1:5523/\n  guest: {}",
        if reinstall {
            "reinstalled"
        } else {
            "installed"
        },
        layout.cli_path().display(),
        layout.helper_path().display(),
        layout.managed_home.display(),
        provision::DEBIAN_BUILD,
        artifacts.debian_archive.display(),
        provision::FIRECRACKER_VERSION,
        artifacts.firecracker_archive.display(),
        provision::FIRECRAB_VERSION,
        artifacts.firecrab_host_archive.display(),
        artifacts.firecrab_installer.display(),
        prepared.os_disk.display(),
        prepared.data_disk.display(),
        prepared.seed_iso.display(),
        prepared.efi_variable_store.display(),
        prepared.provision_marker.display(),
        prepared.ssh_private_key.display(),
        daemon_paths.plist.display(),
        guest_result.lines().collect::<Vec<_>>().join(", ")
    );
    Ok(0)
}

fn run_start() -> Result<i32, Error> {
    let layout = lifecycle::Layout::from_process_env()?;
    let status = daemon::start(&layout)?;
    print_daemon_status(&status);
    Ok(i32::from(!status.success()))
}

fn run_stop() -> Result<i32, Error> {
    let layout = lifecycle::Layout::from_process_env()?;
    daemon::stop(&layout)?;
    println!("[PASS] launchd: stopped");
    Ok(0)
}

fn run_status() -> Result<i32, Error> {
    let layout = lifecycle::Layout::from_process_env()?;
    let status = daemon::status(&layout)?;
    print_daemon_status(&status);
    Ok(i32::from(!status.success()))
}

fn print_daemon_status(status: &daemon::Status) {
    println!(
        "[{}] launchd: {}",
        if status.loaded { "PASS" } else { "FAILED" },
        if status.loaded {
            "loaded"
        } else {
            "not loaded"
        }
    );
    println!(
        "[{}] management_vm: {}",
        if status.ready { "PASS" } else { "FAILED" },
        status
            .detail
            .as_deref()
            .map(str::trim)
            .filter(|detail| !detail.is_empty())
            .unwrap_or("not ready")
            .replace('\n', ", ")
    );
    println!(
        "[{}] api: {}",
        if status.api_reachable {
            "PASS"
        } else {
            "FAILED"
        },
        if status.api_reachable {
            "http://127.0.0.1:5523/"
        } else {
            "unreachable"
        }
    );
}

fn ensure_guest_provisioned(
    layout: &lifecycle::Layout,
    prepared: &provision::PreparedLayout,
) -> Result<String, Error> {
    let failed = prepared.provision_marker.with_file_name("provision.failed");
    if failed.is_file() {
        fs::remove_file(&failed).map_err(|source| lifecycle::Error::Io {
            action: "clear prior provisioning failure",
            path: failed,
            source,
        })?;
    }
    if let Some(result) = provision::guest_result(prepared)? {
        println!("[PASS] guest: Debian provisioning (preserved)");
        return Ok(result);
    }

    println!("[BOOT] Debian EFI provisioning VM");
    let helper = layout.helper_path();
    let status = ProcessCommand::new(&helper)
        .arg("provision")
        .env("FIRECRAB_MICROMANAGER_HOME", &layout.managed_home)
        .status()
        .map_err(|source| Error::ProvisionHelper {
            path: helper,
            source,
        })?;
    if !status.success() {
        return Err(Error::ProvisionExit(status));
    }
    let result = provision::guest_result(prepared)?.ok_or(Error::MissingProvisionMarker)?;
    println!("[PASS] guest: Debian provisioning");
    Ok(result)
}

fn run_uninstall(purge: bool) -> Result<i32, Error> {
    let layout = lifecycle::Layout::from_process_env()?;
    daemon::uninstall(&layout)?;
    lifecycle::uninstall_at(&layout, purge)?;
    println!("microManager uninstalled");
    if purge {
        println!("  purged managed data: {}", layout.managed_home.display());
    } else {
        println!(
            "  preserved managed data: {}",
            layout.managed_home.display()
        );
    }
    Ok(0)
}

fn same_file(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn resolve_helper_source(
    override_path: Option<OsString>,
    current_exe: Option<&Path>,
    path: Option<OsString>,
) -> Result<PathBuf, Error> {
    let candidate = helper_path(override_path, current_exe);
    if candidate.is_file() {
        return Ok(candidate);
    }
    if candidate == Path::new(HELPER_NAME)
        && let Some(path) = path
        && let Some(found) = std::env::split_paths(&path)
            .map(|directory| directory.join(HELPER_NAME))
            .find(|candidate| candidate.is_file())
    {
        return Ok(found);
    }
    Err(Error::MissingHelper(candidate))
}

fn run_with_helper(helper: &Path, command: &Command) -> Result<i32, Error> {
    let arguments = command_arguments(command);
    let status = ProcessCommand::new(helper)
        .args(arguments)
        .status()
        .map_err(|source| Error::Spawn {
            path: helper.to_owned(),
            source,
        })?;
    Ok(status.code().unwrap_or(1))
}

fn helper_path(override_path: Option<OsString>, current_exe: Option<&Path>) -> PathBuf {
    if let Some(override_path) = override_path.filter(|value| !value.is_empty()) {
        return PathBuf::from(override_path);
    }
    if let Some(sibling) = current_exe
        .and_then(Path::parent)
        .map(|directory| directory.join(HELPER_NAME))
        .filter(|path| path.is_file())
    {
        return sibling;
    }
    PathBuf::from(HELPER_NAME)
}

fn command_arguments(command: &Command) -> Vec<OsString> {
    match command {
        Command::Install
        | Command::Reinstall
        | Command::Uninstall { .. }
        | Command::Start
        | Command::Stop
        | Command::Status => {
            unreachable!("lifecycle commands do not invoke the native helper")
        }
        Command::Doctor { json } => {
            let mut arguments = vec![OsString::from("doctor")];
            if *json {
                arguments.push(OsString::from("--json"));
            }
            arguments
        }
        Command::Validate => vec![OsString::from("validate")],
        Command::Run => vec![OsString::from("run")],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: Command,
    }

    #[test]
    fn explicit_helper_override_wins() {
        assert_eq!(
            helper_path(Some(OsString::from("/tmp/custom-helper")), None),
            Path::new("/tmp/custom-helper")
        );
    }

    #[test]
    fn sibling_helper_wins_before_path_lookup() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("firecrab");
        let helper = directory.path().join(HELPER_NAME);
        std::fs::write(&helper, b"helper").unwrap();
        assert_eq!(helper_path(None, Some(&executable)), helper);
    }

    #[test]
    fn missing_sibling_falls_back_to_path_name() {
        assert_eq!(
            helper_path(None, Some(Path::new("/tmp/firecrab"))),
            Path::new(HELPER_NAME)
        );
    }

    #[test]
    fn helper_exit_code_is_forwarded() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let helper = directory.path().join(HELPER_NAME);
        std::fs::write(&helper, b"#!/bin/sh\nexit 7\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();

        let code = run_with_helper(&helper, &Command::Doctor { json: false }).unwrap();
        assert_eq!(code, 7);
    }

    #[test]
    fn same_file_recognizes_canonical_and_symlink_paths() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("firecrab");
        let alias = directory.path().join("firecrab-alias");
        fs::write(&binary, b"binary").unwrap();
        symlink(&binary, &alias).unwrap();
        assert!(same_file(&binary, &binary));
        assert!(same_file(&binary, &alias));
        assert!(!same_file(&binary, &directory.path().join("missing")));
    }

    #[test]
    fn doctor_and_vm_commands_preserve_arguments() {
        let doctor = TestCli::try_parse_from(["test", "doctor", "--json"]).unwrap();
        assert_eq!(
            command_arguments(&doctor.command),
            [OsString::from("doctor"), OsString::from("--json")]
        );

        let validate = TestCli::try_parse_from(["test", "validate"]).unwrap();
        assert_eq!(command_arguments(&validate.command), ["validate"]);

        let run = TestCli::try_parse_from(["test", "run"]).unwrap();
        assert_eq!(command_arguments(&run.command), ["run"]);

        assert!(TestCli::try_parse_from(["test", "validate", "--kernel", "/tmp/Image"]).is_err());
    }
}

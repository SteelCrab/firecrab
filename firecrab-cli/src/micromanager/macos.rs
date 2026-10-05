pub(super) mod daemon;
mod dev;
mod forward;
pub(super) mod lifecycle;
mod provision;

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

use super::{Command, debug, report};

const HELPER_NAME: &str = "firecrab-micromanager-macos";
const HELPER_ENV: &str = "FIRECRAB_MICROMANAGER_HELPER";
/// The guest powers itself off once its script ends. A shutdown that hangs
/// must not hang `install` with it: once the guest reports it is done, the
/// helper gets this long to see the VM stop, then is asked, then made, to.
const PROVISION_POWEROFF_GRACE: Duration = Duration::from_secs(120);
const PROVISION_STOP_GRACE: Duration = Duration::from_secs(30);
const PROVISION_POLL: Duration = Duration::from_millis(250);

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
    #[error(transparent)]
    Forward(#[from] forward::Error),
    #[error(transparent)]
    Dev(#[from] dev::Error),
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
    #[error("could not render the debug report: {0}")]
    Render(#[from] serde_json::Error),
    #[error(transparent)]
    Settings(#[from] super::settings::Error),
    #[error(transparent)]
    Controller(#[from] super::controller::Error),
    #[error("the management VM is not running; run `firecrab service start` first")]
    ShellNotRunning,
    #[error("the management SSH key is missing from {0}; run `firecrab service reinstall`")]
    ShellCredentials(PathBuf),
    #[error("could not run ssh: {0}")]
    ShellSsh(#[source] std::io::Error),
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
        Command::Install { yes } => run_install(false, yes),
        Command::Reinstall { yes } => run_install(true, yes),
        Command::Uninstall { purge } => run_uninstall(purge),
        Command::Start => run_start(),
        Command::Stop => run_stop(),
        Command::Status => run_status(),
        Command::Settings { json, set } => Ok(super::settings::run(
            &lifecycle::Layout::from_process_env()?.managed_home,
            json,
            &set,
        )?),
        Command::Controller { home } => Ok(super::controller::run(&home)?),
        Command::Debug { json, logs, tail } => run_debug(debug::Options::new(json, logs, tail)),
        Command::Dev {
            source,
            release,
            restore,
            yes,
        } => run_dev(source.as_deref(), release, restore, yes),
        Command::Shell { command } => run_shell(&command),
        Command::ForwardPorts {
            manager,
            key,
            known_hosts,
        } => Ok(forward::run(forward::Manager {
            ip: manager,
            key,
            known_hosts,
        })?),
        helper_command => {
            let helper = helper_path(
                std::env::var_os(HELPER_ENV),
                std::env::current_exe().ok().as_deref(),
            );
            run_with_helper(&helper, &helper_command)
        }
    }
}

fn run_install(reinstall: bool, assume_yes: bool) -> Result<i32, Error> {
    let cli_source = std::env::current_exe().map_err(Error::CurrentExecutable)?;
    let helper_source = resolve_helper_source(
        std::env::var_os(HELPER_ENV),
        Some(&cli_source),
        std::env::var_os("PATH"),
    )?;
    let layout = lifecycle::Layout::from_process_env()?;
    let binaries_in_place = same_file(&cli_source, &layout.cli_path())
        && same_file(&helper_source, &layout.helper_path());
    let install_binaries = reinstall || !binaries_in_place;
    if install_binaries {
        lifecycle::check_install(&cli_source, &helper_source, &layout, reinstall)?;
    }
    // Before anything changes, so declining the download leaves the host as it was.
    let artifacts = provision::download_all(&layout.managed_home, assume_yes)?;
    daemon::stop(&layout)?;
    if install_binaries {
        lifecycle::install_from(&cli_source, &helper_source, &layout, reinstall)?;
    } else {
        report!("[PASS] binaries: already installed");
    }
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
    report!(
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
    if super::controller::configured(&layout.managed_home) {
        super::controller::start(&layout.managed_home)?;
        report!("[PASS] microManager: running (automatic wake enabled)");
        return Ok(0);
    }
    let paths = daemon::paths(&layout)?;
    if paths.plist.is_file() && paths.wrapper.is_file() {
        provision::require_guest_provisioned(&layout.managed_home)?;
    }
    let status = daemon::start(&layout)?;
    print_daemon_status(&status);
    Ok(i32::from(!status.success()))
}

fn run_dev(source: Option<&Path>, release: bool, restore: bool, yes: bool) -> Result<i32, Error> {
    // Validate and archive before installing or starting anything.
    let checkout = if restore {
        None
    } else {
        Some(dev::Checkout::prepare(
            source.unwrap_or_else(|| Path::new(".")),
        )?)
    };
    let layout = lifecycle::Layout::from_process_env()?;
    let paths = daemon::paths(&layout)?;
    let _host_work = super::controller::HostWork::begin(&layout.managed_home)?;
    if !paths.wrapper.is_file() || !paths.plist.is_file() {
        if restore {
            return Err(daemon::Error::NotInstalled(paths.wrapper).into());
        }
        report!("[INSTALL] management VM for source development");
        run_install(false, yes)?;
    }
    let mut status = daemon::status(&layout)?;
    if !status.loaded {
        provision::require_guest_provisioned(&layout.managed_home)?;
        status = daemon::start(&layout)?;
    }
    // A failed development API can leave SSH usable without daemon-ready.
    let guest_marker = fs::read_to_string(&paths.manager_ready).unwrap_or_default();
    let ip = status
        .detail
        .as_deref()
        .and_then(manager_ip)
        .or_else(|| manager_ip(&guest_marker))
        .filter(|_| status.loaded)
        .ok_or(dev::Error::GuestNotReady)?;
    dev::deploy(&layout, ip, checkout.as_ref(), release)?;
    let started = Instant::now();
    while !daemon::status(&layout)?.api_reachable {
        if started.elapsed() >= Duration::from_secs(30) {
            return Err(dev::Error::LocalApiUnavailable.into());
        }
        thread::sleep(Duration::from_secs(1));
    }
    report!("[PASS] API: http://127.0.0.1:5523/");
    report!("  logs: firecrab service debug --logs --tail 100");
    Ok(0)
}

fn run_stop() -> Result<i32, Error> {
    let layout = lifecycle::Layout::from_process_env()?;
    if super::controller::active(&layout.managed_home) {
        super::controller::stop(&layout.managed_home)?;
        report!("[PASS] microManager: manually stopped (automatic wake blocked)");
        return Ok(0);
    }
    daemon::stop(&layout)?;
    report!("[PASS] launchd: stopped");
    Ok(0)
}

fn run_status() -> Result<i32, Error> {
    let layout = lifecycle::Layout::from_process_env()?;
    if super::controller::active(&layout.managed_home) {
        return Ok(super::controller::print_status(&layout.managed_home)?);
    }
    let status = daemon::status(&layout)?;
    print_daemon_status(&status);
    print_log_paths(&layout);
    Ok(i32::from(!status.success()))
}

fn run_debug(options: debug::Options) -> Result<i32, Error> {
    let layout = lifecycle::Layout::from_process_env()?;
    let mut report = debug::DebugReport::new(
        "macos",
        &layout.managed_home,
        options,
        &[
            ("daemon", "daemon.log"),
            ("console", "vm-console.log"),
            ("provision", "guest-provision.log"),
        ],
    );
    report.capability = host_capability(&layout);
    match daemon::status(&layout) {
        Ok(status) => {
            let installed = daemon::paths(&layout)
                .is_ok_and(|paths| paths.plist.is_file() && paths.wrapper.is_file());
            report.service = if !installed {
                debug::Probe::fail(
                    "managed launchd files are missing; run `firecrab service install`",
                )
            } else if status.loaded {
                debug::Probe::pass("launchd agent loaded")
            } else {
                debug::Probe::fail("launchd agent not loaded; run `firecrab service start`")
            };
            report.api = if installed && status.ready && status.api_reachable {
                debug::Probe::pass("http://127.0.0.1:5523/")
            } else {
                debug::Probe::fail(
                    "local API is not ready for this managed home; inspect daemon and console logs",
                )
            };
            if installed && status.loaded && status.ready {
                let ip = status.detail.as_deref().and_then(manager_ip);
                report.guest = match ip {
                    Some(ip) => guest_units(&layout, ip),
                    None => debug::Probe::unavailable("daemon-ready has no valid management IP"),
                };
                if options.logs {
                    report.logs.push(debug::LogSource::guest(
                        "guest journal: firecrab-api + firecrab-helper",
                        ip.map_or_else(
                            || Err("management IP unavailable".to_string()),
                            |ip| guest_command(
                                &layout,
                                ip,
                                &format!(
                                    "journalctl --no-pager --output=short-iso -n {} -u firecrab-api -u firecrab-helper -u firecrab-net-helper",
                                    options.tail
                                ),
                            ),
                        ),
                        options.tail,
                    ));
                }
            } else {
                report.guest = debug::Probe::unavailable("management VM is not ready");
                if options.logs {
                    report.logs.push(debug::LogSource::guest(
                        "guest journal: firecrab-api + firecrab-helper",
                        Err("management VM is not running and was not started".to_string()),
                        options.tail,
                    ));
                }
            }
        }
        Err(error) => {
            report.service = debug::Probe::unavailable(error.to_string());
            report.guest = debug::Probe::unavailable("service status unavailable");
            report.api = debug::Probe::unavailable("service status unavailable");
        }
    }
    if let Err(error) = provision::require_guest_provisioned(&layout.managed_home) {
        report.guest = debug::Probe::fail(error.to_string());
        report.api =
            debug::Probe::fail("guest provisioning is incomplete; the local API cannot start");
    }
    if options.json {
        report!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        report!("{}", report.render_human());
    }
    Ok(0)
}

fn host_capability(layout: &lifecycle::Layout) -> debug::Capability {
    let sibling = helper_path(None, std::env::current_exe().ok().as_deref());
    let helper = std::env::var_os(HELPER_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if layout.helper_path().is_file() {
                layout.helper_path()
            } else {
                sibling
            }
        });
    let output = match ProcessCommand::new(&helper)
        .args(["doctor", "--json"])
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            return debug::Capability::unavailable(format!(
                "could not run host capability helper {}: {error}",
                helper.display()
            ));
        }
    };
    match debug::Capability::from_json(&String::from_utf8_lossy(&output.stdout)) {
        Ok(capability) => capability,
        Err(error) => debug::Capability::unavailable(format!(
            "host capability helper returned no valid report: {error}"
        )),
    }
}

/// A root shell in the management VM over its key-only SSH, or one command
/// run there as root.
fn run_shell(command: &[String]) -> Result<i32, Error> {
    use std::io::IsTerminal;

    let layout = lifecycle::Layout::from_process_env()?;
    let _lease = if super::controller::active(&layout.managed_home) {
        Some(super::controller::hold(&layout.managed_home)?)
    } else {
        None
    };
    let paths = daemon::paths(&layout)?;
    let status = daemon::status(&layout)?;
    let guest_marker = fs::read_to_string(&paths.manager_ready).unwrap_or_default();
    let ip = status
        .detail
        .as_deref()
        .and_then(manager_ip)
        .or_else(|| manager_ip(&guest_marker))
        .filter(|_| status.loaded)
        .ok_or(Error::ShellNotRunning)?;
    let runtime = layout.managed_home.join("runtime");
    if !runtime.join("manager_ed25519").is_file() {
        return Err(Error::ShellCredentials(runtime));
    }
    let tty = command.is_empty() || io::stdin().is_terminal();
    let status = ProcessCommand::new("/usr/bin/ssh")
        .args(shell_arguments(&runtime, ip, command, tty))
        .status()
        .map_err(Error::ShellSsh)?;
    Ok(status.code().unwrap_or(1))
}

/// `ssh` arguments for [`run_shell`]. ssh joins everything after the host
/// into one line for the remote shell, so each command argument is quoted to
/// arrive unchanged.
fn shell_arguments(
    runtime: &Path,
    ip: std::net::IpAddr,
    command: &[String],
    tty: bool,
) -> Vec<OsString> {
    let mut arguments: Vec<OsString> = vec!["-i".into(), runtime.join("manager_ed25519").into()];
    for option in [
        "BatchMode=yes",
        "ConnectTimeout=10",
        // The daemon trusts the guest's key the same way on first connect and
        // keeps it in the same file; a changed key is refused.
        "StrictHostKeyChecking=accept-new",
        "UpdateHostKeys=no",
        "LogLevel=ERROR",
    ] {
        arguments.extend(["-o".into(), option.into()]);
    }
    arguments.push("-o".into());
    let mut known_hosts = OsString::from("UserKnownHostsFile=");
    known_hosts.push(runtime.join("known_hosts"));
    arguments.push(known_hosts);
    if tty {
        arguments.push("-t".into());
    }
    arguments.push(format!("root@{ip}").into());
    if !command.is_empty() {
        let line: Vec<String> = command
            .iter()
            .map(|argument| format!("'{}'", argument.replace('\'', "'\\''")))
            .collect();
        arguments.push(line.join(" ").into());
    }
    arguments
}

fn manager_ip(marker: &str) -> Option<std::net::IpAddr> {
    marker
        .lines()
        .find_map(|line| line.strip_prefix("ip="))?
        .parse()
        .ok()
}

fn guest_units(layout: &lifecycle::Layout, ip: std::net::IpAddr) -> debug::Probe {
    match guest_command(
        layout,
        ip,
        "helper=firecrab-helper; if [ \"$(systemctl show --property=LoadState --value firecrab-helper.service)\" = not-found ]; then helper=firecrab-net-helper; fi; for unit in firecrab-api \"$helper\"; do systemctl is-active \"$unit\" || true; done",
    ) {
        Ok(states) => {
            let mut states = states.lines();
            let api = states.next().unwrap_or("unknown");
            let helper = states.next().unwrap_or("unknown");
            let detail = format!("firecrab-api={api}, firecrab-helper={helper}");
            if api == "active" && helper == "active" {
                debug::Probe::pass(detail)
            } else {
                debug::Probe::fail(detail)
            }
        }
        Err(error) => debug::Probe::unavailable(error),
    }
}

fn guest_command(
    layout: &lifecycle::Layout,
    ip: std::net::IpAddr,
    command: &str,
) -> Result<String, String> {
    let runtime = layout.managed_home.join("runtime");
    let key = runtime.join("manager_ed25519");
    let known_hosts = runtime.join("known_hosts");
    if !key.is_file() || !known_hosts.is_file() {
        return Err("management SSH credentials are not ready".to_string());
    }
    let output = ProcessCommand::new("/usr/bin/ssh")
        .arg("-i")
        .arg(&key)
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=3"])
        .args(["-o", "StrictHostKeyChecking=yes", "-o", "UpdateHostKeys=no"])
        .args(["-o", "LogLevel=ERROR"])
        .arg("-o")
        .arg(format!("UserKnownHostsFile={}", known_hosts.display()))
        .arg(format!("root@{ip}"))
        .arg(command)
        .output()
        .map_err(|error| format!("could not query management VM: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if output.status.success() {
        Ok(stdout)
    } else if !stdout.is_empty() {
        Err(stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn print_log_paths(layout: &lifecycle::Layout) {
    let runtime = layout.managed_home.join("runtime");
    report!("  logs: daemon {}", runtime.join("daemon.log").display());
    report!(
        "        console {}",
        runtime.join("vm-console.log").display()
    );
    report!(
        "        provision {}",
        runtime.join("guest-provision.log").display()
    );
}

fn print_daemon_status(status: &daemon::Status) {
    report!(
        "[{}] launchd: {}",
        if status.loaded { "PASS" } else { "FAILED" },
        if status.loaded {
            "loaded"
        } else {
            "not loaded"
        }
    );
    report!(
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
    report!(
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
        report!("[PASS] guest: Debian provisioning (preserved)");
        return Ok(result);
    }
    // A stale "complete" would start the power-off clock before the guest boots.
    let phase = prepared.provision_marker.with_file_name("provision.phase");
    match fs::remove_file(&phase) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(lifecycle::Error::Io {
                action: "clear prior provisioning phase",
                path: phase,
                source,
            }
            .into());
        }
    }

    report!("[BOOT] Debian EFI provisioning VM");
    let helper = layout.helper_path();
    let mut child = ProcessCommand::new(&helper)
        .arg("provision")
        .env("FIRECRAB_MICROMANAGER_HOME", &layout.managed_home)
        .spawn()
        .map_err(|source| Error::ProvisionHelper {
            path: helper.clone(),
            source,
        })?;
    let run = wait_for_provisioning(
        &mut child,
        || provision::guest_finished(prepared),
        PROVISION_POWEROFF_GRACE,
        PROVISION_STOP_GRACE,
    )
    .map_err(|source| Error::ProvisionHelper {
        path: helper,
        source,
    })?;
    match run {
        ProvisionRun::Exited(status) if !status.success() => {
            return Err(Error::ProvisionExit(status));
        }
        ProvisionRun::Exited(_) => {}
        ProvisionRun::Stopped => report!(
            "[WARNING] guest: provisioning finished but the VM did not power off within {}s; stopped it",
            PROVISION_POWEROFF_GRACE.as_secs()
        ),
    }
    // A stopped run still has to have left the success marker behind.
    let result = provision::guest_result(prepared)?.ok_or(Error::MissingProvisionMarker)?;
    report!("[PASS] guest: Debian provisioning");
    Ok(result)
}

#[derive(Debug)]
enum ProvisionRun {
    Exited(ExitStatus),
    /// The guest finished its script but never powered off, so the helper was stopped.
    Stopped,
}

fn wait_for_provisioning(
    child: &mut Child,
    guest_finished: impl Fn() -> bool,
    poweroff_grace: Duration,
    stop_grace: Duration,
) -> io::Result<ProvisionRun> {
    let mut finished_at = None;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(ProvisionRun::Exited(status));
        }
        match finished_at {
            None if guest_finished() => finished_at = Some(Instant::now()),
            Some(at) if Instant::now().duration_since(at) >= poweroff_grace => break,
            _ => {}
        }
        thread::sleep(PROVISION_POLL);
    }
    // The helper turns SIGTERM into a VZ stop request; SIGKILL ends it outright.
    let _ = ProcessCommand::new("/bin/kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    let asked = Instant::now();
    while asked.elapsed() < stop_grace {
        if child.try_wait()?.is_some() {
            return Ok(ProvisionRun::Stopped);
        }
        thread::sleep(PROVISION_POLL);
    }
    let _ = child.kill();
    child.wait()?;
    Ok(ProvisionRun::Stopped)
}

fn run_uninstall(purge: bool) -> Result<i32, Error> {
    let layout = lifecycle::Layout::from_process_env()?;
    if purge {
        lifecycle::validate_purge(&layout)?;
    }
    super::controller::uninstall(&layout.managed_home)?;
    daemon::uninstall(&layout)?;
    lifecycle::uninstall_at(&layout, purge)?;
    report!("microManager uninstalled");
    if purge {
        report!("  purged managed data: {}", layout.managed_home.display());
    } else {
        report!(
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
        Command::Install { .. }
        | Command::Reinstall { .. }
        | Command::Uninstall { .. }
        | Command::Start
        | Command::Stop
        | Command::Status
        | Command::Debug { .. }
        | Command::Dev { .. }
        | Command::Shell { .. }
        | Command::ForwardPorts { .. } => {
            unreachable!("lifecycle and relay commands do not invoke the native helper")
        }
        Command::Settings { .. } | Command::Controller { .. } => {
            unreachable!("host controller commands do not invoke the native helper")
        }
        Command::Doctor { json } => {
            let mut arguments = vec![OsString::from("doctor")];
            if *json {
                arguments.push(OsString::from("--json"));
            }
            arguments
        }
        Command::Validate => vec![OsString::from("validate")],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn shell_ip() -> std::net::IpAddr {
        "192.0.2.7".parse().unwrap()
    }

    #[test]
    fn the_shell_is_an_interactive_root_login_over_the_management_key() {
        let runtime = Path::new("/managed/runtime");
        let arguments = shell_arguments(runtime, shell_ip(), &[], true);
        let text: Vec<String> = arguments
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();

        assert_eq!(text[..2], ["-i", "/managed/runtime/manager_ed25519"]);
        assert!(text.contains(&"-t".to_owned()));
        assert!(text.contains(&"StrictHostKeyChecking=accept-new".to_owned()));
        assert!(text.contains(&"UserKnownHostsFile=/managed/runtime/known_hosts".to_owned()));
        assert_eq!(text.last().unwrap(), "root@192.0.2.7");
    }

    #[test]
    fn a_shell_command_reaches_the_guest_with_each_argument_intact() {
        let runtime = Path::new("/managed/runtime");
        let command = ["sh", "-c", "echo it's here"].map(String::from);
        let arguments = shell_arguments(runtime, shell_ip(), &command, false);
        let text: Vec<String> = arguments
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();

        assert!(!text.contains(&"-t".to_owned()), "no terminal without one");
        let at = text
            .iter()
            .position(|argument| argument == "root@192.0.2.7")
            .unwrap();
        // ssh joins its remaining arguments into one remote shell line, so
        // each argument is quoted for that shell.
        assert_eq!(text[at + 1..], ["'sh' '-c' 'echo it'\\''s here'"]);
    }

    #[test]
    fn debug_accepts_only_an_ip_from_the_ready_marker() {
        assert_eq!(
            manager_ip("vm_pid=42\nip=192.0.2.7\ntunnel_pid=43\n"),
            Some("192.0.2.7".parse().unwrap())
        );
        assert_eq!(manager_ip("ip=bad address\n"), None);
    }
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

    fn provisioning(script: &str) -> Child {
        ProcessCommand::new("/bin/sh")
            .args(["-c", script])
            .spawn()
            .unwrap()
    }

    const SHORT: Duration = Duration::from_millis(300);

    #[test]
    fn a_helper_that_exits_is_reported_as_is() {
        let mut child = provisioning("exit 3");
        let run = wait_for_provisioning(&mut child, || true, SHORT, SHORT).unwrap();
        assert!(matches!(run, ProvisionRun::Exited(status) if status.code() == Some(3)));
    }

    #[test]
    fn a_running_guest_is_never_stopped_before_it_finishes() {
        // Stands in for a guest that is still provisioning: it exits on its own.
        let mut child = provisioning("sleep 1; exit 0");
        let run = wait_for_provisioning(&mut child, || false, SHORT, SHORT).unwrap();
        assert!(matches!(run, ProvisionRun::Exited(status) if status.success()));
    }

    #[test]
    fn a_finished_guest_that_never_powers_off_is_asked_to_stop() {
        let mut child = provisioning("exec sleep 600");
        let started = Instant::now();
        let run =
            wait_for_provisioning(&mut child, || true, SHORT, Duration::from_secs(30)).unwrap();
        assert!(matches!(run, ProvisionRun::Stopped));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "SIGTERM ended it well before the kill deadline"
        );
        assert!(child.try_wait().unwrap().is_some());
    }

    #[test]
    fn a_helper_that_ignores_the_stop_request_is_killed() {
        let mut child = provisioning("trap '' TERM; while :; do sleep 1; done");
        let run = wait_for_provisioning(&mut child, || true, SHORT, SHORT).unwrap();
        assert!(matches!(run, ProvisionRun::Stopped));
        assert!(child.try_wait().unwrap().is_some());
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

        assert!(TestCli::try_parse_from(["test", "run"]).is_err());

        assert!(TestCli::try_parse_from(["test", "validate", "--kernel", "/tmp/Image"]).is_err());
    }
}

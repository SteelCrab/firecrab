//! microManager on Windows: a managed WSL2 Debian distribution that runs Firecrab.
//!
//! Commands and their output follow the macOS backend line for line, so the
//! same `[PASS]`/`[FAILED]` reading works on both hosts.

mod daemon;
mod dev;
mod doctor;
mod filesystem;
pub(super) mod lifecycle;
mod provision;
mod repair;
pub(super) mod resource;
mod wsl;

use super::dev::dashboard;
use super::{Command, DevCommand, debug, managed_home, manifest};
use doctor::Status;
pub(super) use wsl::DISTRO_NAME;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Resource(#[from] super::resource::Error),
    #[error("the host is not ready; run `firecrab service doctor` and fix the FAILED checks")]
    NotReady,
    #[error("could not render the capability report: {0}")]
    Render(#[from] serde_json::Error),
    #[error(transparent)]
    Lifecycle(#[from] lifecycle::Error),
    #[error(transparent)]
    Provision(#[from] provision::Error),
    #[error(transparent)]
    Manifest(#[from] manifest::Error),
    #[error(transparent)]
    ManagedHome(#[from] managed_home::Error),
    #[error(transparent)]
    Daemon(#[from] daemon::Error),
    #[error(transparent)]
    Dev(#[from] super::dev::Error),
    #[error(transparent)]
    Repair(#[from] repair::Error),
    #[error(transparent)]
    Wsl(#[from] wsl::Error),
    #[error("the management VM did not become healthy: {0}")]
    Unhealthy(String),
    #[error("{DISTRO_NAME} is not imported; run `firecrab service install`")]
    NotInstalled,
    #[error("the management VM is not running; run `firecrab service start` first")]
    GuestNotRunning,
    #[error("could not run wsl.exe: {0}")]
    Console(#[source] std::io::Error),
    #[error("`firecrab service {0}` is only used by the macOS microManager daemon")]
    MacosOnly(&'static str),
}

pub fn run(command: Command) -> Result<i32, Error> {
    match command {
        Command::Resource { options } => Ok(super::resource::run(options)?),
        Command::ResourceServer { home } => Ok(super::resource_server::run(home)?),
        Command::ResourceApply { id } => Ok(super::resource::run_apply(&id)?),
        Command::Install { yes } => run_install(false, yes),
        Command::Reinstall { yes } => run_install(true, yes),
        #[cfg(any(target_os = "macos", target_os = "windows", test))]
        Command::Repair => {
            let status = repair::run(&lifecycle::Layout::from_process_env()?)?;
            print_daemon_status(&status);
            Ok(0)
        }
        Command::Uninstall { purge } => {
            run_uninstall(&lifecycle::Layout::from_process_env()?, purge)
        }
        Command::Start => run_start(),
        Command::Stop => run_stop(),
        Command::Status => run_status(),
        Command::Debug { json, logs, tail } => run_debug(debug::Options::new(json, logs, tail)),
        Command::Doctor { json } => run_doctor(json),
        Command::Validate => {
            let layout = lifecycle::Layout::from_process_env()?;
            let installed = manifest::Manifest::installed(&layout.managed_home)?;
            let host = match &installed {
                Some(manifest) => provision::host_from_manifest(manifest, std::env::consts::ARCH)?,
                None => provision::host()?.clone(),
            };
            run_validate(&host, &layout)
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        Command::Shell { command } => run_shell(&command),
        Command::ForwardPorts { .. } => Err(Error::MacosOnly("forward-ports")),
        Command::Dev {
            command,
            source,
            release,
            restore,
            yes,
        } => match command {
            Some(command) => run_dev_control(command),
            None => run_dev(source.as_deref(), release, restore, yes),
        },
    }
}

fn run_doctor(json: bool) -> Result<i32, Error> {
    let report = doctor::report(doctor::Inputs::live());
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{}", doctor::render_human(&report));
    }
    Ok(i32::from(!report.ready))
}

fn run_install(reinstall: bool, assume_yes: bool) -> Result<i32, Error> {
    let report = doctor::report(doctor::Inputs::live());
    if !report.ready {
        println!("{}", doctor::render_human(&report));
        return Err(Error::NotReady);
    }
    let layout = lifecycle::Layout::from_process_env()?;
    let manifest = manifest::Manifest::for_install(&layout.managed_home)?;
    let host = provision::host_from_manifest(&manifest, std::env::consts::ARCH)?;
    lifecycle::prepare(&layout)?;
    // Before the service stops, so declining the download leaves it running.
    let artifacts = provision::download_all(&host, &layout.downloads(), assume_yes)?;
    if reinstall {
        daemon::uninstall(&layout)?;
        managed_home::clear_installation_state(&layout.managed_home)?;
    } else {
        daemon::stop()?;
    }

    let imported = provision::ensure_distro(&layout, &artifacts.debian_rootfs)?;
    let guest = provision::ensure_provisioned(&layout, &host, reinstall || imported)?;
    manifest.verify_provisioned(
        &guest,
        "workload_kernel",
        host.kernel_version.expect("TOML host has a kernel"),
    )?;
    let task =
        daemon::install_with_runtime(&layout, &manifest.runtime.clone().unwrap_or_default())?;
    let status = daemon::start()?;
    if !status.success() {
        print_daemon_status(&status);
        return Err(Error::Unhealthy(format!(
            "task={:?}, ready={}, API reachable={}",
            status.task, status.ready, status.api_reachable
        )));
    }
    manifest.save(&layout.managed_home)?;
    println!(
        "microManager {}\n  managed data: {}\n  distribution: {DISTRO_NAME} (WSL 2, {})\n  Debian WSL rootfs {}: {}\n  Firecracker {}: {}\n  Firecrab {} host: {}\n  guest installer: {}\n  provision marker: {}\n  scheduled task: \\{} ({})\n  API: http://127.0.0.1:5523/\n  guest: {}",
        if reinstall {
            "reinstalled"
        } else {
            "installed"
        },
        layout.managed_home.display(),
        host.architecture,
        host.rootfs_version,
        artifacts.debian_rootfs.display(),
        host.firecracker_version,
        artifacts.firecracker.display(),
        host.firecrab_version,
        artifacts.firecrab_host.display(),
        artifacts.installer.display(),
        layout.provision_marker().display(),
        daemon::TASK_NAME,
        task.display(),
        guest.lines().collect::<Vec<_>>().join(", ")
    );
    println!(
        "  manifest: {}",
        layout.managed_home.join(manifest::FILE_NAME).display()
    );
    Ok(0)
}

fn run_start() -> Result<i32, Error> {
    if daemon::task_state() == daemon::TaskState::Missing {
        return Err(daemon::Error::NotInstalled.into());
    }
    let layout = lifecycle::Layout::from_process_env()?;
    daemon::install(&layout)?;
    let status = daemon::start()?;
    print_daemon_status(&status);
    Ok(i32::from(!status.success()))
}

fn run_dev(
    source: Option<&std::path::Path>,
    release: bool,
    restore: bool,
    yes: bool,
) -> Result<i32, Error> {
    // Invalid source must fail before installing or starting the distribution.
    let checkout = if restore {
        None
    } else {
        Some(super::dev::Checkout::prepare(
            source.unwrap_or_else(|| std::path::Path::new(".")),
        )?)
    };
    let layout = lifecycle::Layout::from_process_env()?;
    let frontend = checkout
        .as_ref()
        .map(|checkout| dashboard::prepare(&checkout.root))
        .transpose()
        .map_err(super::dev::Error::from)?;
    if !layout.runtime().join("microManager-task.xml").is_file()
        || daemon::task_state() == daemon::TaskState::Missing
        || !wsl::contains(&wsl::distributions(), DISTRO_NAME)
    {
        if restore {
            return Err(Error::NotInstalled);
        }
        super::report!("[INSTALL] management VM for source development");
        run_install(false, yes)?;
    }
    if provision::guest_result(&layout)?.is_none() {
        return Err(Error::NotInstalled);
    }
    // Keep WSL resident without requiring the previous API to be healthy:
    // deployment and restore must also recover a broken development service.
    daemon::resume()?;
    dev::deploy(checkout.as_ref(), release)?;
    let status = daemon::wait_ready(std::time::Duration::from_secs(30))?;
    if !status.success() {
        print_daemon_status(&status);
        return Err(super::dev::Error::LocalApiUnavailable.into());
    }
    super::report!("[PASS] API: http://127.0.0.1:5523/");
    if let Some(source) = frontend {
        dashboard::start(&source, &layout.managed_home).map_err(super::dev::Error::from)?;
    } else {
        dashboard::stop(&layout.managed_home).map_err(super::dev::Error::from)?;
    }
    super::report!("  logs: firecrab service debug --logs --tail 100");
    Ok(0)
}

fn run_dev_control(command: DevCommand) -> Result<i32, Error> {
    if !wsl::is_running(DISTRO_NAME) {
        return Err(Error::GuestNotRunning);
    }
    dev::control(command)?;
    let layout = lifecycle::Layout::from_process_env()?;
    if command != DevCommand::Start {
        dashboard::stop(&layout.managed_home).map_err(super::dev::Error::from)?;
    }
    if command != DevCommand::Stop {
        let source = dashboard::source(&layout.managed_home).map_err(super::dev::Error::from)?;
        dashboard::start(&source, &layout.managed_home).map_err(super::dev::Error::from)?;
    }
    Ok(0)
}

fn run_stop() -> Result<i32, Error> {
    match lifecycle::Layout::from_process_env() {
        Ok(layout) => dashboard::stop(&layout.managed_home).map_err(super::dev::Error::from)?,
        Err(lifecycle::Error::MissingManagedHome) => {}
        Err(error) => return Err(error.into()),
    }
    daemon::stop()?;
    println!("[PASS] task_scheduler: stopped");
    Ok(0)
}

fn run_status() -> Result<i32, Error> {
    let status = daemon::status();
    print_daemon_status(&status);
    Ok(i32::from(!status.success()))
}

fn run_debug(options: debug::Options) -> Result<i32, Error> {
    let layout = lifecycle::Layout::from_process_env()?;
    let report = collect_debug(&layout, options);
    if options.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{}", report.render_human());
    }
    Ok(0)
}

fn collect_debug(layout: &lifecycle::Layout, options: debug::Options) -> debug::DebugReport {
    let mut report = debug::DebugReport::new(
        "windows",
        &layout.managed_home,
        options,
        &[
            ("dashboard", "dev-dashboard.log"),
            ("provision", "guest-provision.log"),
        ],
    );
    let capabilities = doctor::report(doctor::Inputs::debug());
    report.capability = debug::Capability::from_checks(
        capabilities.ready,
        capabilities
            .checks
            .into_iter()
            .map(|check| debug::CapabilityCheck {
                id: check.id.to_string(),
                status: match check.status {
                    Status::Pass => "pass",
                    Status::Warning => "warning",
                    Status::Fail => "fail",
                }
                .to_string(),
                detail: check.detail,
                fix: check.fix,
            })
            .collect(),
    );
    let status = daemon::status();
    let task_definition = layout.runtime().join("microManager-task.xml").is_file();
    let distro_disk = layout.distro().join("ext4.vhdx").is_file();
    report.service = match (task_definition, status.task) {
        (false, _) => debug::Probe::fail(
            "managed scheduled-task file missing; run `firecrab service install`",
        ),
        (true, daemon::TaskState::Enabled) => debug::Probe::pass("scheduled task enabled"),
        (true, daemon::TaskState::Disabled) => {
            debug::Probe::fail("scheduled task disabled; run `firecrab service start`")
        }
        (true, daemon::TaskState::Missing) => {
            debug::Probe::fail("scheduled task missing; run `firecrab service install`")
        }
    };
    report.guest = if !distro_disk {
        debug::Probe::unavailable("managed WSL disk missing; inspect provisioning logs")
    } else if status.ready {
        debug::Probe::pass(
            status
                .detail
                .unwrap_or_else(|| "guest services active".to_string()),
        )
    } else {
        debug::Probe::fail(
            status
                .detail
                .unwrap_or_else(|| "management VM not ready".to_string()),
        )
    };
    report.api = if distro_disk && status.api_reachable {
        debug::Probe::pass("http://127.0.0.1:5523/")
    } else {
        debug::Probe::fail("localhost API unreachable; inspect guest services and provisioning log")
    };
    if options.logs {
        let journal = if distro_disk && wsl::is_running(DISTRO_NAME) {
            wsl::root_shell(&format!(
                "journalctl --no-pager --output=short-iso -n {} -u firecrab-api -u firecrab-helper -u firecrab-net-helper",
                options.tail
            ))
            .map_err(|error| error.to_string())
        } else {
            Err("managed distribution is missing or stopped; it was not started".to_string())
        };
        report.logs.push(debug::LogSource::guest(
            "guest journal: firecrab-api + firecrab-helper",
            journal,
            options.tail,
        ));
    }
    report
}

fn print_daemon_status(status: &daemon::Status) {
    let task = match status.task {
        daemon::TaskState::Enabled => "enabled",
        daemon::TaskState::Disabled => "disabled",
        daemon::TaskState::Missing => "not registered",
    };
    println!(
        "[{}] task_scheduler: {task}",
        Status::from_pass(status.task == daemon::TaskState::Enabled).label()
    );
    println!(
        "[{}] management_vm: {}",
        Status::from_pass(status.ready).label(),
        status
            .detail
            .as_deref()
            .filter(|detail| !detail.is_empty())
            .unwrap_or("not ready")
    );
    println!(
        "[{}] api: {}",
        Status::from_pass(status.api_reachable).label(),
        if status.api_reachable {
            "http://127.0.0.1:5523/"
        } else {
            "unreachable"
        }
    );
}

/// Every managed setting with its own status line, all reported in one run.
fn run_validate(host: &provision::Host, layout: &lifecycle::Layout) -> Result<i32, Error> {
    let mut lines = vec![(
        if layout.managed_home.is_dir() {
            Status::Pass
        } else {
            Status::Fail
        },
        "managed_home",
        layout.managed_home.display().to_string(),
    )];
    lines.extend(
        provision::validate_downloads(host, &layout.downloads())
            .into_iter()
            .map(|(label, result)| match result {
                Ok(()) => (Status::Pass, "download", format!("{label}: verified")),
                Err(error) => (Status::Fail, "download", format!("{label}: {error}")),
            }),
    );
    lines.push(if wsl::contains(&wsl::distributions(), DISTRO_NAME) {
        (
            Status::Pass,
            "distribution",
            format!("{DISTRO_NAME} is registered"),
        )
    } else {
        (
            Status::Fail,
            "distribution",
            format!("{DISTRO_NAME} is not registered"),
        )
    });
    lines.push(match provision::guest_result(layout) {
        Ok(Some(result)) => (
            Status::Pass,
            "provision",
            result.lines().collect::<Vec<_>>().join(", "),
        ),
        Ok(None) => (Status::Fail, "provision", "not provisioned".to_string()),
        Err(error) => (Status::Fail, "provision", error.to_string()),
    });
    lines.push(match daemon::task_state() {
        daemon::TaskState::Enabled => (Status::Pass, "task_scheduler", "enabled".to_string()),
        daemon::TaskState::Disabled => (
            Status::Warning,
            "task_scheduler",
            "disabled; `firecrab service start` enables it".to_string(),
        ),
        daemon::TaskState::Missing => {
            (Status::Fail, "task_scheduler", "not registered".to_string())
        }
    });
    for (status, id, detail) in &lines {
        println!("[{}] {id}: {detail}", status.label());
    }
    Ok(i32::from(
        lines.iter().any(|(status, ..)| *status == Status::Fail),
    ))
}

/// A root shell in the managed distribution, or one command run there as
/// root. The distribution keeps running while it is open.
fn run_shell(command: &[String]) -> Result<i32, Error> {
    if !wsl::contains(&wsl::distributions(), DISTRO_NAME) {
        return Err(Error::NotInstalled);
    }
    let status = std::process::Command::new("wsl.exe")
        .args(shell_arguments(command))
        .status()
        .map_err(Error::Console)?;
    Ok(status.code().unwrap_or(1))
}

/// `wsl.exe` arguments for [`run_shell`]. A command runs through `--exec`, so
/// each argument reaches it unchanged instead of being re-parsed by a shell.
fn shell_arguments(command: &[String]) -> Vec<String> {
    let mut arguments = ["-d", DISTRO_NAME, "-u", "root", "--cd", "~"]
        .map(String::from)
        .to_vec();
    if !command.is_empty() {
        arguments.push("--exec".to_owned());
        arguments.extend(command.iter().cloned());
    }
    arguments
}

/// Non-interactive stdin transport; PowerShell never converts credential bytes.
pub(crate) fn run_guest_command(
    command: &[String],
    input: Option<std::fs::File>,
) -> Result<i32, Error> {
    if !wsl::contains(&wsl::distributions(), DISTRO_NAME) {
        return Err(Error::NotInstalled);
    }
    let stdin = input
        .map(std::process::Stdio::from)
        .unwrap_or_else(std::process::Stdio::null);
    let status = std::process::Command::new("wsl.exe")
        .args(shell_arguments(command))
        .stdin(stdin)
        .status()
        .map_err(Error::Console)?;
    Ok(status.code().unwrap_or(1))
}

/// Without `--purge` the distribution stays registered, so its Firecrab data
/// and the managed home survive for the next install, as on macOS.
fn run_uninstall(layout: &lifecycle::Layout, purge: bool) -> Result<i32, Error> {
    if purge {
        lifecycle::validate_purge(layout)?;
    }
    dashboard::stop(&layout.managed_home).map_err(super::dev::Error::from)?;
    daemon::uninstall(layout)?;
    if purge {
        if wsl::contains(&wsl::distributions(), DISTRO_NAME) {
            wsl::run(&["--unregister", DISTRO_NAME])?;
        }
        lifecycle::purge(layout)?;
    }
    println!("microManager uninstalled");
    if purge {
        println!(
            "  purged managed data: {} and WSL distribution {DISTRO_NAME}",
            layout.managed_home.display()
        );
    } else {
        println!(
            "  preserved managed data: {} and WSL distribution {DISTRO_NAME}",
            layout.managed_home.display()
        );
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wsl::fake;

    const ENABLED: &str = "<Task><Settings><Enabled>true</Enabled></Settings></Task>";

    #[test]
    fn the_shell_is_a_root_console_in_the_managed_distribution() {
        assert_eq!(
            shell_arguments(&[]),
            ["-d", DISTRO_NAME, "-u", "root", "--cd", "~"]
        );
    }

    #[test]
    fn a_shell_command_runs_as_given_without_the_login_shell() {
        let command = ["systemctl", "status", "firecrab-api"].map(String::from);
        assert_eq!(
            shell_arguments(&command),
            [
                "-d",
                DISTRO_NAME,
                "-u",
                "root",
                "--cd",
                "~",
                "--exec",
                "systemctl",
                "status",
                "firecrab-api"
            ]
        );
    }

    fn layout() -> (tempfile::TempDir, lifecycle::Layout) {
        let directory = tempfile::tempdir().expect("temp dir");
        let layout = lifecycle::Layout {
            managed_home: directory.path().join("Firecrab").join("micromanager"),
        };
        lifecycle::prepare(&layout).expect("managed directories");
        (directory, layout)
    }

    /// A host with WSL but nothing else registered or running.
    fn empty_host(line: &str) -> wsl::fake::Reply {
        match line {
            "wsl.exe --version" => {
                Ok("WSL version: 2.7.14.0\nKernel version: 6.18.33.2-2\n".into())
            }
            "cmd.exe /c ver" => Ok("Microsoft Windows [Version 10.0.26200.1]\n".into()),
            _ => Err("not found".into()),
        }
    }

    #[test]
    fn doctor_passes_the_readiness_through_as_the_exit_code() {
        let _wsl = fake::answer(empty_host);
        assert_eq!(run(Command::Doctor { json: false }).expect("reported"), 0);
        assert_eq!(run(Command::Doctor { json: true }).expect("reported"), 0);

        let _no_wsl = fake::answer(|_| Err("not found".into()));
        assert_eq!(run(Command::Doctor { json: false }).expect("reported"), 1);
    }

    #[test]
    fn install_stops_at_a_failed_doctor() {
        let _no_wsl = fake::answer(|_| Err("not found".into()));
        assert!(matches!(
            run(Command::Install { yes: false }),
            Err(Error::NotReady)
        ));
        assert!(matches!(
            run(Command::Reinstall { yes: false }),
            Err(Error::NotReady)
        ));
    }

    #[test]
    fn invalid_development_source_fails_before_any_host_command() {
        let directory = tempfile::tempdir().unwrap();
        let wsl = fake::answer(|line| panic!("invalid checkout touched the host: {line}"));
        assert!(matches!(
            run_dev(Some(directory.path()), false, false, false),
            Err(Error::Dev(super::super::dev::Error::Checkout { .. }))
        ));
        assert!(wsl.calls().is_empty());
    }

    #[test]
    fn dev_controls_use_the_running_guest_without_changing_micro_manager() {
        for command in [DevCommand::Start, DevCommand::Stop, DevCommand::Restart] {
            let action = command.as_str();
            let wsl = fake::answer(move |line| match line {
                "wsl.exe --list --running --quiet" => Ok("firecrab-debian\n".into()),
                l if l
                    == format!("wsl.exe -d firecrab-debian -u root --exec bash -s -- {action}") =>
                {
                    Ok(String::new())
                }
                other => panic!("dev control changed the host: {other}"),
            });
            dev::control(command).unwrap();
            assert_eq!(wsl.calls().len(), 1);
        }
    }

    #[test]
    fn dev_controls_do_not_boot_a_stopped_guest() {
        for command in [DevCommand::Start, DevCommand::Stop, DevCommand::Restart] {
            let wsl = fake::answer(|line| match line {
                "wsl.exe --list --running --quiet" => Ok("Debian\n".into()),
                other => panic!("dev control booted a guest: {other}"),
            });
            assert!(matches!(
                run_dev_control(command),
                Err(Error::GuestNotRunning)
            ));
            assert_eq!(wsl.calls().len(), 1);
        }
    }

    #[test]
    fn dev_control_failure_is_reported_without_starting_micro_manager() {
        let wsl = fake::answer(|line| match line {
            "wsl.exe --list --running --quiet" => Ok("firecrab-debian\n".into()),
            l if l.contains("--exec bash -s -- restart") => Err("restart failed".into()),
            other => panic!("dev control changed the host: {other}"),
        });
        assert!(matches!(
            run_dev_control(DevCommand::Restart),
            Err(Error::Wsl(_))
        ));
        assert_eq!(wsl.calls().len(), 2);
    }

    #[test]
    fn status_and_stop_report_an_uninstalled_host() {
        let _wsl = fake::answer(empty_host);
        assert_eq!(run(Command::Status).expect("reported"), 1);
        assert_eq!(run(Command::Stop).expect("nothing to stop"), 0);
        assert!(matches!(
            run(Command::Start),
            Err(Error::Daemon(daemon::Error::NotInstalled))
        ));
    }

    #[test]
    fn debug_reads_failure_markers_without_starting_stopped_wsl() {
        let (_directory, layout) = layout();
        std::fs::write(
            layout.runtime().join("provision.phase"),
            "install-packages\n",
        )
        .unwrap();
        std::fs::write(layout.runtime().join("provision.failed"), "1\n").unwrap();
        std::fs::write(layout.runtime().join("microManager-task.xml"), ENABLED).unwrap();
        std::fs::write(layout.distro().join("ext4.vhdx"), "test disk").unwrap();
        let wsl = fake::answer(|line| match line {
            l if l.starts_with("schtasks.exe /Query") => Ok(ENABLED.into()),
            "wsl.exe --version" => Ok("WSL version: 2.7.14.0\nKernel version: 6.18.33.2\n".into()),
            "wsl.exe --list --quiet" => Ok("firecrab-debian\n".into()),
            "wsl.exe --list --running --quiet" => Ok(String::new()),
            "cmd.exe /c ver" => Ok("Microsoft Windows [Version 10.0.26200.1]".into()),
            other => panic!("debug started or queried a stopped guest: {other}"),
        });
        let report = collect_debug(&layout, debug::Options::new(true, true, Some(20)));
        assert_eq!(report.capability.probe.state, "unavailable");
        assert_eq!(report.provision.phase.as_deref(), Some("install-packages"));
        assert_eq!(report.provision.failure.as_deref(), Some("1"));
        assert_eq!(report.guest.state, "failed");
        assert_eq!(report.logs.last().unwrap().state, "unavailable");
        assert!(wsl.calls().iter().all(|call| !call.contains(" -d ")));
    }

    #[test]
    fn debug_reads_live_guest_journal() {
        let (_directory, layout) = layout();
        std::fs::write(layout.runtime().join("microManager-task.xml"), ENABLED).unwrap();
        std::fs::write(layout.distro().join("ext4.vhdx"), "test disk").unwrap();
        let _wsl = fake::answer(|line| match line {
            l if l.starts_with("schtasks.exe /Query") => Ok(ENABLED.into()),
            "wsl.exe --version" => Ok("WSL version: 2.7.14.0\nKernel version: 6.18.33.2\n".into()),
            "wsl.exe --list --quiet" => Ok("firecrab-debian\n".into()),
            "wsl.exe --list --running --quiet" => Ok("firecrab-debian\n".into()),
            "cmd.exe /c ver" => Ok("Microsoft Windows [Version 10.0.26200.1]".into()),
            l if l.contains("printf 'kernel=") => Ok(format!(
                "kernel=6.18.33.2\nmachine={}\nuptime=900\nkvm=present\nkvm_open=ok\nnested=yes\n",
                std::env::consts::ARCH
            )),
            l if l.contains("systemctl is-active") => {
                Ok("firecrab-api=active\nfirecrab-helper=active\nip=172.20.0.2\n".into())
            }
            l if l.contains("journalctl") => Ok("old\nAuthorization: Bearer secret\nlast\n".into()),
            other => panic!("unexpected command: {other}"),
        });
        let report = collect_debug(&layout, debug::Options::new(true, true, Some(2)));
        assert_eq!(report.capability.probe.state, "pass");
        assert_eq!(report.guest.state, "pass");
        assert_eq!(
            report.logs.last().unwrap().excerpt.as_deref(),
            Some("[REDACTED]\nlast")
        );
    }

    #[test]
    fn shell_needs_the_managed_distribution() {
        let _wsl = fake::answer(|_| Ok("Debian\n".into()));
        assert!(matches!(run_shell(&[]), Err(Error::NotInstalled)));
    }

    #[test]
    fn validate_fails_until_everything_is_in_place() {
        let (_directory, layout) = layout();
        let _wsl = fake::answer(|line| match line {
            "wsl.exe --list --quiet" => Ok("firecrab-debian\n".into()),
            l if l.starts_with("schtasks.exe /Query") => Ok(ENABLED.into()),
            other => panic!("unexpected {other}"),
        });
        let host = provision::host().expect("this host is supported");
        assert_eq!(
            run_validate(host, &layout).expect("reported"),
            1,
            "nothing is downloaded"
        );
    }

    #[test]
    fn uninstall_keeps_the_distribution_and_data_without_purge() {
        let (_directory, layout) = layout();
        let wsl = fake::answer(|line| match line {
            l if l.starts_with("schtasks.exe /Query") => Ok(ENABLED.into()),
            "wsl.exe --list --running --quiet" => Ok(String::new()),
            _ => Ok(String::new()),
        });
        assert_eq!(run_uninstall(&layout, false).expect("uninstalled"), 0);
        assert!(layout.managed_home.is_dir());
        assert!(!wsl.calls().iter().any(|call| call.contains("--unregister")));
    }

    #[test]
    fn purge_unregisters_the_distribution_and_deletes_the_managed_home() {
        let (_directory, layout) = layout();
        manifest::Manifest::bundled()
            .unwrap()
            .save(&layout.managed_home)
            .unwrap();
        std::fs::write(
            layout.provision().join("legacy-install.sh"),
            b"legacy installer",
        )
        .unwrap();
        std::fs::write(layout.provision_marker(), b"legacy marker").unwrap();
        std::fs::write(layout.distro().join("ext4.vhdx"), b"distribution data").unwrap();
        let wsl = fake::answer(|line| match line {
            "wsl.exe --list --quiet" => Ok("Debian\nfirecrab-debian\n".into()),
            "wsl.exe --list --running --quiet" => Ok(String::new()),
            l if l.starts_with("schtasks.exe /Query") => Err("not found".into()),
            _ => Ok(String::new()),
        });
        assert_eq!(run_uninstall(&layout, true).expect("purged"), 0);
        assert!(!layout.managed_home.exists());
        assert!(
            wsl.calls()
                .contains(&"wsl.exe --unregister firecrab-debian".to_string())
        );
        assert!(
            !wsl.calls()
                .iter()
                .any(|line| line == "wsl.exe --unregister Debian")
        );
    }

    #[test]
    fn an_unsafe_purge_is_refused_before_anything_is_removed() {
        let directory = tempfile::tempdir().expect("temp dir");
        let layout = lifecycle::Layout {
            managed_home: directory.path().join("firecrab"),
        };
        let _wsl = fake::answer(|line| panic!("nothing may run: {line}"));
        assert!(matches!(
            run_uninstall(&layout, true),
            Err(Error::Lifecycle(_))
        ));
    }
}

//! microManager — the host-side launcher for the managed Debian guest.
//!
//! Each host has its own backend module; everything they share lives beside them.

#[cfg(any(target_os = "macos", target_os = "windows", test))]
mod artifact;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod controller;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
mod debug;
pub(crate) mod dev;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
mod host_platform;
#[cfg(target_os = "macos")]
mod macos;
mod managed_home;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
mod manifest;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
mod os_repair;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
mod resource;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
mod resource_server;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
mod runtime;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
mod settings;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
mod settings_tui;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
mod sleepy;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
mod snapshot;
// Everything but the `wsl.exe` and `schtasks.exe` calls is plain Rust, so the
// Windows backend also builds and runs its tests on the Linux CI runner.
#[cfg(any(target_os = "windows", test))]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod windows;

use std::io::{self, Write};
use std::time::Duration;

use clap::Subcommand;

#[cfg(target_os = "macos")]
pub use macos::run;
#[cfg(target_os = "macos")]
pub(crate) use macos::run_guest_command;
#[cfg(target_os = "windows")]
pub use windows::run;
#[cfg(target_os = "windows")]
pub(crate) use windows::run_guest_command;

/// Use the activated controller port, preserving the old port until settings
/// are applied. Explicit API flags and saved remote hosts take precedence.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn local_api_base() -> Result<Option<String>, String> {
    #[cfg(target_os = "windows")]
    let home = windows::lifecycle::Layout::from_process_env().map(|layout| layout.managed_home);
    #[cfg(target_os = "macos")]
    let home = macos::lifecycle::Layout::from_process_env().map(|layout| layout.managed_home);
    let Ok(home) = home else { return Ok(None) };
    controller::local_api_base(&home).map_err(|error| error.to_string())
}

/// `println!` panics if the write returns an error, and on some terminal
/// multiplexers a burst of output (a progress bar redraw, then this line)
/// can make a `write(2)` return `EAGAIN` even on an inherited, blocking-by-default
/// fd. Retry transient failures so a fully successful install/uninstall/status
/// report never crashes the process on its way out the door.
macro_rules! report {
    ($($arg:tt)*) => {{
        $crate::micromanager::print_report(format_args!("{}\n", format_args!($($arg)*)))
    }};
}
pub(crate) use report;

/// Writes one formatted chunk to stdout without a trailing newline, so a
/// progress line can redraw itself in place.
pub(crate) fn print_report(args: std::fmt::Arguments<'_>) {
    let text = args.to_string();
    write_retrying_transient_errors(&mut io::stdout(), text.as_bytes());
}

const WOULD_BLOCK_BACKOFF: Duration = Duration::from_millis(1);

/// Writes `bytes` in full, retrying `WouldBlock`/`Interrupted` instead of the
/// bubbling-up-then-panicking that `println!` does. Any other error is
/// swallowed: a status line is best-effort and must never crash a process
/// whose actual work already succeeded.
fn write_retrying_transient_errors(writer: &mut impl Write, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        match writer.write(bytes) {
            Ok(0) => break,
            Ok(n) => bytes = &bytes[n..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            // The terminal is draining; yield instead of spinning a core on it.
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(WOULD_BLOCK_BACKOFF);
            }
            Err(_) => return,
        }
    }
    let _ = writer.flush();
}

/// `firecrab service` on every host that runs Firecrab inside a managed Debian
/// VM. Common lifecycle verbs share one definition; platform tools are gated.
#[derive(Subcommand)]
#[cfg(any(target_os = "macos", target_os = "windows", test))]
pub enum Command {
    /// Show or change management VM CPU, RAM, disk, and networking.
    #[command(name = "resorce", visible_alias = "resource")]
    Resource {
        #[command(flatten)]
        options: resource::Options,
    },
    /// Native, loopback-only resource control used by the Host screen.
    #[command(hide = true)]
    ResourceServer {
        #[arg(long)]
        home: Option<std::path::PathBuf>,
    },
    /// Detached apply worker; survives the managed service restart.
    #[command(hide = true)]
    ResourceApply { id: String },
    /// Install microManager and provision the managed Debian VM.
    Install {
        /// Skip the download confirmation prompt (implied when not attached to a TTY).
        #[arg(short, long)]
        yes: bool,
    },
    /// Reinstall microManager while preserving managed VM data.
    Reinstall {
        /// Skip the download confirmation prompt (implied when not attached to a TTY).
        #[arg(short, long)]
        yes: bool,
    },
    /// Repair the resident service using the existing managed VM and installed binaries.
    ///
    /// Windows requires PowerShell running as Administrator for offline disk repair.
    #[cfg(any(target_os = "macos", target_os = "windows", test))]
    Repair,
    /// Remove microManager and optionally all managed VM data.
    Uninstall {
        /// Also delete the managed Debian VM and its persistent data.
        #[arg(long)]
        purge: bool,
    },
    /// Start the resident management VM and wait for the local API.
    Start,
    /// Stop the resident management VM; keep the development dashboard running.
    Stop,
    /// Restart the resident management VM and wait for the local API; keep the dev dashboard.
    Restart,
    /// Show the resident service, VM readiness, and local API status.
    Status,
    /// Check that this host can run the managed Debian VM.
    Doctor {
        /// Emit a machine-readable capability report.
        #[arg(long)]
        json: bool,
    },
    /// Show and validate the managed Debian VM configuration.
    Validate,
    /// Edit microManager settings in a terminal (no workload management).
    Settings {
        /// Print saved settings without starting the management VM.
        #[arg(long, conflicts_with = "set")]
        json: bool,
        /// Save one or more settings, e.g. --set sleepy=true --set idle_minutes=10.
        #[arg(long, value_name = "KEY=VALUE")]
        set: Vec<String>,
    },
    /// Host-side API proxy; launched by the resident controller service.
    #[command(hide = true)]
    Controller {
        #[arg(long)]
        home: std::path::PathBuf,
    },
    /// Diagnose the managed VM without changing its state.
    Debug {
        /// Emit a machine-readable diagnostic report.
        #[arg(long)]
        json: bool,
        /// Include bounded excerpts from available logs.
        #[arg(long)]
        logs: bool,
        /// Number of lines from each log (requires --logs; maximum 1000).
        #[arg(long, requires = "logs", value_parser = clap::value_parser!(u16).range(1..=1000))]
        tail: Option<u16>,
    },
    /// Build/deploy guest services and start the source dashboard.
    #[command(args_conflicts_with_subcommands = true)]
    Dev {
        #[command(subcommand)]
        command: Option<DevCommand>,
        /// Checkout root (defaults to the current directory).
        #[arg(long, conflicts_with = "restore")]
        source: Option<std::path::PathBuf>,
        /// Development dashboard port (does not change the API port).
        #[arg(long, default_value_t = 8080, conflicts_with = "restore", value_parser = clap::value_parser!(u16).range(1..))]
        port: u16,
        /// Build optimized guest binaries instead of debug binaries.
        #[arg(long, conflicts_with = "restore")]
        release: bool,
        /// Stop workload MicroVMs and restore the installed release binaries.
        #[arg(long)]
        restore: bool,
        /// Skip the artifact download prompt if the management VM needs installation.
        #[arg(short, long, conflicts_with = "restore")]
        yes: bool,
    },
    /// Open a root shell in the managed Debian guest, or run one command there.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[cfg_attr(target_os = "windows", command(alias = "run"))]
    Shell {
        /// Command to run instead of an interactive shell, e.g. `systemctl status firecrab-api`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Serve running VMs' TCP port forwards on 127.0.0.1; run by the macOS daemon.
    #[command(hide = true)]
    ForwardPorts {
        /// Management VM address that DNATs the forwarded host ports.
        manager: std::net::IpAddr,
        /// Management SSH private key.
        #[arg(long)]
        key: std::path::PathBuf,
        /// Management SSH known_hosts file.
        #[arg(long)]
        known_hosts: std::path::PathBuf,
    },
}

/// Lifecycle operations shared by native and container development backends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Subcommand)]
pub enum DevCommand {
    /// Start the deployed API/helper and dashboard without rebuilding or booting the VM.
    Start,
    /// Stop the API/helper if the VM is running, and stop the development dashboard.
    Stop,
    /// Restart the deployed API/helper and dashboard without rebuilding or rebooting the VM.
    Restart,
    /// Stop workload MicroVMs, restore installed services, and remove development state.
    Clean,
}

impl DevCommand {
    /// Verb accepted by the shared in-guest development transaction script.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Clean => "clean",
        }
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
    fn write_retrying_transient_errors_survives_would_block_and_short_writes() {
        struct Flaky {
            out: Vec<u8>,
            would_block_left: u32,
        }
        impl Write for Flaky {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                if self.would_block_left > 0 {
                    self.would_block_left -= 1;
                    return Err(io::Error::from(io::ErrorKind::WouldBlock));
                }
                // Also exercise a short write, one byte at a time.
                let n = buf.len().min(1);
                self.out.extend_from_slice(&buf[..n]);
                Ok(n)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut flaky = Flaky {
            out: Vec::new(),
            would_block_left: 2,
        };
        write_retrying_transient_errors(&mut flaky, b"microManager installed\n");
        assert_eq!(flaky.out, b"microManager installed\n");
    }

    #[test]
    fn write_retrying_transient_errors_gives_up_on_a_real_error_without_panicking() {
        struct AlwaysBroken;
        impl Write for AlwaysBroken {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        // Must return, not panic: a status line is best-effort.
        write_retrying_transient_errors(&mut AlwaysBroken, b"anything\n");
    }

    #[test]
    fn install_and_reinstall_accept_short_and_long_yes() {
        for flag in ["-y", "--yes"] {
            let install = TestCli::try_parse_from(["test", "install", flag]).unwrap();
            assert!(matches!(install.command, Command::Install { yes: true }));

            let reinstall = TestCli::try_parse_from(["test", "reinstall", flag]).unwrap();
            assert!(matches!(
                reinstall.command,
                Command::Reinstall { yes: true }
            ));
        }

        let install = TestCli::try_parse_from(["test", "install"]).unwrap();
        assert!(matches!(install.command, Command::Install { yes: false }));
    }

    #[test]
    fn debug_log_tail_is_explicit_and_bounded() {
        let parsed =
            TestCli::try_parse_from(["test", "debug", "--json", "--logs", "--tail", "50"]).unwrap();
        assert!(matches!(
            parsed.command,
            Command::Debug {
                json: true,
                logs: true,
                tail: Some(50)
            }
        ));
        assert!(TestCli::try_parse_from(["test", "debug", "--tail", "50"]).is_err());
        assert!(TestCli::try_parse_from(["test", "debug", "--logs", "--tail", "0"]).is_err());
        assert!(TestCli::try_parse_from(["test", "debug", "--logs", "--tail", "1001"]).is_err());
    }

    #[test]
    fn repair_requires_no_install_or_download_options() {
        let repair = TestCli::try_parse_from(["test", "repair"]).unwrap();
        assert!(matches!(repair.command, Command::Repair));
        assert!(TestCli::try_parse_from(["test", "repair", "--yes"]).is_err());
        assert!(TestCli::try_parse_from(["test", "repair", "--purge"]).is_err());
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn shell_opens_a_console_or_runs_one_command() {
        let cli = TestCli::try_parse_from(["test", "shell"]).unwrap();
        assert!(matches!(cli.command, Command::Shell { ref command } if command.is_empty()));

        for line in [
            &["test", "shell", "systemctl", "status", "firecrab-api"][..],
            &["test", "shell", "--", "systemctl", "status", "firecrab-api"][..],
        ] {
            let cli = TestCli::try_parse_from(line).unwrap();
            assert!(matches!(
                cli.command,
                Command::Shell { ref command } if command == &["systemctl", "status", "firecrab-api"]
            ));
        }

        // Options after the command belong to the command, not to `shell`.
        let cli =
            TestCli::try_parse_from(["test", "shell", "journalctl", "-u", "firecrab-api"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Shell { ref command } if command == &["journalctl", "-u", "firecrab-api"]
        ));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn run_still_opens_the_shell_on_windows() {
        let cli = TestCli::try_parse_from(["test", "run"]).unwrap();
        assert!(matches!(cli.command, Command::Shell { ref command } if command.is_empty()));
    }

    #[test]
    fn dev_restore_is_independent_of_source_and_build_options() {
        let cli = TestCli::try_parse_from(["test", "dev", "--restore"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Dev {
                restore: true,
                source: None,
                ..
            }
        ));
        assert!(TestCli::try_parse_from(["test", "dev", "--restore", "--source", "."]).is_err());
        assert!(TestCli::try_parse_from(["test", "dev", "--restore", "--release"]).is_err());
        assert!(TestCli::try_parse_from(["test", "dev", "--restore", "--port", "8098"]).is_err());
        let cli = TestCli::try_parse_from([
            "test",
            "dev",
            "--source",
            "/tmp/firecrab",
            "--release",
            "--yes",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Dev {
                release: true,
                yes: true,
                restore: false,
                ..
            }
        ));
    }

    #[test]
    fn dev_dashboard_port_defaults_to_8080_and_accepts_only_nonzero_ports() {
        let cli = TestCli::try_parse_from(["test", "dev"]).unwrap();
        assert!(matches!(cli.command, Command::Dev { port: 8080, .. }));
        for port in ["1", "8098", "65535"] {
            let cli = TestCli::try_parse_from(["test", "dev", "--port", port]).unwrap();
            assert!(
                matches!(cli.command, Command::Dev { port: parsed, .. } if parsed.to_string() == port)
            );
        }
        for port in ["0", "65536", "-1", "invalid"] {
            assert!(TestCli::try_parse_from(["test", "dev", "--port", port]).is_err());
        }
        assert!(TestCli::try_parse_from(["test", "dev", "--port"]).is_err());
    }

    #[test]
    fn management_vm_controls_are_separate_from_dev_controls() {
        for (action, dev_command) in [
            ("start", DevCommand::Start),
            ("stop", DevCommand::Stop),
            ("restart", DevCommand::Restart),
        ] {
            let cli = TestCli::try_parse_from(["test", action]).unwrap();
            assert!(matches!(
                (cli.command, action),
                (Command::Start, "start") | (Command::Stop, "stop") | (Command::Restart, "restart")
            ));
            let cli = TestCli::try_parse_from(["test", "dev", action]).unwrap();
            assert!(matches!(
                cli.command,
                Command::Dev { command: Some(command), .. } if command == dev_command
            ));
        }
    }

    #[test]
    fn dev_controls_are_separate_from_build_and_restore_options() {
        let cli = TestCli::try_parse_from(["test", "dev"]).unwrap();
        assert!(matches!(cli.command, Command::Dev { command: None, .. }));

        for command in [
            DevCommand::Start,
            DevCommand::Stop,
            DevCommand::Restart,
            DevCommand::Clean,
        ] {
            let cli = TestCli::try_parse_from(["test", "dev", command.as_str()]).unwrap();
            assert!(matches!(
                cli.command,
                Command::Dev { command: Some(parsed), .. } if parsed == command
            ));
            for options in [
                &["--source", "."][..],
                &["--port", "8098"][..],
                &["--release"][..],
                &["--restore"][..],
                &["--yes"][..],
            ] {
                let mut args = vec!["test", "dev"];
                args.extend_from_slice(options);
                args.push(command.as_str());
                assert!(TestCli::try_parse_from(args).is_err());
                let mut args = vec!["test", "dev", command.as_str()];
                args.extend_from_slice(options);
                assert!(TestCli::try_parse_from(args).is_err());
            }
        }
    }
}

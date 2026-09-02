use clap::{Parser, Subcommand};

mod api_client;
mod doctor;
#[cfg(test)]
mod host_api_tests;
mod image;
mod info;
mod network;
mod service;
mod shell;
mod status;
mod update;
mod vm;
mod vm_console;

/// `clap`-derived top-level CLI, replacing `scripts/firecrab-doctor.sh`.
#[derive(Parser)]
#[command(name = "firecrab", version, about = "firecrab host CLI")]
struct Cli {
    /// Override the API base URL (else FIRECRAB_API, else http://127.0.0.1:5523).
    #[arg(long, global = true, value_name = "URL")]
    api: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Diagnose host readiness for firecrab (KVM, nft, dnsmasq, UFW, ...).
    Doctor {
        /// Also print sha256 (first 12 hex chars) of template images.
        #[arg(long)]
        digest: bool,
        /// Emit the [`doctor::Report`] as JSON instead of the human format.
        #[arg(long)]
        json: bool,
    },
    /// Show version and resolved host configuration paths.
    Info {
        /// Emit the [`info::InfoReport`] as JSON instead of the human format.
        #[arg(long)]
        json: bool,
    },
    /// Show systemd unit status and the API host status.
    Status {
        /// Emit the [`status::StatusReport`] as JSON instead of the human format.
        #[arg(long)]
        json: bool,
    },
    /// Check for a newer firecrab release, and optionally install it.
    Update {
        /// Only report whether a newer release exists (the default).
        #[arg(long)]
        check: bool,
        /// Download the matching host bundle and hand the swap to the helper.
        #[arg(long, conflicts_with = "check")]
        apply: bool,
        /// Emit the report as JSON instead of the human format.
        #[arg(long)]
        json: bool,
    },
    /// Manage MicroVMs through the host API.
    Vm {
        #[command(subcommand)]
        command: vm::Command,
    },
    /// Manage MicroNetworks through the host API.
    Network {
        #[command(subcommand)]
        command: network::Command,
    },
    /// Inspect template images through the host API.
    Image {
        #[command(subcommand)]
        command: image::Command,
    },
    /// Install, remove, or control the firecrab host services.
    Service {
        #[command(subcommand)]
        command: service::Command,
    },
}

fn main() {
    std::process::exit(run(Cli::parse()));
}

fn run(cli: Cli) -> i32 {
    let Cli { api, command } = cli;
    match command {
        Command::Doctor { digest, json } => run_doctor(digest, json),
        Command::Info { json } => {
            run_info(json, api);
            0
        }
        Command::Status { json } => {
            run_status(json, api);
            0
        }
        Command::Update { check, apply, json } => run_update(check, apply, json),
        Command::Vm { command } => {
            let client = build_api_client(api.as_deref());
            finish_api_command(vm::run(&client, command))
        }
        Command::Network { command } => {
            let client = build_api_client(api.as_deref());
            finish_api_command(network::run(&client, command))
        }
        Command::Image { command } => {
            let client = build_api_client(api.as_deref());
            finish_api_command(image::run(&client, command))
        }
        Command::Service { command } => match service::run(&shell::RealCommandRunner, command) {
            Ok(()) => 0,
            Err(error) => {
                error.report();
                1
            }
        },
    }
}

fn build_api_client(api: Option<&str>) -> api_client::ApiClient {
    api_client::ApiClient::new(api_client::resolve_api_base(api))
}

fn finish_api_command<E: std::fmt::Display>(result: Result<(), E>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

/// Runs the `doctor` subcommand end-to-end (env resolution, the live
/// check run, and either output format) and returns the process exit code
/// [`doctor::Report::exit_code`] computes. Split out of `main()` so this
/// logic is unit-testable without needing a live `std::process::exit` —
/// same pattern as `firecrab-api/src/main.rs`'s `run()`.
fn run_doctor(digest: bool, json: bool) -> i32 {
    let env = doctor::DoctorEnv::from_process_env();
    let runner = shell::RealCommandRunner;
    let report = doctor::run_all(&env, &runner, digest);
    if json {
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    } else {
        doctor::print_human(&report);
    }
    report.exit_code()
}

/// Runs the `info` subcommand end-to-end — extracted for the same
/// testability reason as [`run_doctor`].
fn run_info(json: bool, api: Option<String>) {
    let api_base = api_client::resolve_api_base(api.as_deref());
    let report = info::collect(&api_base);
    if json {
        info::print_json(&report);
    } else {
        info::print_human(&report);
    }
}

/// Runs the `status` subcommand end-to-end — extracted for the same
/// testability reason as [`run_doctor`].
fn run_status(json: bool, api: Option<String>) {
    let runner = shell::RealCommandRunner;
    let api_base = api_client::resolve_api_base(api.as_deref());
    let client = api_client::ApiClient::new(api_base);
    let report = status::collect(&runner, &client);
    if json {
        status::print_json(&report);
    } else {
        status::print_human(&report);
    }
}

/// Runs the `update` subcommand end-to-end and returns the process exit code —
/// extracted for the same testability reason as [`run_doctor`].
///
/// With neither flag given this behaves as `--check`: a read-only default is
/// the safe one for a command that can otherwise replace every binary on the
/// host.
fn run_update(check: bool, apply: bool, json: bool) -> i32 {
    let outcome = update::run_check();
    if check || !apply {
        if json {
            update::print_check_json(&outcome.report);
        } else {
            update::print_check_human(&outcome.report);
        }
        return i32::from(outcome.report.error.is_some());
    }

    match update::run_apply(&outcome) {
        Ok(update::ApplyOutcome::AlreadyCurrent) => {
            if json {
                update::print_check_json(&outcome.report);
            } else {
                println!("already at {}", outcome.report.current);
            }
            0
        }
        Ok(update::ApplyOutcome::Applied { version }) => {
            if json {
                update::print_check_json(&outcome.report);
            } else {
                println!("firecrab {version} installed");
                println!("  firecrab-api and firecrab-net-helper are restarting now");
            }
            0
        }
        Err(error) => {
            if json {
                let mut report = outcome.report.clone();
                report.error = Some(error.to_string());
                update::print_check_json(&report);
            } else {
                eprintln!("[ERROR] {error}");
            }
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These call the real subcommand bodies against the real host (real
    // DoctorEnv/RealCommandRunner, a real unreachable-by-design API base).
    // They intentionally assert only structural things — "produced a
    // valid exit code", "didn't panic" — never host-specific pass/fail
    // outcomes, since those depend on what's actually installed on the
    // machine running the tests.

    #[test]
    fn run_doctor_json_returns_valid_exit_code() {
        let code = run_doctor(false, true);
        assert!(code == 0 || code == 1, "unexpected exit code {code}");
    }

    #[test]
    fn run_doctor_human_returns_valid_exit_code() {
        let code = run_doctor(false, false);
        assert!(code == 0 || code == 1, "unexpected exit code {code}");
    }

    #[test]
    fn run_info_json_does_not_panic() {
        run_info(true, Some("http://127.0.0.1:1".to_owned()));
    }

    #[test]
    fn run_info_human_does_not_panic() {
        run_info(false, Some("http://127.0.0.1:1".to_owned()));
    }

    #[test]
    fn run_status_json_does_not_panic() {
        // Port 1 is a reserved, never-listening port, so the API portion
        // fails fast (connection refused) instead of waiting out the
        // client's 3s timeout.
        run_status(true, Some("http://127.0.0.1:1".to_owned()));
    }

    #[test]
    fn run_status_human_does_not_panic() {
        run_status(false, Some("http://127.0.0.1:1".to_owned()));
    }

    #[test]
    fn cli_parses_doctor_digest_and_json_flags() {
        let cli = Cli::try_parse_from(["firecrab", "doctor", "--digest", "--json"]).unwrap();
        match cli.command {
            Command::Doctor { digest, json } => {
                assert!(digest);
                assert!(json);
            }
            _ => panic!("expected Doctor"),
        }
    }

    #[test]
    fn cli_parses_info_api_flag() {
        let cli = Cli::try_parse_from(["firecrab", "info", "--api", "http://x:1"]).unwrap();
        assert_eq!(cli.api.as_deref(), Some("http://x:1"));
        match cli.command {
            Command::Info { json } => {
                assert!(!json);
            }
            _ => panic!("expected Info"),
        }
    }

    #[test]
    fn cli_parses_status_subcommand() {
        let cli = Cli::try_parse_from(["firecrab", "status"]).unwrap();
        assert!(matches!(cli.command, Command::Status { json: false }));
        assert!(cli.api.is_none());
    }

    #[test]
    fn cli_rejects_unknown_subcommand() {
        assert!(Cli::try_parse_from(["firecrab", "bogus"]).is_err());
    }

    #[test]
    fn cli_parses_update_flags() {
        let cli = Cli::try_parse_from(["firecrab", "update", "--apply", "--json"]).unwrap();
        match cli.command {
            Command::Update { check, apply, json } => {
                assert!(!check);
                assert!(apply);
                assert!(json);
            }
            _ => panic!("expected Update"),
        }
    }

    #[test]
    fn cli_defaults_update_to_a_read_only_check() {
        let cli = Cli::try_parse_from(["firecrab", "update"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Update {
                check: false,
                apply: false,
                json: false
            }
        ));
    }

    #[test]
    fn cli_rejects_check_and_apply_together() {
        assert!(Cli::try_parse_from(["firecrab", "update", "--check", "--apply"]).is_err());
    }

    #[test]
    fn run_update_check_json_returns_a_valid_exit_code() {
        let _guard = update::ENV_LOCK.lock().unwrap();
        // Point the check at a reserved, never-listening port so this never
        // reaches the real GitHub API (rate limits, offline CI sandboxes).
        // SAFETY: serialized by update::ENV_LOCK against every other
        // env-touching test in this crate.
        unsafe { std::env::set_var("FIRECRAB_RELEASE_API", "http://127.0.0.1:1/releases/latest") };
        let code = run_update(true, false, true);
        unsafe { std::env::remove_var("FIRECRAB_RELEASE_API") };
        assert_eq!(code, 1, "an unreachable check must exit non-zero");
    }

    #[test]
    fn run_update_check_human_does_not_panic() {
        let _guard = update::ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by update::ENV_LOCK — see the note above.
        unsafe { std::env::set_var("FIRECRAB_RELEASE_API", "http://127.0.0.1:1/releases/latest") };
        let code = run_update(false, false, false);
        unsafe { std::env::remove_var("FIRECRAB_RELEASE_API") };
        assert!(code == 0 || code == 1, "unexpected exit code {code}");
    }

    #[test]
    fn cli_parses_vm_commands() {
        let list = Cli::try_parse_from([
            "firecrab",
            "vm",
            "list",
            "--json",
            "--api",
            "http://host.test:5523",
        ])
        .unwrap();
        assert_eq!(list.api.as_deref(), Some("http://host.test:5523"));
        assert!(matches!(
            list.command,
            Command::Vm {
                command: vm::Command::List { json: true }
            }
        ));

        let create = Cli::try_parse_from([
            "firecrab",
            "--api",
            "http://host.test:5523",
            "vm",
            "create",
            "--name",
            "demo",
            "--template",
            "alpine-3.24.1",
            "--network",
            "11111111-1111-4111-8111-111111111111",
        ])
        .unwrap();
        assert_eq!(create.api.as_deref(), Some("http://host.test:5523"));
        assert!(matches!(
            create.command,
            Command::Vm {
                command: vm::Command::Create {
                    cpu: 1,
                    ram: 512,
                    disk_gb: 2,
                    ..
                }
            }
        ));

        for action in ["start", "stop", "delete"] {
            assert!(
                Cli::try_parse_from([
                    "firecrab",
                    "vm",
                    action,
                    "11111111-1111-4111-8111-111111111111"
                ])
                .is_ok(),
                "failed to parse vm {action}"
            );
        }
    }

    #[test]
    fn cli_parses_network_commands() {
        assert!(Cli::try_parse_from(["firecrab", "network", "list"]).is_ok());
        assert!(
            Cli::try_parse_from([
                "firecrab",
                "network",
                "create",
                "--name",
                "lab",
                "--subnet-cidr",
                "172.31.0.0/24"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "firecrab",
                "network",
                "delete",
                "11111111-1111-4111-8111-111111111111"
            ])
            .is_ok()
        );
    }

    #[test]
    fn cli_parses_image_list() {
        assert!(matches!(
            Cli::try_parse_from(["firecrab", "image", "list", "--json"])
                .unwrap()
                .command,
            Command::Image {
                command: image::Command::List { json: true }
            }
        ));
    }

    #[test]
    fn cli_rejects_missing_create_arguments_and_bad_uuid() {
        assert!(Cli::try_parse_from(["firecrab", "vm", "create"]).is_err());
        assert!(Cli::try_parse_from(["firecrab", "network", "create"]).is_err());
        assert!(Cli::try_parse_from(["firecrab", "vm", "start", "not-a-uuid"]).is_err());
    }

    #[test]
    fn api_command_failure_returns_non_zero() {
        let code =
            run(
                Cli::try_parse_from(["firecrab", "--api", "http://127.0.0.1:1", "image", "list"])
                    .unwrap(),
            );
        assert_eq!(code, 1);
    }

    #[test]
    fn cli_parses_service_commands() {
        let install = Cli::try_parse_from([
            "firecrab",
            "service",
            "install",
            "--version",
            "v0.3.0",
            "--libc",
            "musl",
            "--bin-dir",
            "target/release",
            "--dashboard-dir",
            "firecrab-frontend/dist",
            "--no-deps",
            "--no-frontend",
            "--check",
        ])
        .unwrap();
        match install.command {
            Command::Service {
                command: service::Command::Install(opts),
            } => {
                assert_eq!(opts.version.as_deref(), Some("v0.3.0"));
                assert_eq!(opts.libc.as_deref(), Some("musl"));
                assert_eq!(
                    opts.bin_dir.as_deref(),
                    Some(std::path::Path::new("target/release"))
                );
                assert_eq!(
                    opts.dashboard_dir.as_deref(),
                    Some(std::path::Path::new("firecrab-frontend/dist"))
                );
                assert!(opts.no_deps && opts.no_frontend && opts.check);
            }
            _ => panic!("expected service install"),
        }

        let uninstall =
            Cli::try_parse_from(["firecrab", "service", "uninstall", "--purge"]).unwrap();
        assert!(matches!(
            uninstall.command,
            Command::Service {
                command: service::Command::Uninstall { purge: true }
            }
        ));

        let reinstall =
            Cli::try_parse_from(["firecrab", "service", "reinstall", "--purge"]).unwrap();
        assert!(matches!(
            reinstall.command,
            Command::Service {
                command: service::Command::Reinstall { purge: true, .. }
            }
        ));

        for verb in ["start", "stop", "restart", "enable", "disable"] {
            assert!(
                Cli::try_parse_from(["firecrab", "service", verb]).is_ok(),
                "failed to parse service {verb}"
            );
        }
        assert!(
            Cli::try_parse_from(["firecrab", "service", "install", "--dashboard-dir", "x"])
                .is_err(),
            "--dashboard-dir requires --bin-dir"
        );
        assert!(Cli::try_parse_from(["firecrab", "service", "bogus"]).is_err());
    }
}

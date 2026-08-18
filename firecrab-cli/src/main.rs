use clap::{Parser, Subcommand};

mod api_client;
mod doctor;
mod info;
mod shell;
mod status;

/// `clap`-derived top-level CLI, replacing `scripts/firecrab-doctor.sh`.
#[derive(Parser)]
#[command(name = "firecrab", version, about = "firecrab host CLI")]
struct Cli {
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
        /// Override the API base URL (else FIRECRAB_API, else http://127.0.0.1:5523).
        #[arg(long)]
        api: Option<String>,
    },
    /// Show systemd unit status and the API host status.
    Status {
        /// Emit the [`status::StatusReport`] as JSON instead of the human format.
        #[arg(long)]
        json: bool,
        /// Override the API base URL (else FIRECRAB_API, else http://127.0.0.1:5523).
        #[arg(long)]
        api: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Doctor { digest, json } => std::process::exit(run_doctor(digest, json)),
        Command::Info { json, api } => run_info(json, api),
        Command::Status { json, api } => run_status(json, api),
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
        match cli.command {
            Command::Info { json, api } => {
                assert!(!json);
                assert_eq!(api.as_deref(), Some("http://x:1"));
            }
            _ => panic!("expected Info"),
        }
    }

    #[test]
    fn cli_parses_status_subcommand() {
        let cli = Cli::try_parse_from(["firecrab", "status"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Status {
                json: false,
                api: None
            }
        ));
    }

    #[test]
    fn cli_rejects_unknown_subcommand() {
        assert!(Cli::try_parse_from(["firecrab", "bogus"]).is_err());
    }
}

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
        Command::Doctor { digest, json } => {
            let env = doctor::DoctorEnv::from_process_env();
            let runner = shell::RealCommandRunner;
            let report = doctor::run_all(&env, &runner, digest);
            if json {
                println!("{}", serde_json::to_string_pretty(&report).unwrap());
            } else {
                doctor::print_human(&report);
            }
            std::process::exit(report.exit_code());
        }
        Command::Info { json, api } => {
            let api_base = api_client::resolve_api_base(api.as_deref());
            let report = info::collect(&api_base);
            if json {
                info::print_json(&report);
            } else {
                info::print_human(&report);
            }
        }
        Command::Status { json, api } => {
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
    }
}

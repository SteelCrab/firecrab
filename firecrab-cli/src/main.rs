use clap::{Parser, Subcommand};

mod doctor;
mod shell;

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
        #[arg(long)]
        json: bool,
    },
    /// Show version and resolved host configuration paths.
    Info {
        #[arg(long)]
        json: bool,
        /// Override the API base URL (else FIRECRAB_API, else http://127.0.0.1:5523).
        #[arg(long)]
        api: Option<String>,
    },
    /// Show systemd unit status and the API host status.
    Status {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        api: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Doctor { .. } => {
            println!("doctor: not yet implemented");
        }
        Command::Info { .. } => {
            println!("info: not yet implemented");
        }
        Command::Status { .. } => {
            println!("status: not yet implemented");
        }
    }
}

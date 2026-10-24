//! Offline bootstrap for Firecrab's private certificate authority.

use clap::Subcommand;
use firecrab_pki::Error;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a private root CA; refuse to overwrite an existing directory.
    Init {
        /// PKI directory (default: ~/.firecrab/pki).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Issue a host certificate with UUID identity and optional DNS/IP names.
    IssueHostCert {
        /// Non-nil host identity, also used for a controller's client certificate.
        host_id: Uuid,
        /// Existing PKI directory (default: ~/.firecrab/pki).
        #[arg(long)]
        dir: Option<PathBuf>,
        /// DNS name or IP used to connect to this host; can be repeated.
        #[arg(long)]
        san: Vec<String>,
        /// Validity in days, bounded by the root CA's expiration.
        #[arg(long, default_value_t = 365, value_parser = clap::value_parser!(u32).range(1..=3650))]
        days: u32,
    },
}

fn directory(configured: Option<PathBuf>) -> Result<PathBuf, Error> {
    if let Some(path) = configured {
        return Ok(path);
    }
    let config = crate::hosts::config_path().map_err(|e| Error(e.to_string()))?;
    Ok(config.with_file_name("pki"))
}

pub fn run(command: Command) -> Result<(), Error> {
    match command {
        Command::Init { dir } => {
            let cert = firecrab_pki::init_ca(&directory(dir)?)?;
            println!("CA certificate: {}", cert.display());
        }
        Command::IssueHostCert {
            host_id,
            dir,
            san,
            days,
        } => {
            let paths = firecrab_pki::issue_host_cert(&directory(dir)?, host_id, san, days)?;
            println!(
                "Host ID: {host_id}\nCertificate: {}\nPrivate key: {}\nCA certificate: {}",
                paths.cert.display(),
                paths.key.display(),
                paths.ca.display()
            );
        }
    }
    Ok(())
}

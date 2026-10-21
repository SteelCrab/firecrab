//! `firecrab service` — install.sh installation, removal, and systemd control in Rust.
//!
//! Installation and reinstallation reuse install.sh from the release bundle.
//! Service control and removal run through [`privileged::Privileged`].
#![allow(dead_code)]

use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::shell::CommandRunner;

pub mod deps;
pub mod env;
pub mod firecracker;
pub mod host_state;
mod installer;
pub mod layout;
pub mod output;
pub mod payload;
pub mod pkg;
pub mod privileged;
pub mod selinux;
pub mod uninstall;
pub mod units;

/// `firecrab service <sub>`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Install the host services (install.sh equivalent).
    Install(InstallOpts),
    /// Stop and remove units, binaries and the dashboard.
    Uninstall {
        /// Also delete $DATADIR and $CONFDIR (VM disks and the database!).
        #[arg(long)]
        purge: bool,
    },
    /// Uninstall (keeping data unless --purge) and install again.
    Reinstall {
        /// Also delete $DATADIR and $CONFDIR before installing.
        #[arg(long)]
        purge: bool,
        #[command(flatten)]
        opts: InstallOpts,
    },
    /// Start firecrab-helper then firecrab-api.
    Start,
    /// Stop firecrab-api then firecrab-helper.
    Stop,
    /// Stop then start both units.
    Restart,
    /// Enable both units at boot.
    Enable,
    /// Disable both units at boot.
    Disable,
}

/// Options shared by `install` and `reinstall`.
#[derive(Debug, Clone, Default, Args)]
pub struct InstallOpts {
    /// Release tag to install (default: latest).
    #[arg(long, value_name = "TAG")]
    pub version: Option<String>,
    /// Force the bundle libc: gnu, glibc or musl.
    #[arg(long, value_name = "LIBC")]
    pub libc: Option<String>,
    /// Install binaries from this directory instead of a release (needs a checkout).
    #[arg(long, value_name = "DIR")]
    pub bin_dir: Option<PathBuf>,
    /// Dashboard build to install alongside --bin-dir.
    #[arg(long, value_name = "DIR", requires = "bin_dir")]
    pub dashboard_dir: Option<PathBuf>,
    /// Do not install packages or firecracker; report what is missing.
    #[arg(long)]
    pub no_deps: bool,
    /// Skip the dashboard.
    #[arg(long)]
    pub no_frontend: bool,
    /// Report every step without changing the host.
    #[arg(long)]
    pub check: bool,
}

/// Everything `firecrab service` can fail at.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// One pipeline step failed; `fix` is the operator hint install.sh prints.
    #[error("{step}: {detail}")]
    Step {
        /// Pipeline step name (`deps`, `payload`, `units`, ...).
        step: &'static str,
        /// What went wrong.
        detail: String,
        /// How to fix it, if known.
        fix: Option<String>,
    },
    /// sudo is missing, needs a password without a tty, or refused.
    #[error("sudo: {0}")]
    Sudo(String),
    /// A unit file is not in $UNITDIR.
    #[error("firecrab is not installed — run `firecrab service install`")]
    NotInstalled,
    /// Release download / checksum failure (reuses `firecrab update`'s errors).
    #[error(transparent)]
    Bundle(#[from] crate::update::UpdateError),
}

impl Error {
    /// Shorthand for [`Error::Step`] without a fix hint.
    pub fn step(step: &'static str, detail: impl Into<String>) -> Self {
        Self::Step {
            step,
            detail: detail.into(),
            fix: None,
        }
    }

    /// Shorthand for [`Error::Step`] with a fix hint.
    pub fn step_fix(step: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self::Step {
            step,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    /// Renders the `xx step: detail` + `→ fix` lines the way install.sh's `die` does.
    pub fn report(&self) {
        output::fail(&self.to_string());
        if let Self::Step { fix: Some(fix), .. } = self {
            eprintln!("  → {fix}");
        }
    }
}

/// Executes one `firecrab service` command.
pub fn run(runner: &dyn CommandRunner, command: Command) -> Result<(), Error> {
    let env = env::ServiceEnv::from_process_env();
    let privileged = privileged::Privileged::detect(runner);
    match command {
        Command::Start | Command::Stop | Command::Restart | Command::Enable | Command::Disable => {
            units::require_installed(&env)?;
            privileged.ensure_ticket()?;
            let bind = env::ServiceEnv::api_bind();
            let probe = move || units::http_probe(&bind);
            match command {
                Command::Start => units::start(&privileged, &probe),
                Command::Stop => units::stop(&privileged),
                Command::Restart => {
                    units::stop(&privileged)?;
                    units::start(&privileged, &probe)
                }
                Command::Enable => units::enable(&privileged),
                Command::Disable => units::disable(&privileged),
                _ => unreachable!("guarded by the outer match"),
            }
        }
        Command::Uninstall { purge } => {
            privileged.ensure_ticket()?;
            uninstall::uninstall(&privileged, &env, purge)
        }
        Command::Install(opts) => installer::run(opts, false, false),
        Command::Reinstall { opts, purge } => installer::run(opts, true, purge),
    }
}

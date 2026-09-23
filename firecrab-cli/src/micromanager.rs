//! microManager — the host-side launcher for the managed Debian guest.
//!
//! Each host has its own backend module; everything they share lives beside them.

mod artifact;
#[cfg(target_os = "macos")]
mod macos;
mod managed_home;
// Everything but the `wsl.exe` and `schtasks.exe` calls is plain Rust, so the
// Windows backend also builds and runs its tests on the Linux CI runner.
#[cfg(any(target_os = "windows", test))]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod windows;

use clap::Subcommand;

#[cfg(target_os = "macos")]
pub use macos::run;
#[cfg(target_os = "windows")]
pub use windows::run;

/// `firecrab service` on every host that runs Firecrab inside a managed Debian
/// VM. One definition keeps the verbs identical; each backend decides how.
#[derive(Subcommand)]
pub enum Command {
    /// Install microManager and provision the managed Debian VM.
    Install,
    /// Reinstall microManager while preserving managed VM data.
    Reinstall,
    /// Remove microManager and optionally all managed VM data.
    Uninstall {
        /// Also delete the managed Debian VM and its persistent data.
        #[arg(long)]
        purge: bool,
    },
    /// Start the resident management VM and wait for the local API.
    Start,
    /// Stop the resident management VM.
    Stop,
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
    /// Run the managed Debian VM in the foreground with its console attached.
    Run,
}

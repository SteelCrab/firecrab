//! microManager — the host-side launcher for the managed Debian guest.
//!
//! Each host has its own backend module; everything they share lives beside them.

mod artifact;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
pub use macos::{Command, run};
#[cfg(target_os = "windows")]
pub use windows::{Command, run};

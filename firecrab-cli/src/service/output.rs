//! Print progress messages using the same prefixes as install.sh.
//!
//! Task 1 uses only `fail` through [`super::Error::report`]. Later tasks use
//! the remaining functions to implement installation, removal, and systemd control.
#![allow(dead_code)]

/// `==> msg` — completed step (stdout).
pub fn log(msg: &str) {
    println!("\x1b[1;32m==>\x1b[0m {msg}");
}

/// `--  msg` — starting operation (stdout).
pub fn step(msg: &str) {
    println!("\x1b[1;36m--\x1b[0m  {msg}");
}

/// `!!  msg` — warning that allows execution to continue (stderr).
pub fn warn(msg: &str) {
    eprintln!("\x1b[1;33m!!\x1b[0m  {msg}");
}

/// `xx  msg` — failure (stderr). The caller decides whether to exit.
pub fn fail(msg: &str) {
    eprintln!("\x1b[1;31mxx\x1b[0m  {msg}");
}

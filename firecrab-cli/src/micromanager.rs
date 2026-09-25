//! microManager — the host-side launcher for the managed Debian guest.
//!
//! Each host has its own backend module; everything they share lives beside them.

mod artifact;
mod host_platform;
#[cfg(target_os = "macos")]
mod macos;
mod managed_home;
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
#[cfg(target_os = "windows")]
pub use windows::run;

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
/// VM. One definition keeps the verbs identical; each backend decides how.
#[derive(Subcommand)]
pub enum Command {
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
}

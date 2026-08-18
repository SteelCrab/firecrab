use std::collections::HashMap;
use std::io;
use std::process::Output;

/// Abstracts spawning an external command so doctor checks can be unit
/// tested against canned output instead of the real host.
pub trait CommandRunner {
    /// Same contract as `std::process::Command::output`: `Err` means the
    /// command could not even be spawned (e.g. not on `$PATH`), a
    /// nonzero exit is still `Ok`.
    fn run(&self, cmd: &str, args: &[&str]) -> io::Result<Output>;
}

/// Shells out via `std::process::Command`. Used by every subcommand at
/// runtime.
pub struct RealCommandRunner;

impl CommandRunner for RealCommandRunner {
    fn run(&self, cmd: &str, args: &[&str]) -> io::Result<Output> {
        std::process::Command::new(cmd).args(args).output()
    }
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct FakeCommandRunner {
    responses: HashMap<String, (i32, String, String)>,
}

#[cfg(test)]
impl FakeCommandRunner {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn key(cmd: &str, args: &[&str]) -> String {
        let mut k = cmd.to_owned();
        for a in args {
            k.push(' ');
            k.push_str(a);
        }
        k
    }

    /// Registers the output `run(cmd, args)` should return. Unregistered
    /// invocations return an `ErrorKind::NotFound` error, matching what a
    /// missing binary on `$PATH` looks like to `std::process::Command`.
    pub(crate) fn set(
        &mut self,
        cmd: &str,
        args: &[&str],
        exit_code: i32,
        stdout: &str,
        stderr: &str,
    ) {
        self.responses.insert(
            Self::key(cmd, args),
            (exit_code, stdout.to_owned(), stderr.to_owned()),
        );
    }
}

#[cfg(test)]
impl CommandRunner for FakeCommandRunner {
    fn run(&self, cmd: &str, args: &[&str]) -> io::Result<Output> {
        use std::os::unix::process::ExitStatusExt;
        let key = Self::key(cmd, args);
        match self.responses.get(&key) {
            Some((code, stdout, stderr)) => Ok(Output {
                status: std::process::ExitStatus::from_raw(code << 8),
                stdout: stdout.clone().into_bytes(),
                stderr: stderr.clone().into_bytes(),
            }),
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no fake response for: {key}"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_runner_returns_configured_output() {
        let mut fake = FakeCommandRunner::new();
        fake.set("nft", &["list", "tables"], 0, "table inet firecrab\n", "");
        let out = fake.run("nft", &["list", "tables"]).unwrap();
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "table inet firecrab\n"
        );
    }

    #[test]
    fn fake_runner_errors_on_unconfigured_command() {
        let fake = FakeCommandRunner::new();
        let err = fake.run("nft", &["list", "tables"]).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn fake_runner_encodes_nonzero_exit_code() {
        let mut fake = FakeCommandRunner::new();
        fake.set("ufw", &["status"], 1, "", "Permission denied\n");
        let out = fake.run("ufw", &["status"]).unwrap();
        assert!(!out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stderr), "Permission denied\n");
    }
}

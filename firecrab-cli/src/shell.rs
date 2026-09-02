use std::io;
use std::process::Output;

/// Abstracts spawning an external command so doctor checks can be unit
/// tested against canned output instead of the real host.
pub trait CommandRunner {
    /// Same contract as `std::process::Command::output`: `Err` means the
    /// command could not even be spawned (e.g. not on `$PATH`), a
    /// nonzero exit is still `Ok`.
    fn run(&self, cmd: &str, args: &[&str]) -> io::Result<Output>;

    /// Like [`CommandRunner::run`], but writes `input` to the child's stdin
    /// first (`sudo tee <path>` is how the service installer writes root-owned files).
    // Not called from non-test code until the `firecrab service` installer (a later
    // task) writes unit files through it.
    #[allow(dead_code)]
    fn run_with_stdin(&self, cmd: &str, args: &[&str], input: &[u8]) -> io::Result<Output>;
}

/// Shells out via `std::process::Command`. Used by every subcommand at
/// runtime.
pub struct RealCommandRunner;

impl CommandRunner for RealCommandRunner {
    fn run(&self, cmd: &str, args: &[&str]) -> io::Result<Output> {
        std::process::Command::new(cmd).args(args).output()
    }

    fn run_with_stdin(&self, cmd: &str, args: &[&str], input: &[u8]) -> io::Result<Output> {
        use std::io::Write;
        use std::process::Stdio;
        let mut child = std::process::Command::new(cmd)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        // Write stdin from a separate thread: `input` can exceed the pipe
        // buffer, and a child that writes to stdout/stderr before reading all
        // of stdin would otherwise deadlock the parent (blocked on
        // `write_all`) against the child (blocked on a full stdout pipe).
        // `wait_with_output` below drains stdout/stderr concurrently with
        // this write.
        if let Some(mut stdin) = child.stdin.take() {
            let input = input.to_vec();
            std::thread::spawn(move || {
                let _ = stdin.write_all(&input);
            });
        }
        child.wait_with_output()
    }
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct FakeCommandRunner {
    // A key can carry more than one response: successive calls consume the
    // sequence in order, then repeat the last entry. `set` stores a
    // one-element sequence, so it behaves exactly as before.
    responses: std::collections::HashMap<String, Vec<(i32, String, String)>>,
    // How many responses have already been consumed for each key, so a
    // repeated call advances through a sequence registered via `set_seq`.
    cursors: std::cell::RefCell<std::collections::HashMap<String, usize>>,
    permissive: bool,
    calls: std::cell::RefCell<Vec<String>>,
    stdin: std::cell::RefCell<std::collections::HashMap<String, Vec<u8>>>,
}

#[cfg(test)]
impl FakeCommandRunner {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Unregistered invocations succeed with empty output — for flows that
    /// issue dozens of `sudo install`/`systemctl` calls whose exact output is irrelevant.
    pub(crate) fn permissive() -> Self {
        Self {
            permissive: true,
            ..Self::default()
        }
    }

    /// Every invocation so far, as `"cmd arg1 arg2"`, in call order.
    pub(crate) fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }

    /// Bytes written to the stdin of the given `"cmd args"` line, if any.
    pub(crate) fn stdin_of(&self, cmd_line: &str) -> Option<Vec<u8>> {
        self.stdin.borrow().get(cmd_line).cloned()
    }

    fn key(cmd: &str, args: &[&str]) -> String {
        let mut k = cmd.to_owned();
        for a in args {
            k.push(' ');
            k.push_str(a);
        }
        k
    }

    /// Registers the output `run(cmd, args)` should return, every time it is
    /// called. Unregistered invocations return an `ErrorKind::NotFound`
    /// error, matching what a missing binary on `$PATH` looks like to
    /// `std::process::Command`.
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
            vec![(exit_code, stdout.to_owned(), stderr.to_owned())],
        );
    }

    /// Registers a sequence of outputs for `run(cmd, args)`: the first call
    /// gets `responses[0]`, the second `responses[1]`, and so on; once the
    /// sequence is exhausted, every further call repeats the last entry.
    /// Use this to prove a caller re-probes after a state-changing command
    /// (e.g. `have` returning absent, then present after an install).
    pub(crate) fn set_seq(&mut self, cmd: &str, args: &[&str], responses: &[(i32, &str, &str)]) {
        self.responses.insert(
            Self::key(cmd, args),
            responses
                .iter()
                .map(|(code, stdout, stderr)| (*code, (*stdout).to_owned(), (*stderr).to_owned()))
                .collect(),
        );
    }

    fn respond(&self, key: String) -> io::Result<Output> {
        use std::os::unix::process::ExitStatusExt;
        self.calls.borrow_mut().push(key.clone());
        match self.responses.get(&key) {
            Some(sequence) => {
                let mut cursors = self.cursors.borrow_mut();
                let cursor = cursors.entry(key).or_insert(0);
                let index = (*cursor).min(sequence.len() - 1);
                if *cursor < sequence.len() - 1 {
                    *cursor += 1;
                }
                let (code, stdout, stderr) = &sequence[index];
                Ok(Output {
                    status: std::process::ExitStatus::from_raw(code << 8),
                    stdout: stdout.clone().into_bytes(),
                    stderr: stderr.clone().into_bytes(),
                })
            }
            None if self.permissive => Ok(Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            }),
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no fake response for: {key}"),
            )),
        }
    }
}

#[cfg(test)]
impl CommandRunner for FakeCommandRunner {
    fn run(&self, cmd: &str, args: &[&str]) -> io::Result<Output> {
        self.respond(Self::key(cmd, args))
    }

    fn run_with_stdin(&self, cmd: &str, args: &[&str], input: &[u8]) -> io::Result<Output> {
        let key = Self::key(cmd, args);
        self.stdin.borrow_mut().insert(key.clone(), input.to_vec());
        self.respond(key)
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

    #[test]
    fn fake_runner_records_calls_in_order_and_stdin() {
        let fake = FakeCommandRunner::permissive();
        fake.run("sudo", &["systemctl", "daemon-reload"]).unwrap();
        fake.run_with_stdin("sudo", &["tee", "/etc/x"], b"hello")
            .unwrap();
        assert_eq!(
            fake.calls(),
            vec!["sudo systemctl daemon-reload", "sudo tee /etc/x"]
        );
        assert_eq!(fake.stdin_of("sudo tee /etc/x"), Some(b"hello".to_vec()));
    }

    #[test]
    fn permissive_fake_returns_success_for_unregistered_commands() {
        let fake = FakeCommandRunner::permissive();
        assert!(fake.run("anything", &[]).unwrap().status.success());
    }

    #[test]
    fn strict_fake_still_errors_on_unregistered_commands() {
        let fake = FakeCommandRunner::new();
        assert_eq!(
            fake.run("anything", &[]).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn set_seq_advances_through_responses_then_repeats_the_last() {
        let mut fake = FakeCommandRunner::new();
        fake.set_seq(
            "sh",
            &["-c", "command -v nft"],
            &[(1, "", ""), (0, "/usr/sbin/nft\n", "")],
        );
        let first = fake.run("sh", &["-c", "command -v nft"]).unwrap();
        assert!(!first.status.success());
        let second = fake.run("sh", &["-c", "command -v nft"]).unwrap();
        assert!(second.status.success());
        let third = fake.run("sh", &["-c", "command -v nft"]).unwrap();
        assert!(
            third.status.success(),
            "repeats the last entry once exhausted"
        );
    }

    #[test]
    fn real_runner_pipes_stdin() {
        let out = RealCommandRunner
            .run_with_stdin("cat", &[], b"piped")
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "piped");
    }
}

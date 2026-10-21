//! install.sh의 `$SUDO` 접두: root면 빈 문자열, 아니면 `sudo`.
//!
//! 전체 프로세스를 root로 올리지 않고 변경 명령마다 `sudo`를 붙이는 이유는
//! install.sh와 같다 — `$HOME`, SSH 키, 다운로드 캐시가 호출자의 것으로 남는다.

use std::io;
use std::path::Path;
use std::process::Output;

use crate::shell::CommandRunner;

use super::Error;

/// Runs host-mutating commands with a `sudo` prefix when not already root.
pub struct Privileged<'a> {
    runner: &'a dyn CommandRunner,
    use_sudo: bool,
}

impl<'a> Privileged<'a> {
    /// `sudo` unless the effective uid is 0.
    pub fn detect(runner: &'a dyn CommandRunner) -> Self {
        Self::with_sudo(runner, !nix::unistd::geteuid().is_root())
    }

    /// Explicit form for tests.
    pub fn with_sudo(runner: &'a dyn CommandRunner, use_sudo: bool) -> Self {
        Self { runner, use_sudo }
    }

    /// The underlying runner, for read-only probes that must not go through sudo.
    pub fn runner(&self) -> &'a dyn CommandRunner {
        self.runner
    }

    /// Whether commands are prefixed with `sudo`.
    pub fn uses_sudo(&self) -> bool {
        self.use_sudo
    }

    fn argv<'b>(&self, cmd: &'b str, args: &[&'b str]) -> (&'b str, Vec<&'b str>) {
        if self.use_sudo {
            let mut full = Vec::with_capacity(args.len() + 1);
            full.push(cmd);
            full.extend_from_slice(args);
            ("sudo", full)
        } else {
            (cmd, args.to_vec())
        }
    }

    /// install.sh `require_sudo_ticket`: `sudo -n true`, else one interactive `sudo -v`.
    ///
    /// Whether a controlling terminal is available for that interactive
    /// prompt is decided by the caller via `has_tty` (see [`Self::ensure_ticket`]
    /// for the real check), so this can be exercised deterministically in tests.
    pub fn ensure_ticket_with(&self, has_tty: bool) -> Result<(), Error> {
        if !self.use_sudo {
            return Ok(());
        }
        match self.runner.run("sudo", &["-n", "true"]) {
            Ok(out) if out.status.success() => return Ok(()),
            Ok(_) => {}
            Err(_) => {
                return Err(Error::Sudo(
                    "sudo is required for privileged steps; install it first".into(),
                ));
            }
        }
        if !has_tty {
            return Err(Error::Sudo(
                "needs a password and this session has no terminal".into(),
            ));
        }
        super::output::step("sudo password required");
        // `sudo -v` talks to the controlling tty itself; stdout/stderr capture does not interfere.
        match std::process::Command::new("sudo").arg("-v").status() {
            Ok(status) if status.success() => Ok(()),
            _ => Err(Error::Sudo("authentication failed".into())),
        }
    }

    /// [`Self::ensure_ticket_with`], deciding tty availability from `/dev/tty`.
    pub fn ensure_ticket(&self) -> Result<(), Error> {
        self.ensure_ticket_with(Path::new("/dev/tty").exists())
    }

    /// `[sudo] cmd args…`, raw `Output`.
    pub fn run(&self, cmd: &str, args: &[&str]) -> io::Result<Output> {
        let (cmd, args) = self.argv(cmd, args);
        self.runner.run(cmd, &args)
    }

    /// [`Self::run`] that turns a spawn failure or a nonzero exit into [`Error::Step`].
    pub fn run_ok(&self, step: &'static str, cmd: &str, args: &[&str]) -> Result<Output, Error> {
        let line = format!("{cmd} {}", args.join(" "));
        let out = self
            .run(cmd, args)
            .map_err(|e| Error::step(step, format!("{line}: {e}")))?;
        if out.status.success() {
            Ok(out)
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            Err(Error::step(
                step,
                format!("{line} failed ({}): {stderr}", out.status),
            ))
        }
    }

    /// `[sudo] env K=V… cmd args…`.
    pub fn run_env(
        &self,
        step: &'static str,
        envs: &[(&str, &str)],
        cmd: &str,
        args: &[&str],
    ) -> Result<Output, Error> {
        let assignments: Vec<String> = envs.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let mut full: Vec<&str> = assignments.iter().map(String::as_str).collect();
        full.push(cmd);
        full.extend_from_slice(args);
        self.run_ok(step, "env", &full)
    }

    /// `[sudo] tee path` with `contents` on stdin, then `[sudo] chmod mode path`.
    pub fn write_file(
        &self,
        step: &'static str,
        path: &Path,
        contents: &[u8],
        mode: &str,
    ) -> Result<(), Error> {
        let path_s = path.to_string_lossy();
        let (cmd, args) = self.argv("tee", &[&path_s]);
        let out = self
            .runner
            .run_with_stdin(cmd, &args, contents)
            .map_err(|e| Error::step(step, format!("tee {path_s}: {e}")))?;
        if !out.status.success() {
            return Err(Error::step(
                step,
                format!(
                    "tee {path_s} failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            ));
        }
        self.run_ok(step, "chmod", &[mode, &path_s]).map(|_| ())
    }

    /// `[sudo] install -o owner -g group -m mode src dest`.
    pub fn install_file(
        &self,
        step: &'static str,
        src: &Path,
        dest: &Path,
        owner: &str,
        group: &str,
        mode: &str,
    ) -> Result<(), Error> {
        let (src, dest) = (src.to_string_lossy(), dest.to_string_lossy());
        self.run_ok(
            step,
            "install",
            &["-o", owner, "-g", group, "-m", mode, &src, &dest],
        )
        .map(|_| ())
    }

    /// `[sudo] install -d -o owner -g group -m mode dirs…`.
    pub fn install_dirs(
        &self,
        step: &'static str,
        dirs: &[&Path],
        owner: &str,
        group: &str,
        mode: &str,
    ) -> Result<(), Error> {
        let dirs: Vec<String> = dirs
            .iter()
            .map(|d| d.to_string_lossy().into_owned())
            .collect();
        let mut args = vec!["-d", "-o", owner, "-g", group, "-m", mode];
        args.extend(dirs.iter().map(String::as_str));
        self.run_ok(step, "install", &args).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::FakeCommandRunner;
    use std::path::Path;

    #[test]
    fn sudo_prefix_only_when_not_root() {
        let fake = FakeCommandRunner::permissive();
        Privileged::with_sudo(&fake, true)
            .run("systemctl", &["daemon-reload"])
            .unwrap();
        Privileged::with_sudo(&fake, false)
            .run("systemctl", &["daemon-reload"])
            .unwrap();
        assert_eq!(
            fake.calls(),
            vec!["sudo systemctl daemon-reload", "systemctl daemon-reload"]
        );
    }

    #[test]
    fn run_ok_maps_nonzero_exit_to_step_error() {
        let mut fake = FakeCommandRunner::new();
        fake.set("sudo", &["useradd", "x"], 9, "", "useradd: UID in use\n");
        let err = Privileged::with_sudo(&fake, true)
            .run_ok("account", "useradd", &["x"])
            .unwrap_err();
        match err {
            Error::Step { step, detail, .. } => {
                assert_eq!(step, "account");
                assert!(
                    detail.contains("useradd") && detail.contains("UID in use"),
                    "{detail}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn run_ok_maps_spawn_failure_to_step_error() {
        let fake = FakeCommandRunner::new(); // strict: nothing registered → NotFound
        let err = Privileged::with_sudo(&fake, false)
            .run_ok("units", "systemctl", &["x"])
            .unwrap_err();
        assert!(matches!(err, Error::Step { step: "units", .. }));
    }

    #[test]
    fn write_file_uses_tee_then_chmod() {
        let fake = FakeCommandRunner::permissive();
        let p = Privileged::with_sudo(&fake, true);
        p.write_file(
            "units",
            Path::new("/etc/systemd/system/a.service"),
            b"[Unit]\n",
            "0644",
        )
        .unwrap();
        assert_eq!(
            fake.calls(),
            vec![
                "sudo tee /etc/systemd/system/a.service",
                "sudo chmod 0644 /etc/systemd/system/a.service",
            ]
        );
        assert_eq!(
            fake.stdin_of("sudo tee /etc/systemd/system/a.service"),
            Some(b"[Unit]\n".to_vec())
        );
    }

    #[test]
    fn install_helpers_build_install_argv() {
        let fake = FakeCommandRunner::permissive();
        let p = Privileged::with_sudo(&fake, false);
        p.install_file(
            "binaries",
            Path::new("/tmp/a"),
            Path::new("/usr/local/lib/firecrab/a"),
            "root",
            "root",
            "0755",
        )
        .unwrap();
        p.install_dirs(
            "directories",
            &[
                Path::new("/var/lib/firecrab"),
                Path::new("/var/lib/firecrab/data"),
            ],
            "firecrab",
            "firecrab",
            "0750",
        )
        .unwrap();
        assert_eq!(
            fake.calls(),
            vec![
                "install -o root -g root -m 0755 /tmp/a /usr/local/lib/firecrab/a",
                "install -d -o firecrab -g firecrab -m 0750 /var/lib/firecrab /var/lib/firecrab/data",
            ]
        );
    }

    #[test]
    fn run_env_prefixes_env_assignments() {
        let fake = FakeCommandRunner::permissive();
        Privileged::with_sudo(&fake, true)
            .run_env(
                "firecracker",
                &[(
                    "FIRECRACKER_NOTICE_DIR",
                    "/usr/local/share/firecrab/firecracker",
                )],
                "bash",
                &["/tmp/i.sh"],
            )
            .unwrap();
        assert_eq!(
            fake.calls(),
            vec![
                "sudo env FIRECRACKER_NOTICE_DIR=/usr/local/share/firecrab/firecracker bash /tmp/i.sh"
            ]
        );
    }

    #[test]
    fn ensure_ticket_is_a_noop_as_root_and_probes_sudo_otherwise() {
        let fake = FakeCommandRunner::permissive();
        Privileged::with_sudo(&fake, false)
            .ensure_ticket_with(true)
            .unwrap();
        assert!(fake.calls().is_empty());
        Privileged::with_sudo(&fake, true)
            .ensure_ticket_with(true)
            .unwrap();
        assert_eq!(fake.calls(), vec!["sudo -n true"]);
    }

    #[test]
    fn ensure_ticket_fails_without_a_tty_when_sudo_needs_a_password() {
        let mut fake = FakeCommandRunner::new();
        fake.set(
            "sudo",
            &["-n", "true"],
            1,
            "",
            "sudo: a password is required\n",
        );
        // `sudo -v` is not registered → NotFound → treated as unusable.
        let err = Privileged::with_sudo(&fake, true)
            .ensure_ticket_with(false)
            .unwrap_err();
        assert!(matches!(err, Error::Sudo(_)));
    }
}

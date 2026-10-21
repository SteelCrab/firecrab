//! install.sh `selinux_active` / `label_selinux_binaries`.
//!
//! The default policy labels `$PREFIX/lib/firecrab` as `lib_t`. Rules that move
//! services out of `init_t` apply only to `bin_t` executables. Without that label,
//! the API stays in `init_t` and all outbound HTTPS requests to registries are denied.

use crate::shell::CommandRunner;

use super::env::ServiceEnv;
use super::output;
use super::pkg::have;
use super::privileged::Privileged;

/// Check whether SELinux is loaded. Include Permissive mode because incorrect
/// labels would break the installation as soon as Enforcing mode is restored.
pub fn selinux_active(runner: &dyn CommandRunner) -> bool {
    if !have(runner, "getenforce") {
        return false;
    }
    runner
        .run("getenforce", &[])
        .map(|out| {
            let mode = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            mode == "Enforcing" || mode == "Permissive"
        })
        .unwrap_or(false)
}

fn pattern(env: &ServiceEnv) -> String {
    format!("{}(/.*)?", env.libdir.display())
}

/// Record and apply the `bin_t` file context to `$LIBDIR`. Report failures as warnings.
pub fn label_binaries(privileged: &Privileged<'_>, env: &ServiceEnv) {
    if !selinux_active(privileged.runner()) {
        return;
    }
    let pattern = pattern(env);
    if !have(privileged.runner(), "semanage") {
        output::warn(&format!(
            "semanage missing; cannot label {} bin_t — the services will stay in init_t and every registry read will fail (see `firecrab doctor`)",
            env.libdir.display()
        ));
        return;
    }
    // `-a` fails when the rule already exists, which every re-run hits.
    let added = privileged
        .run("semanage", &["fcontext", "-a", "-t", "bin_t", &pattern])
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !added {
        let modified = privileged
            .run("semanage", &["fcontext", "-m", "-t", "bin_t", &pattern])
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !modified {
            output::warn(&format!(
                "could not record an SELinux file context for {}",
                env.libdir.display()
            ));
            return;
        }
    }
    if have(privileged.runner(), "restorecon") {
        let libdir = env.libdir.to_string_lossy().into_owned();
        if privileged
            .run("restorecon", &["-R", &libdir])
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            output::warn(&format!("restorecon failed for {libdir}"));
        }
    }
    output::log(&format!("SELinux: {} labelled bin_t", env.libdir.display()));
}

/// Remove the file context rule on uninstall. If the rule outlives the directory,
/// anything later installed at that path would be silently relabeled.
pub fn unlabel_binaries(privileged: &Privileged<'_>, env: &ServiceEnv) {
    if !have(privileged.runner(), "semanage") {
        return;
    }
    let _ = privileged.run("semanage", &["fcontext", "-d", &pattern(env)]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::FakeCommandRunner;
    use std::path::Path;

    fn env_for(prefix: &Path) -> ServiceEnv {
        ServiceEnv::from_values(
            "firecrab",
            "firecrab",
            prefix,
            Path::new("/var/lib/firecrab"),
            Path::new("/etc/firecrab"),
            Path::new("/etc/systemd/system"),
        )
    }

    #[test]
    fn selinux_active_follows_getenforce() {
        let mut fake = FakeCommandRunner::new();
        fake.set(
            "sh",
            &["-c", "command -v getenforce"],
            0,
            "/usr/sbin/getenforce\n",
            "",
        );
        fake.set("getenforce", &[], 0, "Enforcing\n", "");
        assert!(selinux_active(&fake));

        let mut fake = FakeCommandRunner::new();
        fake.set(
            "sh",
            &["-c", "command -v getenforce"],
            0,
            "/usr/sbin/getenforce\n",
            "",
        );
        fake.set("getenforce", &[], 0, "Permissive\n", "");
        assert!(
            selinux_active(&fake),
            "permissive counts: labels still have to be right"
        );

        let mut fake = FakeCommandRunner::new();
        fake.set(
            "sh",
            &["-c", "command -v getenforce"],
            0,
            "/usr/sbin/getenforce\n",
            "",
        );
        fake.set("getenforce", &[], 0, "Disabled\n", "");
        assert!(!selinux_active(&fake));

        // getenforce absent entirely
        assert!(!selinux_active(&FakeCommandRunner::new()));
    }

    #[test]
    fn label_records_the_context_and_restores_it() {
        let mut fake = FakeCommandRunner::permissive();
        fake.set(
            "sh",
            &["-c", "command -v getenforce"],
            0,
            "/usr/sbin/getenforce\n",
            "",
        );
        fake.set("getenforce", &[], 0, "Enforcing\n", "");
        fake.set(
            "sh",
            &["-c", "command -v semanage"],
            0,
            "/usr/sbin/semanage\n",
            "",
        );
        fake.set(
            "sh",
            &["-c", "command -v restorecon"],
            0,
            "/usr/sbin/restorecon\n",
            "",
        );
        let env = env_for(Path::new("/usr/local"));
        label_binaries(&Privileged::with_sudo(&fake, true), &env);
        let calls = fake.calls();
        assert!(
            calls
                .iter()
                .any(|c| c == "sudo semanage fcontext -a -t bin_t /usr/local/lib/firecrab(/.*)?"),
            "{calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|c| c == "sudo restorecon -R /usr/local/lib/firecrab"),
            "{calls:?}"
        );
    }

    #[test]
    fn label_falls_back_to_modify_when_the_rule_exists() {
        let mut fake = FakeCommandRunner::permissive();
        fake.set(
            "sh",
            &["-c", "command -v getenforce"],
            0,
            "/usr/sbin/getenforce\n",
            "",
        );
        fake.set("getenforce", &[], 0, "Enforcing\n", "");
        fake.set(
            "sh",
            &["-c", "command -v semanage"],
            0,
            "/usr/sbin/semanage\n",
            "",
        );
        fake.set("sh", &["-c", "command -v restorecon"], 1, "", "");
        fake.set(
            "sudo",
            &[
                "semanage",
                "fcontext",
                "-a",
                "-t",
                "bin_t",
                "/usr/local/lib/firecrab(/.*)?",
            ],
            1,
            "",
            "already defined\n",
        );
        label_binaries(
            &Privileged::with_sudo(&fake, true),
            &env_for(Path::new("/usr/local")),
        );
        assert!(
            fake.calls()
                .iter()
                .any(|c| c.contains("semanage fcontext -m -t bin_t")),
            "{:?}",
            fake.calls()
        );
    }

    #[test]
    fn label_without_semanage_only_warns() {
        let mut fake2 = FakeCommandRunner::permissive();
        fake2.set(
            "sh",
            &["-c", "command -v getenforce"],
            0,
            "/usr/sbin/getenforce\n",
            "",
        );
        fake2.set("getenforce", &[], 0, "Enforcing\n", "");
        fake2.set("sh", &["-c", "command -v semanage"], 1, "", "");
        label_binaries(
            &Privileged::with_sudo(&fake2, true),
            &env_for(Path::new("/usr/local")),
        );
        // Positive: proves the run actually reached the `have(semanage)` check
        // (past the SELinux-active guard) rather than bailing out earlier for
        // an unrelated reason.
        assert!(
            fake2
                .calls()
                .iter()
                .any(|c| c == "sh -c command -v semanage"),
            "{:?}",
            fake2.calls()
        );
        assert!(
            !fake2
                .calls()
                .iter()
                .any(|c| c.contains("semanage fcontext")),
            "{:?}",
            fake2.calls()
        );
    }

    #[test]
    fn unlabel_deletes_the_rule_when_semanage_exists() {
        let mut fake = FakeCommandRunner::permissive();
        fake.set(
            "sh",
            &["-c", "command -v semanage"],
            0,
            "/usr/sbin/semanage\n",
            "",
        );
        unlabel_binaries(
            &Privileged::with_sudo(&fake, true),
            &env_for(Path::new("/usr/local")),
        );
        assert!(
            fake.calls()
                .iter()
                .any(|c| c == "sudo semanage fcontext -d /usr/local/lib/firecrab(/.*)?"),
            "{:?}",
            fake.calls()
        );
    }
}

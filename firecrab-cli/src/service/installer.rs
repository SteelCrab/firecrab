//! Use the same host installer for the CLI and curl entry points.

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use super::{Error, InstallOpts, payload};

fn bundled_script() -> String {
    let source = include_str!("../../../install.sh").replace("\r\n", "\n");
    let (before, rest) = source.split_once("# BEGIN RELEASE_HELPERS\n").unwrap();
    let (_, after) = rest.split_once("# END RELEASE_HELPERS\n").unwrap();
    format!(
        "{before}{}\n{after}",
        include_str!("../../../scripts/firecrab-release.sh").replace("\r\n", "\n")
    )
}

fn arguments(opts: &InstallOpts) -> Vec<OsString> {
    let mut args = Vec::new();
    for (flag, value) in [("--version", &opts.version), ("--libc", &opts.libc)] {
        if let Some(value) = value {
            args.extend([flag.into(), value.into()]);
        }
    }
    for (flag, value) in [
        ("--bin-dir", &opts.bin_dir),
        ("--dashboard-dir", &opts.dashboard_dir),
    ] {
        if let Some(value) = value {
            args.extend([flag.into(), value.as_os_str().to_owned()]);
        }
    }
    for (flag, enabled) in [
        ("--no-deps", opts.no_deps),
        ("--no-frontend", opts.no_frontend),
        ("--check", opts.check),
    ] {
        if enabled {
            args.push(flag.into());
        }
    }
    args
}

fn execute(script: &Path, args: &[OsString]) -> Result<(), Error> {
    // Inherit the terminal so package progress and sudo prompts stay visible.
    let status = Command::new("bash")
        .arg(script)
        .args(args)
        .status()
        .map_err(|error| Error::step("install", error.to_string()))?;
    if !status.success() {
        return Err(Error::step(
            "install",
            format!("host installer exited with {status}"),
        ));
    }
    Ok(())
}

pub(super) fn run(opts: InstallOpts, reinstall: bool, purge: bool) -> Result<(), Error> {
    let mut temp = tempfile::NamedTempFile::new()
        .map_err(|error| Error::step("install", error.to_string()))?;
    temp.write_all(bundled_script().as_bytes())
        .map_err(|error| Error::step("install", error.to_string()))?;
    let checkout = match &opts.bin_dir {
        Some(bin) => {
            let absolute = std::fs::canonicalize(bin)
                .map_err(|error| Error::step("payload", format!("{}: {error}", bin.display())))?;
            Some(
                payload::find_checkout(&absolute)
                    .ok_or_else(|| Error::step("payload", "--bin-dir needs a Firecrab checkout"))?,
            )
        }
        None => None,
    };
    let local_script = checkout.map(|root| root.join("install.sh"));
    let script = local_script.as_deref().unwrap_or(temp.path());
    if reinstall && !opts.check {
        let mut args = vec![OsString::from("--uninstall")];
        if purge {
            args.push("--purge".into());
        }
        execute(script, &args)?;
    }
    execute(script, &arguments(&opts))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkout(body: &str) -> tempfile::TempDir {
        let root = tempfile::Builder::new()
            .prefix("firecrab checkout ")
            .tempdir()
            .unwrap();
        std::fs::create_dir_all(root.path().join("packaging/systemd")).unwrap();
        std::fs::create_dir_all(root.path().join("target/release")).unwrap();
        std::fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(
            root.path().join("packaging/systemd/firecrab-api.service"),
            "[Service]\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("install.sh"),
            format!(
                r#"printf '%s\0' "$@" >> "$0.calls"
printf '\n' >> "$0.calls"
{body}
"#
            ),
        )
        .unwrap();
        root
    }

    fn options(root: &Path) -> InstallOpts {
        InstallOpts {
            bin_dir: Some(root.join("target/release")),
            ..InstallOpts::default()
        }
    }

    fn calls(root: &Path) -> Vec<Vec<String>> {
        std::fs::read_to_string(root.join("install.sh.calls"))
            .unwrap()
            .lines()
            .map(|line| line.split_terminator('\0').map(str::to_owned).collect())
            .collect()
    }

    #[test]
    fn install_forwards_options_as_literal_arguments_to_the_checkout() {
        let _guard = crate::update::ENV_LOCK.lock().unwrap();
        let root = checkout("exit 0");
        let bin = root.path().join("target/release");
        let dashboard = root.path().join("dashboard $(exit 99)");
        let opts = InstallOpts {
            version: Some("v0.3.1; exit 99".into()),
            libc: Some("musl".into()),
            bin_dir: Some(bin.clone()),
            dashboard_dir: Some(dashboard.clone()),
            no_deps: true,
            no_frontend: true,
            check: true,
        };
        let runner = crate::shell::FakeCommandRunner::new();
        crate::service::run(&runner, crate::service::Command::Install(opts)).unwrap();
        assert_eq!(
            calls(root.path()),
            vec![vec![
                "--version".to_owned(),
                "v0.3.1; exit 99".to_owned(),
                "--libc".to_owned(),
                "musl".to_owned(),
                "--bin-dir".to_owned(),
                bin.display().to_string(),
                "--dashboard-dir".to_owned(),
                dashboard.display().to_string(),
                "--no-deps".to_owned(),
                "--no-frontend".to_owned(),
                "--check".to_owned(),
            ]]
        );
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn reinstall_uninstalls_before_installing_and_only_purges_when_requested() {
        let _guard = crate::update::ENV_LOCK.lock().unwrap();
        for purge in [false, true] {
            let root = checkout("exit 0");
            let runner = crate::shell::FakeCommandRunner::new();
            crate::service::run(
                &runner,
                crate::service::Command::Reinstall {
                    opts: options(root.path()),
                    purge,
                },
            )
            .unwrap();
            let mut uninstall = vec!["--uninstall".to_owned()];
            if purge {
                uninstall.push("--purge".to_owned());
            }
            assert_eq!(
                calls(root.path()),
                vec![
                    uninstall,
                    vec![
                        "--bin-dir".to_owned(),
                        root.path().join("target/release").display().to_string(),
                    ],
                ]
            );
            assert!(runner.calls().is_empty());
        }
    }

    #[test]
    fn reinstall_check_never_uninstalls_even_with_purge() {
        let root = checkout("exit 0");
        let mut opts = options(root.path());
        opts.check = true;
        run(opts, true, true).unwrap();
        assert_eq!(
            calls(root.path()),
            vec![vec![
                "--bin-dir".to_owned(),
                root.path().join("target/release").display().to_string(),
                "--check".to_owned(),
            ]]
        );
    }

    #[test]
    fn failed_uninstall_stops_reinstall_before_the_installation() {
        let root = checkout("exit 23");
        let error = run(options(root.path()), true, true).unwrap_err();
        assert!(matches!(
            error,
            Error::Step {
                step: "install",
                ..
            }
        ));
        assert!(error.to_string().contains("23"), "{error}");
        assert_eq!(
            calls(root.path()),
            vec![vec!["--uninstall".to_owned(), "--purge".to_owned()]]
        );
    }

    #[test]
    fn failed_install_is_reported_after_a_successful_uninstall() {
        let root = checkout(
            r#"if [ "$1" = --uninstall ]; then exit 0; fi
exit 17"#,
        );
        let error = run(options(root.path()), true, false).unwrap_err();
        assert!(matches!(
            error,
            Error::Step {
                step: "install",
                ..
            }
        ));
        assert!(error.to_string().contains("17"), "{error}");
        assert_eq!(calls(root.path()).len(), 2);
    }

    #[test]
    fn invalid_local_payload_is_rejected_before_reinstall_can_uninstall() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("outside-checkout")).unwrap();
        for name in ["missing", "outside-checkout"] {
            let path = root.path().join(name);
            let error = run(
                InstallOpts {
                    bin_dir: Some(path),
                    ..InstallOpts::default()
                },
                true,
                true,
            )
            .unwrap_err();
            assert!(matches!(
                error,
                Error::Step {
                    step: "payload",
                    ..
                }
            ));
        }
    }

    #[test]
    fn embedded_installer_propagates_argument_rejection() {
        let error = run(
            InstallOpts {
                libc: Some("invalid-libc".into()),
                check: true,
                ..InstallOpts::default()
            },
            false,
            false,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            Error::Step {
                step: "install",
                ..
            }
        ));
        assert!(error.to_string().contains("host installer exited"));
    }

    #[test]
    fn embedded_installer_runs_without_a_checkout_or_network() {
        let mut script = tempfile::NamedTempFile::new().unwrap();
        script.write_all(bundled_script().as_bytes()).unwrap();
        let output = Command::new("bash")
            .arg(script.path())
            .arg("--help")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("ingress is bundled"));
    }
}

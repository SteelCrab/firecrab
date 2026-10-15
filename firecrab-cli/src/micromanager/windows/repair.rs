//! Restore the scheduled task and guest services without provisioning the WSL disk.

use std::path::PathBuf;
use std::time::Duration;

use super::super::{managed_home, report};
use super::{daemon, lifecycle::Layout, provision, wsl};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    ManagedHome(#[from] managed_home::Error),
    #[error("cannot repair microManager: {0} is missing; run `firecrab service reinstall`")]
    MissingFile(PathBuf),
    #[error(
        "cannot repair microManager: guest provisioning is incomplete; run `firecrab service reinstall`"
    )]
    IncompleteProvision,
    #[error("cannot repair microManager: {0}; run `firecrab service reinstall`")]
    Provision(#[from] provision::Error),
    #[error(
        "cannot repair microManager: firecrab-debian is not registered; run `firecrab service install`"
    )]
    NotImported,
    #[error("microManager repair failed: {0}; inspect `firecrab service debug --logs`")]
    Wsl(#[from] wsl::Error),
    #[error("microManager repair failed: {0}; inspect `firecrab service debug --logs`")]
    Daemon(#[from] daemon::Error),
    #[error("microManager repair failed: {0}; inspect `firecrab service debug --logs`")]
    Unhealthy(String),
}

pub fn run(layout: &Layout) -> Result<daemon::Status, Error> {
    run_with(layout, daemon::task_definition, daemon::READY_TIMEOUT)
}

fn run_with(
    layout: &Layout,
    task_definition: impl FnOnce() -> Result<String, daemon::Error>,
    timeout: Duration,
) -> Result<daemon::Status, Error> {
    report!("[REPAIR 1/6] validate the WSL disk, provisioning, and task prerequisites");
    validate(layout)?;
    let task = task_definition()?;

    report!("[REPAIR 2/6] stop the resident task and managed WSL distribution");
    daemon::stop()?;
    report!("[REPAIR 3/6] rebuild the scheduled task definition and registration");
    daemon::register(layout, &task)?;
    report!("[REPAIR 4/6] start the resident task and restart the installed guest services");
    daemon::resume()?;
    wsl::root_shell(&guest_service_script())?;
    report!("[REPAIR 5/6] wait for guest services and the localhost API");
    require_healthy(daemon::wait_ready(timeout)?)?;
    report!("[REPAIR 6/6] verify the resident task, live guest services, and localhost API");
    require_healthy(daemon::status())
}

/// Only host-side reads precede shutdown. Missing registration is recoverable;
/// a missing disk or an unfinished installation requires install/reinstall.
fn validate(layout: &Layout) -> Result<(), Error> {
    managed_home::reject_symlink_components(&layout.managed_home)?;
    for path in [
        layout.distro().join("ext4.vhdx"),
        layout.provision_marker(),
        layout.runtime().join("provision.failed"),
        layout.runtime().join("provision.phase"),
        layout.runtime().join("microManager-task.xml"),
    ] {
        managed_home::reject_symlink_components(&path)?;
    }
    let disk = layout.distro().join("ext4.vhdx");
    if !disk.is_file() {
        return Err(Error::MissingFile(disk));
    }
    if provision::guest_result(layout)?.is_none() {
        return Err(Error::IncompleteProvision);
    }
    // Unlike the status convenience helper, propagate a failed WSL query.
    let distributions = wsl::run(&["--list", "--quiet"])?;
    if !wsl::contains(&wsl::parse_list(&distributions), wsl::DISTRO_NAME) {
        return Err(Error::NotImported);
    }
    Ok(())
}

fn guest_service_script() -> String {
    format!(
        "set -eu; test -d /run/systemd/system; {}; \
         systemctl cat firecrab-api \"$helper\" >/dev/null; \
         systemctl daemon-reload; \
         systemctl reset-failed \"$helper\" firecrab-api; \
         systemctl enable \"$helper\" firecrab-api; \
         systemctl restart \"$helper\"; systemctl restart firecrab-api",
        daemon::HELPER_SETUP
    )
}

fn require_healthy(status: daemon::Status) -> Result<daemon::Status, Error> {
    if status.success() {
        Ok(status)
    } else {
        Err(Error::Unhealthy(format!(
            "task={:?}, ready={}, API reachable={}, guest={}",
            status.task,
            status.ready,
            status.api_reachable,
            status.detail.as_deref().unwrap_or("unknown")
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use wsl::fake;

    const ENABLED: &str = "<Task><Settings><Enabled>true</Enabled></Settings></Task>";
    const DISABLED: &str = "<Task><Settings><Enabled>false</Enabled></Settings></Task>";
    const PROVISIONED: &str = "schema=1\nkvm=usable\nfirecrab_api=active\nfirecrab_net_helper=active\nnested_firecracker=passed\n";
    const READY: &str = "firecrab-api=active\nfirecrab-helper=active\nip=172.20.1.5\n";

    fn installed() -> (tempfile::TempDir, Layout) {
        let directory = tempfile::tempdir().unwrap();
        let layout = Layout {
            managed_home: directory.path().join("micromanager"),
        };
        fs::create_dir_all(layout.distro()).unwrap();
        fs::create_dir_all(layout.runtime()).unwrap();
        fs::write(layout.distro().join("ext4.vhdx"), b"preserved disk").unwrap();
        fs::write(layout.provision_marker(), PROVISIONED).unwrap();
        (directory, layout)
    }

    fn repair(layout: &Layout) -> Result<daemon::Status, Error> {
        run_with(layout, || Ok(ENABLED.into()), Duration::from_secs(1))
    }

    fn healthy_host(line: &str) -> fake::Reply {
        match line {
            "wsl.exe --list --quiet" | "wsl.exe --list --running --quiet" => {
                Ok("Debian\nfirecrab-debian\n".into())
            }
            l if l.starts_with("schtasks.exe /Query") => Ok(ENABLED.into()),
            l if l.contains("systemctl is-active") => Ok(READY.into()),
            _ => Ok(String::new()),
        }
    }

    #[test]
    fn missing_disk_fails_before_any_host_command() {
        let (_directory, layout) = installed();
        fs::remove_file(layout.distro().join("ext4.vhdx")).unwrap();
        let host = fake::answer(|line| panic!("validation touched the host: {line}"));
        assert!(matches!(run(&layout), Err(Error::MissingFile(_))));
        assert!(host.calls().is_empty());
    }

    #[test]
    fn missing_or_stale_provisioning_fails_before_stopping() {
        for marker in [None, Some("schema=0\n"), Some("schema=1\nkvm=usable\n")] {
            let (_directory, layout) = installed();
            if let Some(marker) = marker {
                fs::write(layout.provision_marker(), marker).unwrap();
            } else {
                fs::remove_file(layout.provision_marker()).unwrap();
            }
            let host = fake::answer(|line| panic!("incomplete install touched the host: {line}"));
            assert!(matches!(run(&layout), Err(Error::IncompleteProvision)));
            assert!(host.calls().is_empty());
        }
    }

    #[test]
    fn failed_provisioning_is_preserved_and_requires_reinstall() {
        let (_directory, layout) = installed();
        fs::write(layout.runtime().join("provision.failed"), "23\n").unwrap();
        let host = fake::answer(|line| panic!("failed install touched the host: {line}"));
        let error = run(&layout).unwrap_err();
        assert!(matches!(error, Error::Provision(_)));
        assert!(error.to_string().contains("firecrab service reinstall"));
        assert_eq!(
            fs::read_to_string(layout.runtime().join("provision.failed")).unwrap(),
            "23\n"
        );
        assert!(host.calls().is_empty());
    }

    #[test]
    fn missing_distribution_or_failed_query_does_not_stop_or_import_anything() {
        for reply in [Ok("Debian\n".into()), Err("WSL unavailable".into())] {
            let (_directory, layout) = installed();
            let expected_missing = reply.is_ok();
            let host = fake::answer(move |line| {
                assert_eq!(line, "wsl.exe --list --quiet");
                reply.clone()
            });
            let error = run(&layout).unwrap_err();
            assert_eq!(matches!(error, Error::NotImported), expected_missing);
            assert_eq!(host.calls().len(), 1);
        }
    }

    #[test]
    fn missing_task_prerequisites_leave_a_running_guest_untouched() {
        let (_directory, layout) = installed();
        let host = fake::answer(healthy_host);
        let result = run_with(
            &layout,
            || Err(daemon::Error::MissingLauncher(PathBuf::from("wslg.exe"))),
            Duration::ZERO,
        );
        assert!(matches!(
            result,
            Err(Error::Daemon(daemon::Error::MissingLauncher(_)))
        ));
        assert_eq!(host.calls(), ["wsl.exe --list --quiet"]);
    }

    #[test]
    fn lost_or_damaged_task_definition_is_rebuilt_and_disk_is_preserved() {
        for old_definition in [None, Some("damaged XML")] {
            let (_directory, layout) = installed();
            let definition = layout.runtime().join("microManager-task.xml");
            if let Some(old) = old_definition {
                fs::write(&definition, old).unwrap();
            }
            let mut registered = false;
            let host = fake::answer(move |line| {
                if line.starts_with("schtasks.exe /Query") && !registered {
                    return Err("task missing".into());
                }
                if line.starts_with("schtasks.exe /Create") {
                    registered = true;
                }
                healthy_host(line)
            })
            .with_api_response(true);
            assert!(repair(&layout).unwrap().success());
            assert_eq!(
                wsl::decode_console(&fs::read(&definition).unwrap()),
                ENABLED
            );
            assert_eq!(
                fs::read(layout.distro().join("ext4.vhdx")).unwrap(),
                b"preserved disk"
            );
            assert_eq!(
                fs::read_to_string(layout.provision_marker()).unwrap(),
                PROVISIONED
            );
            let calls = host.calls();
            let position =
                |needle: &str| calls.iter().position(|call| call.contains(needle)).unwrap();
            assert!(position("--terminate") < position("/Create"));
            assert!(position("/Create") < position("/Run"));
            assert!(position("/Run") < position("systemctl restart"));
            assert!(calls.iter().all(|call| !call.contains("--import")
                && !call.contains("--unregister")
                && !call.contains("--shutdown")
                && !call.contains("install-firecrab")));
        }
    }

    #[test]
    fn stop_or_registration_failure_prevents_guest_restart() {
        for failed_command in ["/DISABLE", "/Create"] {
            let (_directory, layout) = installed();
            let host = fake::answer(move |line| {
                if line.contains(failed_command) {
                    Err("access denied".into())
                } else {
                    healthy_host(line)
                }
            });
            assert!(matches!(repair(&layout), Err(Error::Daemon(_))));
            assert!(
                !host
                    .calls()
                    .iter()
                    .any(|call| call.contains("/Run") || call.contains("systemctl restart"))
            );
        }
    }

    #[test]
    fn guest_restart_failure_is_reported_without_claiming_readiness() {
        let (_directory, layout) = installed();
        let host = fake::answer(|line| {
            if line.contains("systemctl restart") {
                Err("guest unit missing".into())
            } else {
                healthy_host(line)
            }
        })
        .with_api_response(true);
        let error = repair(&layout).unwrap_err();
        assert!(matches!(error, Error::Wsl(_)));
        assert!(error.to_string().contains("firecrab service debug --logs"));
        assert!(
            !host
                .calls()
                .iter()
                .any(|call| call.contains("systemctl is-active"))
        );
    }

    #[test]
    fn an_unreachable_local_api_times_out_even_with_active_guest_units() {
        let (_directory, layout) = installed();
        let _host = fake::answer(healthy_host).with_api_response(false);
        assert!(matches!(
            run_with(&layout, || Ok(ENABLED.into()), Duration::ZERO),
            Err(Error::Daemon(daemon::Error::ReadyTimeout(0)))
        ));
        let status = daemon::status();
        assert!(status.ready);
        assert!(!status.api_reachable);
        assert!(matches!(require_healthy(status), Err(Error::Unhealthy(_))));
    }

    #[test]
    fn a_task_disabled_during_repair_is_a_failure() {
        let (_directory, layout) = installed();
        let host = fake::answer(|line| {
            if line.starts_with("schtasks.exe /Query") {
                Ok(DISABLED.into())
            } else {
                healthy_host(line)
            }
        })
        .with_api_response(true);
        assert!(matches!(repair(&layout), Err(Error::Unhealthy(_))));
        assert!(host.calls().iter().any(|call| call.contains("/ENABLE")));
    }

    #[test]
    fn services_that_fail_after_readiness_are_rejected_by_the_final_check() {
        let (_directory, layout) = installed();
        let mut probes = 0;
        let _host = fake::answer(move |line| {
            if line.contains("systemctl is-active") {
                probes += 1;
                if probes > 1 {
                    return Ok("firecrab-api=failed\nfirecrab-helper=active\n".into());
                }
            }
            healthy_host(line)
        })
        .with_api_response(true);
        let error = repair(&layout).unwrap_err();
        assert!(matches!(error, Error::Unhealthy(_)));
        assert!(error.to_string().contains("firecrab-api=failed"));
    }

    #[cfg(unix)]
    #[test]
    fn guest_script_restarts_the_installed_helper_name_and_stops_on_missing_units() {
        for (load_state, helper) in [
            ("loaded", "firecrab-helper"),
            ("not-found", "firecrab-net-helper"),
        ] {
            let script = format!(
                "test() {{ [ \"$*\" = '-d /run/systemd/system' ]; }}; \
                 systemctl() {{ if [ \"$1\" = show ]; then printf '%s\\n' {load_state}; else printf '%s\\n' \"$*\"; fi; }}; {}",
                guest_service_script()
            );
            let output = std::process::Command::new("sh")
                .args(["-c", &script])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!(
                    "daemon-reload\nreset-failed {helper} firecrab-api\nenable {helper} firecrab-api\nrestart {helper}\nrestart firecrab-api\n"
                )
            );
        }
        let script = format!(
            "test() {{ return 0; }}; systemctl() {{ case $1 in show) printf 'loaded\\n';; cat) return 23;; *) printf 'unexpected mutation\\n';; esac; }}; {}",
            guest_service_script()
        );
        let output = std::process::Command::new("sh")
            .args(["-c", &script])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(23));
        assert!(
            output.stdout.is_empty(),
            "no mutation after missing guest units"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_disk_marker_or_task_definition_is_rejected_before_shutdown() {
        for relative in [
            "distro/ext4.vhdx",
            "runtime/provisioned",
            "runtime/microManager-task.xml",
        ] {
            let (directory, layout) = installed();
            let path = layout.managed_home.join(relative);
            if path.exists() {
                fs::remove_file(&path).unwrap();
            }
            let outside = directory.path().join("outside");
            fs::write(&outside, PROVISIONED).unwrap();
            std::os::unix::fs::symlink(&outside, &path).unwrap();
            let host = fake::answer(|line| panic!("unsafe path touched the host: {line}"));
            assert!(matches!(run(&layout), Err(Error::ManagedHome(_))));
            assert!(host.calls().is_empty());
            assert_eq!(fs::read_to_string(outside).unwrap(), PROVISIONED);
        }
    }
}

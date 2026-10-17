//! install.sh `do_uninstall`: 이 설치가 만든 것만 지운다. 데이터는 `--purge`에서만.

use super::Error;
use super::env::{LEGACY_HELPER_UNIT, ServiceEnv, UNITS};
use super::host_state;
use super::output;
use super::payload::{BUNDLE_BINARIES, EXTRACT_HELPERS, resolve_binary};
use super::privileged::Privileged;
use super::selinux;

const STEP: &str = "uninstall";

/// helper가 VM마다 띄우는 transient 유닛. API가 멈춰도 VM이 살아남도록 설계돼
/// 있어서, 따로 멈추지 않으면 uninstall 뒤에도 shim과 Firecracker가 남는다.
const VM_UNIT_PATTERN: &str = "firecrab-vm-*.service";

/// 실행 중이거나 실패 상태로 남은 `firecrab-vm-*` 유닛을 멈춘다.
///
/// helper와 API가 아직 떠 있을 때 해야 TAP 정리와 종료 기록이 평소 경로를
/// 탄다. 멈추지 못해도 uninstall은 계속한다 — 경고만 남긴다.
fn stop_vm_units(privileged: &Privileged<'_>) {
    let listing = privileged.runner().run(
        "systemctl",
        &[
            "list-units",
            "--all",
            "--plain",
            "--no-legend",
            "--no-pager",
            VM_UNIT_PATTERN,
        ],
    );
    let units = match listing {
        Ok(out) if out.status.success() => vm_units_in(&String::from_utf8_lossy(&out.stdout)),
        // systemd가 없는 호스트에는 멈출 유닛도 없다.
        _ => return,
    };
    if units.is_empty() {
        return;
    }
    output::step(&format!("stopping {} MicroVM unit(s)", units.len()));
    let mut stop = vec!["stop"];
    stop.extend(units.iter().map(String::as_str));
    let stopped = privileged
        .run("systemctl", &stop)
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !stopped {
        output::warn(
            "some MicroVM units did not stop — their firecracker processes may keep running",
        );
    }
    // `--collect`는 실패한 transient 유닛을 치우지 못한다.
    let mut reset = vec!["reset-failed"];
    reset.extend(units.iter().map(String::as_str));
    let _ = privileged.run("systemctl", &reset);
}

/// `systemctl list-units --plain --no-legend` 출력에서 VM 유닛 이름만.
fn vm_units_in(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|unit| unit.starts_with("firecrab-vm-") && unit.ends_with(".service"))
        .map(str::to_owned)
        .collect()
}

/// 유닛·바이너리·대시보드를 제거하고, `purge`면 데이터와 설정까지 지운다.
///
/// 계정은 남긴다 — 데이터 디렉터리가 그 계정 소유이고, 지우면 남은 파일이
/// 고아가 된다.
pub fn uninstall(privileged: &Privileged<'_>, env: &ServiceEnv, purge: bool) -> Result<(), Error> {
    stop_vm_units(privileged);
    for unit in UNITS.into_iter().rev().chain([LEGACY_HELPER_UNIT]) {
        // 이미 없거나 비활성이면 실패가 정상이므로 결과를 보지 않는다.
        let _ = privileged.run("systemctl", &["disable", "--now", unit]);
        let path = env.unit_path(unit).to_string_lossy().into_owned();
        privileged.run_ok(STEP, "rm", &["-f", &path])?;
    }
    privileged.run_ok(STEP, "systemctl", &["daemon-reload"])?;
    output::log("units removed");

    // SIGTERM은 helper의 소켓 루프만 멈춘다 — helper가 만든 브리지, TAP,
    // nftables 테이블은 재부팅 전까지 남는다. 바이너리가 아직 디스크에 있는
    // 동안 자체 teardown을 돌린다.
    if let Some(teardown) = resolve_binary("firecrab-helper", None, &env.libdir) {
        let helper_s = teardown.to_string_lossy().into_owned();
        let ok = privileged
            .run(&helper_s, &["--teardown"])
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            output::warn(
                "network teardown failed — bridges/nftables tables may remain until reboot",
            );
        }
    }

    // 설치기가 libdir에 넣는 전부: 바이너리 둘, 옛 이름 링크, 커널 추출 스크립트.
    // 하나라도 빠지면 디렉터리가 비지 않아 아래 `rmdir`이 조용히 건너뛴다.
    let installed = BUNDLE_BINARIES[..2]
        .iter()
        .chain(&["firecrab-net-helper"])
        .chain(EXTRACT_HELPERS.iter());
    let mut doomed: Vec<String> = installed
        .map(|name| env.libdir.join(name).to_string_lossy().into_owned())
        .collect();
    doomed.push(env.bindir.join("firecrab").to_string_lossy().into_owned());
    let mut rm = vec!["-f"];
    rm.extend(doomed.iter().map(String::as_str));
    privileged.run_ok(STEP, "rm", &rm)?;

    let dashboard = env
        .sharedir
        .join("dashboard")
        .to_string_lossy()
        .into_owned();
    let licenses = env.sharedir.join("licenses").to_string_lossy().into_owned();
    privileged.run_ok(STEP, "rm", &["-rf", &dashboard, &licenses])?;

    let license = env.sharedir.join("LICENSE").to_string_lossy().into_owned();
    let notices = env
        .sharedir
        .join("THIRD_PARTY_NOTICES.txt")
        .to_string_lossy()
        .into_owned();
    let inventory = env
        .sharedir
        .join("release-license-inventory.json")
        .to_string_lossy()
        .into_owned();
    privileged.run_ok(STEP, "rm", &["-f", &license, &notices, &inventory])?;

    // Firecracker는 따로 설치되므로 일부러 남긴다 — 그 바이너리 옆의 업스트림
    // 고지도 함께 남는다.
    let libdir = env.libdir.to_string_lossy().into_owned();
    let sharedir = env.sharedir.to_string_lossy().into_owned();
    let _ = privileged.run("rmdir", &["--ignore-fail-on-non-empty", &libdir, &sharedir]);

    selinux::unlabel_binaries(privileged, env);
    output::log("binaries and dashboard removed");

    // 기록 파일이 `$CONFDIR`에 있으니 purge보다 먼저.
    host_state::restore(privileged, env);

    if purge {
        output::warn(&format!(
            "purging {} and {} (all VM disks and the database)",
            env.datadir.display(),
            env.confdir.display()
        ));
        let (data, conf) = (
            env.datadir.to_string_lossy().into_owned(),
            env.confdir.to_string_lossy().into_owned(),
        );
        privileged.run_ok(STEP, "rm", &["-rf", &data, &conf])?;
    } else {
        output::log(&format!(
            "kept {} and {} — pass --purge to delete them too",
            env.datadir.display(),
            env.confdir.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::FakeCommandRunner;
    use std::path::Path;

    fn env_for(root: &Path) -> ServiceEnv {
        ServiceEnv::from_values(
            "firecrab",
            "firecrab",
            &root.join("usr/local"),
            &root.join("var/lib/firecrab"),
            &root.join("etc/firecrab"),
            &root.join("etc/systemd/system"),
        )
    }

    fn seeded_env(root: &Path) -> ServiceEnv {
        let env = env_for(root);
        std::fs::create_dir_all(&env.libdir).unwrap();
        std::fs::create_dir_all(&env.unitdir).unwrap();
        std::fs::write(env.libdir.join("firecrab-helper"), b"x").unwrap();
        for unit in UNITS {
            std::fs::write(env.unit_path(unit), b"[Unit]\n").unwrap();
        }
        env
    }

    #[test]
    fn units_are_disabled_removed_and_reloaded_before_anything_else() {
        let dir = tempfile::tempdir().unwrap();
        let env = seeded_env(dir.path());
        let fake = FakeCommandRunner::permissive();
        uninstall(&Privileged::with_sudo(&fake, true), &env, false).unwrap();
        let calls = fake.calls();
        let reload = calls
            .iter()
            .position(|c| c == "sudo systemctl daemon-reload")
            .unwrap();
        for unit in UNITS {
            let disable = calls
                .iter()
                .position(|c| c == &format!("sudo systemctl disable --now {unit}"))
                .unwrap();
            let remove = calls
                .iter()
                .position(|c| c == &format!("sudo rm -f {}", env.unit_path(unit).display()))
                .unwrap();
            assert!(disable < remove && remove < reload, "{calls:?}");
        }
    }

    #[test]
    fn network_teardown_runs_while_the_helper_binary_is_still_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let env = seeded_env(dir.path());
        let fake = FakeCommandRunner::permissive();
        uninstall(&Privileged::with_sudo(&fake, true), &env, false).unwrap();
        let calls = fake.calls();
        let teardown = calls
            .iter()
            .position(|c| {
                c == &format!(
                    "sudo {} --teardown",
                    env.libdir.join("firecrab-helper").display()
                )
            })
            .expect("teardown call");
        // `rposition`, not `position`: the unit-file removal loop earlier in
        // the run also matches "starts with sudo rm -f and contains
        // firecrab-helper" (it deletes firecrab-helper.service). The
        // binary removal this test cares about is the later, combined call.
        let removal = calls
            .iter()
            .rposition(|c| c.starts_with("sudo rm -f ") && c.contains("firecrab-helper"))
            .unwrap();
        assert!(teardown < removal, "{calls:?}");
    }

    #[test]
    fn a_failing_teardown_is_only_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let env = seeded_env(dir.path());
        let mut fake = FakeCommandRunner::permissive();
        let helper = env.libdir.join("firecrab-helper").display().to_string();
        fake.set("sudo", &[&helper, "--teardown"], 1, "", "rtnetlink error\n");
        uninstall(&Privileged::with_sudo(&fake, true), &env, false).unwrap();
    }

    #[test]
    fn data_and_config_survive_without_purge() {
        let dir = tempfile::tempdir().unwrap();
        let env = seeded_env(dir.path());
        let fake = FakeCommandRunner::permissive();
        uninstall(&Privileged::with_sudo(&fake, true), &env, false).unwrap();
        let calls = fake.calls();
        assert!(
            !calls
                .iter()
                .any(|c| c.contains(&env.datadir.display().to_string())
                    && c.starts_with("sudo rm -rf")),
            "{calls:?}"
        );
    }

    #[test]
    fn purge_removes_data_and_config() {
        let dir = tempfile::tempdir().unwrap();
        let env = seeded_env(dir.path());
        let fake = FakeCommandRunner::permissive();
        uninstall(&Privileged::with_sudo(&fake, true), &env, true).unwrap();
        assert!(
            fake.calls().iter().any(|c| c
                == &format!(
                    "sudo rm -rf {} {}",
                    env.datadir.display(),
                    env.confdir.display()
                )),
            "{:?}",
            fake.calls()
        );
    }

    /// `systemctl list-units` for the helper's transient per-VM units.
    const VM_LISTING: [&str; 6] = [
        "list-units",
        "--all",
        "--plain",
        "--no-legend",
        "--no-pager",
        "firecrab-vm-*.service",
    ];

    #[test]
    fn running_vms_are_stopped_before_the_services_they_depend_on_go() {
        let dir = tempfile::tempdir().unwrap();
        let env = seeded_env(dir.path());
        let mut fake = FakeCommandRunner::permissive();
        fake.set(
            "systemctl",
            &VM_LISTING,
            0,
            "firecrab-vm-aaaa.service loaded active running firecrab MicroVM aaaa\n\
             firecrab-vm-bbbb.service loaded failed failed firecrab MicroVM bbbb\n",
            "",
        );
        uninstall(&Privileged::with_sudo(&fake, true), &env, false).unwrap();
        let calls = fake.calls();
        let stop = calls
            .iter()
            .position(|c| {
                c == "sudo systemctl stop firecrab-vm-aaaa.service firecrab-vm-bbbb.service"
            })
            .unwrap_or_else(|| panic!("{calls:?}"));
        let first_unit_removed = calls
            .iter()
            .position(|c| c.starts_with("sudo systemctl disable --now"))
            .unwrap();
        assert!(stop < first_unit_removed, "{calls:?}");
        assert!(
            calls.contains(
                &"sudo systemctl reset-failed firecrab-vm-aaaa.service firecrab-vm-bbbb.service"
                    .to_owned()
            ),
            "{calls:?}"
        );
    }

    #[test]
    fn nothing_is_stopped_when_there_is_no_vm_unit() {
        let dir = tempfile::tempdir().unwrap();
        let env = seeded_env(dir.path());
        let mut fake = FakeCommandRunner::permissive();
        fake.set("systemctl", &VM_LISTING, 0, "", "");
        uninstall(&Privileged::with_sudo(&fake, true), &env, false).unwrap();
        let calls = fake.calls();
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with("sudo systemctl stop") || c.contains("reset-failed")),
            "{calls:?}"
        );
    }

    #[test]
    fn a_host_without_systemd_has_no_vm_units_to_stop() {
        let dir = tempfile::tempdir().unwrap();
        let env = seeded_env(dir.path());
        let mut fake = FakeCommandRunner::permissive();
        fake.set(
            "systemctl",
            &VM_LISTING,
            1,
            "",
            "System has not been booted with systemd",
        );
        uninstall(&Privileged::with_sudo(&fake, true), &env, false).unwrap();
        assert!(
            !fake
                .calls()
                .iter()
                .any(|c| c.starts_with("sudo systemctl stop")),
            "{:?}",
            fake.calls()
        );
    }

    #[test]
    fn every_file_the_installer_puts_in_libdir_is_removed() {
        use crate::service::payload::{BUNDLE_BINARIES, EXTRACT_HELPERS};
        let dir = tempfile::tempdir().unwrap();
        let env = seeded_env(dir.path());
        let fake = FakeCommandRunner::permissive();
        uninstall(&Privileged::with_sudo(&fake, true), &env, false).unwrap();
        let calls = fake.calls();
        let removal = calls
            .iter()
            .rfind(|c| c.starts_with("sudo rm -f ") && c.contains("firecrab-helper"))
            .unwrap_or_else(|| panic!("{calls:?}"));
        // layout.rs installs the first two bundle binaries, the legacy helper
        // name and the extract helpers into libdir; each one must be listed,
        // or the directory is not empty and `rmdir` leaves it behind.
        let installed = BUNDLE_BINARIES[..2]
            .iter()
            .chain(&["firecrab-net-helper"])
            .chain(EXTRACT_HELPERS.iter());
        for name in installed {
            let path = env.libdir.join(name).display().to_string();
            assert!(removal.contains(&path), "{path} is not removed: {removal}");
        }
    }

    #[test]
    fn binaries_dashboard_and_licenses_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let env = seeded_env(dir.path());
        let fake = FakeCommandRunner::permissive();
        uninstall(&Privileged::with_sudo(&fake, true), &env, false).unwrap();
        let calls = fake.calls().join("\n");
        assert!(
            calls.contains(&env.libdir.join("firecrab-api").display().to_string()),
            "{calls}"
        );
        assert!(
            calls.contains(&env.bindir.join("firecrab").display().to_string()),
            "{calls}"
        );
        assert!(
            calls.contains(&env.sharedir.join("dashboard").display().to_string()),
            "{calls}"
        );
        assert!(
            calls.contains(&env.sharedir.join("licenses").display().to_string()),
            "{calls}"
        );
        assert!(
            calls.contains("rmdir --ignore-fail-on-non-empty"),
            "{calls}"
        );
    }
}

//! install.sh `do_uninstall`: 이 설치가 만든 것만 지운다. 데이터는 `--purge`에서만.

use super::Error;
use super::env::{ServiceEnv, UNITS};
use super::output;
use super::privileged::Privileged;
use super::selinux;

const STEP: &str = "uninstall";

/// 유닛·바이너리·대시보드를 제거하고, `purge`면 데이터와 설정까지 지운다.
///
/// 계정은 남긴다 — 데이터 디렉터리가 그 계정 소유이고, 지우면 남은 파일이
/// 고아가 된다.
pub fn uninstall(privileged: &Privileged<'_>, env: &ServiceEnv, purge: bool) -> Result<(), Error> {
    for unit in UNITS {
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
    let helper = env.libdir.join("firecrab-net-helper");
    if helper.is_file() {
        let helper_s = helper.to_string_lossy().into_owned();
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

    let api = env
        .libdir
        .join("firecrab-api")
        .to_string_lossy()
        .into_owned();
    let helper_s = helper.to_string_lossy().into_owned();
    let cli = env.bindir.join("firecrab").to_string_lossy().into_owned();
    privileged.run_ok(STEP, "rm", &["-f", &api, &helper_s, &cli])?;

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
        std::fs::write(env.libdir.join("firecrab-net-helper"), b"x").unwrap();
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
                    env.libdir.join("firecrab-net-helper").display()
                )
            })
            .expect("teardown call");
        // `rposition`, not `position`: the unit-file removal loop earlier in
        // the run also matches "starts with sudo rm -f and contains
        // firecrab-net-helper" (it deletes firecrab-net-helper.service). The
        // binary removal this test cares about is the later, combined call.
        let removal = calls
            .iter()
            .rposition(|c| c.starts_with("sudo rm -f ") && c.contains("firecrab-net-helper"))
            .unwrap();
        assert!(teardown < removal, "{calls:?}");
    }

    #[test]
    fn a_failing_teardown_is_only_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let env = seeded_env(dir.path());
        let mut fake = FakeCommandRunner::permissive();
        let helper = env.libdir.join("firecrab-net-helper").display().to_string();
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

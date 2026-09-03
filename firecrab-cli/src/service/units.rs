//! install.sh `install_units` / `start_units` / `wait_for_api`, 그리고
//! `firecrab service start|stop|restart|enable|disable`의 systemd 호출.

use std::time::Duration;

use crate::shell::CommandRunner;

use super::Error;
use super::env::{ServiceEnv, UNITS};
use super::output;
use super::payload::Payload;
use super::privileged::Privileged;

const STEP_UNITS: &str = "units";
const STEP_START: &str = "start";
const STEP_STOP: &str = "stop";

/// API가 실제로 응답할 때까지 기다리는 최대 횟수/간격.
const API_ATTEMPTS: u32 = 60;
const API_INTERVAL: Duration = Duration::from_secs(1);

/// 유닛을 재시작한 직후, 상태를 확인하기 전에 기다리는 시간
/// (install.sh `start_units`의 `sleep 1`과 같다 — Type=simple 유닛은 fork된
/// 즉시 `is-active`가 참이 되므로, 곧바로 죽는 유닛을 놓치지 않으려면 잠깐
/// 기다려야 한다).
const SETTLE: Duration = Duration::from_secs(1);

/// install.sh `install_units`의 sed 치환.
pub fn render_unit(template: &str, env: &ServiceEnv, uid: &str) -> String {
    template
        .replace("@LIBDIR@", &env.libdir.to_string_lossy())
        .replace("@SHAREDIR@", &env.sharedir.to_string_lossy())
        .replace("@DATADIR@", &env.datadir.to_string_lossy())
        .replace("@CONFDIR@", &env.confdir.to_string_lossy())
        .replace("@PREFIX@", &env.prefix.to_string_lossy())
        .replace("@FIRECRAB_USER@", &env.user)
        .replace("@FIRECRAB_GROUP@", &env.group)
        .replace("@FIRECRAB_UID@", uid)
}

/// 템플릿을 이 호스트의 경로·계정으로 렌더해 `$UNITDIR`에 쓰고 daemon-reload.
pub fn install_units(
    privileged: &Privileged<'_>,
    env: &ServiceEnv,
    payload: &Payload,
) -> Result<(), Error> {
    let uid_out = privileged
        .runner()
        .run("id", &["-u", &env.user])
        .map_err(|e| Error::step(STEP_UNITS, format!("id -u {}: {e}", env.user)))?;
    if !uid_out.status.success() {
        return Err(Error::step(
            STEP_UNITS,
            format!("id -u {} failed", env.user),
        ));
    }
    let uid = String::from_utf8_lossy(&uid_out.stdout).trim().to_owned();

    for unit in UNITS {
        let source = payload.units.join(unit);
        let template = std::fs::read_to_string(&source).map_err(|e| {
            Error::step(
                STEP_UNITS,
                format!("could not read {}: {e}", source.display()),
            )
        })?;
        let rendered = render_unit(&template, env, &uid);
        privileged.write_file(
            STEP_UNITS,
            &env.unit_path(unit),
            rendered.as_bytes(),
            "0644",
        )?;
    }
    privileged.run_ok(STEP_UNITS, "systemctl", &["daemon-reload"])?;
    output::log(&format!("units installed to {}", env.unitdir.display()));
    Ok(())
}

/// 두 유닛 파일이 모두 `$UNITDIR`에 있어야 서비스 제어가 의미를 갖는다.
pub fn require_installed(env: &ServiceEnv) -> Result<(), Error> {
    if UNITS.iter().all(|unit| env.unit_path(unit).is_file()) {
        Ok(())
    } else {
        Err(Error::NotInstalled)
    }
}

/// `systemctl is-active <unit>`, systemctl 자체가 없으면 `"unknown"`.
///
/// `status` 서브커맨드의 `systemd_is_active`와 같은 규칙이다 — 여기서 그
/// 구현에 위임한다.
pub fn is_active(runner: &dyn CommandRunner, unit: &str) -> String {
    crate::status::systemd_is_active(runner, unit)
}

/// 부팅 시 활성화.
pub fn enable(privileged: &Privileged<'_>) -> Result<(), Error> {
    let mut args = vec!["enable"];
    args.extend(UNITS);
    privileged
        .run_ok(STEP_UNITS, "systemctl", &args)
        .map(|_| ())
}

/// 부팅 시 비활성화.
pub fn disable(privileged: &Privileged<'_>) -> Result<(), Error> {
    let mut args = vec!["disable"];
    args.extend(UNITS);
    privileged
        .run_ok(STEP_UNITS, "systemctl", &args)
        .map(|_| ())
}

/// `probe`가 참을 돌려줄 때까지 `attempts`번 재시도한다.
pub fn wait_for(probe: &dyn Fn() -> bool, attempts: u32, interval: Duration) -> bool {
    for attempt in 0..attempts {
        if probe() {
            return true;
        }
        if attempt + 1 < attempts {
            std::thread::sleep(interval);
        }
    }
    false
}

/// install.sh `wait_for_api`의 HTTP probe.
///
/// `is-active`는 Type=simple 유닛에서 프로세스가 fork 되는 순간 참이 된다 —
/// 실제로 서빙하는 시점이 아니다. API는 리스너를 열기 전에 설치된 모든 템플릿
/// 아티팩트를 해시하므로, 큰 rootfs가 있으면 fork 이후로도 한참 걸린다.
pub fn http_probe(bind: &str) -> bool {
    let url = format!("http://{bind}/");
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .ok()
        .and_then(|client| client.get(&url).send().ok())
        .map(|response| response.status().is_success())
        .unwrap_or(false)
}

/// [`confirm_active`]의 본체 — `settle`과 `(attempts, interval)`을 주입해
/// 실서비스 경로의 대기 시간을 테스트에서는 0에 가깝게 줄일 수 있게 한다.
fn confirm_active_with(
    privileged: &Privileged<'_>,
    probe: &dyn Fn() -> bool,
    settle: Duration,
    attempts: u32,
    interval: Duration,
) -> Result<(), Error> {
    std::thread::sleep(settle);
    let mut failed = Vec::new();
    for unit in UNITS {
        if is_active(privileged.runner(), unit) == "active" {
            output::log(&format!("{unit} is running"));
        } else {
            output::warn(&format!(
                "{unit} failed to start — journalctl -u {unit} -n 30"
            ));
            failed.push(unit);
        }
    }
    if !failed.is_empty() {
        return Err(Error::step_fix(
            STEP_START,
            format!("did not come up: {}", failed.join(", ")),
            format!("journalctl -u {} -n 30", failed[0]),
        ));
    }
    let bind = ServiceEnv::api_bind();
    if wait_for(probe, attempts, interval) {
        Ok(())
    } else {
        Err(Error::step_fix(
            STEP_START,
            format!("firecrab-api did not answer http://{bind}/ within {API_ATTEMPTS}s"),
            "journalctl -u firecrab-api -n 30",
        ))
    }
}

fn confirm_active(privileged: &Privileged<'_>, probe: &dyn Fn() -> bool) -> Result<(), Error> {
    confirm_active_with(privileged, probe, SETTLE, API_ATTEMPTS, API_INTERVAL)
}

/// 부팅 순서대로 시작한 뒤 실제로 떴는지 확인한다.
pub fn start(privileged: &Privileged<'_>, probe: &dyn Fn() -> bool) -> Result<(), Error> {
    let mut args = vec!["start"];
    args.extend(UNITS);
    privileged.run_ok(STEP_START, "systemctl", &args)?;
    confirm_active(privileged, probe)
}

/// install.sh `start_units`: enable + restart + 확인.
pub fn enable_and_start(
    privileged: &Privileged<'_>,
    probe: &dyn Fn() -> bool,
) -> Result<(), Error> {
    enable(privileged)?;
    let mut args = vec!["restart"];
    args.extend(UNITS);
    privileged.run_ok(STEP_START, "systemctl", &args)?;
    confirm_active(privileged, probe)
}

/// 역순으로 정지한다 — API가 helper보다 먼저 내려가야 한다.
pub fn stop(privileged: &Privileged<'_>) -> Result<(), Error> {
    let mut args = vec!["stop"];
    args.extend(UNITS.iter().rev().copied());
    privileged.run_ok(STEP_STOP, "systemctl", &args)?;
    for unit in UNITS {
        let state = is_active(privileged.runner(), unit);
        if state == "active" {
            return Err(Error::step(STEP_STOP, format!("{unit} is still active")));
        }
        output::log(&format!(
            "{unit} is {}",
            if state.is_empty() { "stopped" } else { &state }
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::FakeCommandRunner;
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, Ordering};

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

    const TEMPLATE: &str = "\
User=@FIRECRAB_USER@
Group=@FIRECRAB_GROUP@
WorkingDirectory=@DATADIR@
Environment=FIRECRAB_STATIC_ROOT=@SHAREDIR@/dashboard
Environment=PREFIX=@PREFIX@
Environment=FIRECRAB_LIBDIR=@LIBDIR@
EnvironmentFile=-@CONFDIR@/api.env
ExecStart=@LIBDIR@/firecrab-api
Environment=FIRECRAB_NET_HELPER_ALLOWED_UID=@FIRECRAB_UID@
";

    #[test]
    fn render_unit_replaces_every_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        let rendered = render_unit(TEMPLATE, &env, "900");
        assert!(
            !rendered.contains('@'),
            "unsubstituted placeholder left:\n{rendered}"
        );
        assert!(rendered.contains(&format!("WorkingDirectory={}", env.datadir.display())));
        assert!(rendered.contains(&format!("ExecStart={}/firecrab-api", env.libdir.display())));
        assert!(rendered.contains("FIRECRAB_NET_HELPER_ALLOWED_UID=900"));
        assert!(rendered.contains("User=firecrab"));
    }

    #[test]
    fn install_units_writes_both_units_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let env = env_for(root);
        let units_dir = root.join("payload-units");
        std::fs::create_dir_all(&units_dir).unwrap();
        for unit in UNITS {
            std::fs::write(units_dir.join(unit), TEMPLATE).unwrap();
        }
        let payload = Payload {
            bin: root.to_path_buf(),
            units: units_dir,
            extract: root.to_path_buf(),
            dashboard: None,
            license: root.join("LICENSE"),
            gpl: root.join("GPL"),
            third_party: None,
            inventory: None,
            checkout: None,
            _temp: None,
        };
        let mut fake = FakeCommandRunner::permissive();
        fake.set("id", &["-u", "firecrab"], 0, "900\n", "");
        install_units(&Privileged::with_sudo(&fake, true), &env, &payload).unwrap();
        let calls = fake.calls();
        for unit in UNITS {
            let path = env.unit_path(unit);
            assert!(
                calls
                    .iter()
                    .any(|c| c == &format!("sudo tee {}", path.display())),
                "{calls:?}"
            );
            let written = fake
                .stdin_of(&format!("sudo tee {}", path.display()))
                .unwrap();
            assert!(!String::from_utf8_lossy(&written).contains('@'));
        }
        assert!(
            calls.iter().any(|c| c == "sudo systemctl daemon-reload"),
            "{calls:?}"
        );
    }

    #[test]
    fn require_installed_checks_both_unit_files() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        std::fs::create_dir_all(&env.unitdir).unwrap();
        assert!(matches!(require_installed(&env), Err(Error::NotInstalled)));
        for unit in UNITS {
            std::fs::write(env.unit_path(unit), b"[Unit]\n").unwrap();
        }
        assert!(require_installed(&env).is_ok());
    }

    #[test]
    fn start_uses_boot_order_and_stop_reverses_it() {
        let mut fake = FakeCommandRunner::permissive();
        // First is-active probe (inside `start`'s confirm) sees both units up;
        // the second (inside `stop`'s post-check) sees them down again.
        for unit in UNITS {
            fake.set_seq(
                "systemctl",
                &["is-active", unit],
                &[(0, "active\n", ""), (3, "inactive\n", "")],
            );
        }
        let privileged = Privileged::with_sudo(&fake, true);
        start_with_settle(&privileged, &|| true, Duration::ZERO).unwrap();
        stop(&privileged).unwrap();
        let calls = fake.calls();
        assert!(
            calls
                .iter()
                .any(|c| c
                    == "sudo systemctl start firecrab-net-helper.service firecrab-api.service"),
            "{calls:?}"
        );
        assert!(
            calls.iter().any(
                |c| c == "sudo systemctl stop firecrab-api.service firecrab-net-helper.service"
            ),
            "{calls:?}"
        );
    }

    #[test]
    fn start_fails_when_a_unit_is_not_active() {
        let mut fake = FakeCommandRunner::permissive();
        fake.set(
            "systemctl",
            &["is-active", "firecrab-api.service"],
            1,
            "failed\n",
            "",
        );
        fake.set(
            "systemctl",
            &["is-active", "firecrab-net-helper.service"],
            0,
            "active\n",
            "",
        );
        let err = start_with_settle(
            &Privileged::with_sudo(&fake, true),
            &|| true,
            Duration::ZERO,
        )
        .unwrap_err();
        match err {
            Error::Step {
                step: "start",
                detail,
                ..
            } => assert!(detail.contains("firecrab-api.service"), "{detail}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn start_fails_when_the_api_never_answers() {
        let mut fake = FakeCommandRunner::permissive();
        for unit in UNITS {
            fake.set("systemctl", &["is-active", unit], 0, "active\n", "");
        }
        let err = start_with_settle(
            &Privileged::with_sudo(&fake, true),
            &|| false,
            Duration::ZERO,
        )
        .unwrap_err();
        assert!(matches!(err, Error::Step { step: "start", .. }), "{err:?}");
    }

    #[test]
    fn enable_and_disable_pass_both_units() {
        let fake = FakeCommandRunner::permissive();
        let privileged = Privileged::with_sudo(&fake, true);
        enable(&privileged).unwrap();
        disable(&privileged).unwrap();
        assert_eq!(
            fake.calls(),
            vec![
                "sudo systemctl enable firecrab-net-helper.service firecrab-api.service",
                "sudo systemctl disable firecrab-net-helper.service firecrab-api.service",
            ]
        );
    }

    #[test]
    fn is_active_collapses_a_missing_systemctl_to_unknown() {
        let mut fake = FakeCommandRunner::new();
        fake.set(
            "systemctl",
            &["is-active", "firecrab-api.service"],
            0,
            "active\n",
            "",
        );
        assert_eq!(is_active(&fake, "firecrab-api.service"), "active");
        assert_eq!(is_active(&fake, "firecrab-net-helper.service"), "unknown");
    }

    #[test]
    fn wait_for_retries_until_the_probe_succeeds() {
        let calls = AtomicU32::new(0);
        let probe = || calls.fetch_add(1, Ordering::SeqCst) >= 2;
        assert!(wait_for(&probe, 5, Duration::from_millis(1)));
        assert_eq!(calls.load(Ordering::SeqCst), 3);

        let never = || false;
        assert!(!wait_for(&never, 3, Duration::from_millis(1)));
    }

    /// [`start`] with an injectable settle duration and polling budget, for
    /// tests only — the real entry point always uses [`SETTLE`] and the real
    /// [`API_ATTEMPTS`]/[`API_INTERVAL`].
    fn start_with_settle(
        privileged: &Privileged<'_>,
        probe: &dyn Fn() -> bool,
        settle: Duration,
    ) -> Result<(), Error> {
        let mut args = vec!["start"];
        args.extend(UNITS);
        privileged.run_ok(STEP_START, "systemctl", &args)?;
        confirm_active_with(privileged, probe, settle, 3, Duration::from_millis(1))
    }
}

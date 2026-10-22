//! install.sh `install_units` / `start_units` / `wait_for_api`, plus systemd calls
//! for `firecrab service start|stop|restart|enable|disable`.

use std::time::Duration;

use crate::shell::CommandRunner;

use super::Error;
use super::env::{LEGACY_HELPER_UNIT, ServiceEnv, UNITS};
use super::output;
use super::payload::Payload;
use super::privileged::Privileged;

const STEP_UNITS: &str = "units";
const STEP_START: &str = "start";
const STEP_STOP: &str = "stop";

/// Maximum attempts and interval for waiting until the API responds.
const API_ATTEMPTS: u32 = 60;
const API_INTERVAL: Duration = Duration::from_secs(1);

/// Delay between restarting a unit and checking its status, matching `sleep 1`
/// in install.sh `start_units`. For Type=simple units, `is-active` becomes true
/// as soon as the process is forked. Wait briefly to catch units that exit
/// immediately.
const SETTLE: Duration = Duration::from_secs(1);

/// The sed substitutions from install.sh `install_units`.
pub fn render_unit(template: &str, env: &ServiceEnv, uid: &str) -> String {
    let normalized = template
        .lines()
        .filter(|line| !line.starts_with("Alias="))
        .map(|line| line.replace("firecrab-net-helper", "firecrab-helper"))
        .collect::<Vec<_>>()
        .join("\n");
    normalized
        .replace("@LIBDIR@", &env.libdir.to_string_lossy())
        .replace("@SHAREDIR@", &env.sharedir.to_string_lossy())
        .replace("@DATADIR@", &env.datadir.to_string_lossy())
        .replace("@CONFDIR@", &env.confdir.to_string_lossy())
        .replace("@UNITDIR@", &env.unitdir.to_string_lossy())
        .replace("@PREFIX@", &env.prefix.to_string_lossy())
        .replace("@FIRECRAB_USER@", &env.user)
        .replace("@FIRECRAB_GROUP@", &env.group)
        .replace("@FIRECRAB_UID@", uid)
        + "\n"
}

/// Render templates with this host's paths and accounts, write to `$UNITDIR`, then run daemon-reload.
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

    let mut rendered_units = Vec::new();
    for unit in UNITS {
        let mut source = payload.units.join(unit);
        if unit == UNITS[0] && !source.is_file() {
            source = payload.units.join(LEGACY_HELPER_UNIT);
        }
        let template = std::fs::read_to_string(&source).map_err(|e| {
            Error::step(
                STEP_UNITS,
                format!("could not read {}: {e}", source.display()),
            )
        })?;
        let mut rendered = render_unit(&template, env, &uid);
        if unit == UNITS[0] {
            rendered.push_str("Alias=firecrab-net-helper.service\n");
        }
        rendered_units.push((unit, rendered));
    }
    let legacy = env.unit_path(LEGACY_HELPER_UNIT);
    if legacy.is_file() && !legacy.is_symlink() {
        privileged.run_ok(
            STEP_UNITS,
            "systemctl",
            &["disable", "--now", LEGACY_HELPER_UNIT],
        )?;
        privileged.run_ok(STEP_UNITS, "rm", &["-f", &legacy.to_string_lossy()])?;
    }
    for (unit, rendered) in rendered_units {
        privileged.write_file(
            STEP_UNITS,
            &env.unit_path(unit),
            rendered.as_bytes(),
            "0644",
        )?;
    }
    privileged.run_ok(
        STEP_UNITS,
        "ln",
        &["-sfn", UNITS[0], &legacy.to_string_lossy()],
    )?;
    privileged.run_ok(STEP_UNITS, "systemctl", &["daemon-reload"])?;
    output::log(&format!("units installed to {}", env.unitdir.display()));
    Ok(())
}

/// Both unit files must be in `$UNITDIR` before service control is useful.
pub fn require_installed(env: &ServiceEnv) -> Result<(), Error> {
    if env.unit_path(UNITS[1]).is_file()
        && (env.unit_path(UNITS[0]).is_file() || env.unit_path(LEGACY_HELPER_UNIT).is_file())
    {
        Ok(())
    } else {
        Err(Error::NotInstalled)
    }
}

/// Use an existing legacy service when the canonical unit is not installed.
pub(crate) fn helper_unit(runner: &dyn CommandRunner) -> &'static str {
    let load = |unit| {
        runner
            .run(
                "systemctl",
                &["show", "--property=LoadState", "--value", unit],
            )
            .ok()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    if load(UNITS[0]).as_deref() == Some("not-found")
        && load(LEGACY_HELPER_UNIT).as_deref() == Some("loaded")
    {
        LEGACY_HELPER_UNIT
    } else {
        UNITS[0]
    }
}

fn control_units(privileged: &Privileged<'_>) -> [&'static str; 2] {
    [helper_unit(privileged.runner()), UNITS[1]]
}

/// Run `systemctl is-active <unit>`, or return `"unknown"` if systemctl is unavailable.
///
/// Use the same rules as the `status` subcommand's `systemd_is_active`
/// by delegating to that implementation.
pub fn is_active(runner: &dyn CommandRunner, unit: &str) -> String {
    crate::status::systemd_is_active(runner, unit)
}

/// Enable at boot.
pub fn enable(privileged: &Privileged<'_>) -> Result<(), Error> {
    let mut args = vec!["enable"];
    args.extend(control_units(privileged));
    privileged
        .run_ok(STEP_UNITS, "systemctl", &args)
        .map(|_| ())
}

/// Disable at boot.
pub fn disable(privileged: &Privileged<'_>) -> Result<(), Error> {
    let mut args = vec!["disable"];
    args.extend(control_units(privileged));
    privileged
        .run_ok(STEP_UNITS, "systemctl", &args)
        .map(|_| ())
}

/// Retry up to `attempts` times until `probe` returns true.
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

/// HTTP probe from install.sh `wait_for_api`.
///
/// For Type=simple units, `is-active` becomes true when the process is forked,
/// before it can serve requests. The API hashes all installed template artifacts
/// before opening its listener, so large rootfs files can delay readiness well after the fork.
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

/// Core of [`confirm_active`]. Inject `settle` and `(attempts, interval)` so tests
/// can reduce the delays used by the real service path to nearly zero.
fn confirm_active_with(
    privileged: &Privileged<'_>,
    probe: &dyn Fn() -> bool,
    settle: Duration,
    attempts: u32,
    interval: Duration,
) -> Result<(), Error> {
    std::thread::sleep(settle);
    let mut failed = Vec::new();
    for unit in control_units(privileged) {
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

/// Start in boot order, then verify the services are actually running.
pub fn start(privileged: &Privileged<'_>, probe: &dyn Fn() -> bool) -> Result<(), Error> {
    let mut args = vec!["start"];
    args.extend(control_units(privileged));
    privileged.run_ok(STEP_START, "systemctl", &args)?;
    confirm_active(privileged, probe)
}

/// install.sh `start_units`: enable, restart, and verify.
pub fn enable_and_start(
    privileged: &Privileged<'_>,
    probe: &dyn Fn() -> bool,
) -> Result<(), Error> {
    enable(privileged)?;
    let mut args = vec!["restart"];
    args.extend(control_units(privileged));
    privileged.run_ok(STEP_START, "systemctl", &args)?;
    confirm_active(privileged, probe)
}

/// Stop in reverse order so the API shuts down before the helper.
pub fn stop(privileged: &Privileged<'_>) -> Result<(), Error> {
    let mut args = vec!["stop"];
    args.extend(control_units(privileged).into_iter().rev());
    privileged.run_ok(STEP_STOP, "systemctl", &args)?;
    for unit in control_units(privileged) {
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
        std::fs::rename(units_dir.join(UNITS[0]), units_dir.join(LEGACY_HELPER_UNIT)).unwrap();
        std::fs::create_dir_all(&env.unitdir).unwrap();
        std::fs::write(env.unit_path(LEGACY_HELPER_UNIT), b"[Unit]\n").unwrap();
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
        let stop = calls
            .iter()
            .position(|call| call == "sudo systemctl disable --now firecrab-net-helper.service")
            .unwrap();
        let alias = calls
            .iter()
            .position(|call| call.starts_with("sudo ln -sfn firecrab-helper.service "))
            .unwrap();
        let reload = calls
            .iter()
            .position(|call| call == "sudo systemctl daemon-reload")
            .unwrap();
        assert!(stop < alias && alias < reload, "{calls:?}");
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
                .any(|c| c == "sudo systemctl start firecrab-helper.service firecrab-api.service"),
            "{calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|c| c == "sudo systemctl stop firecrab-api.service firecrab-helper.service"),
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
            &["is-active", "firecrab-helper.service"],
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
            fake.calls()
                .into_iter()
                .filter(|call| call.starts_with("sudo "))
                .collect::<Vec<_>>(),
            vec![
                "sudo systemctl enable firecrab-helper.service firecrab-api.service",
                "sudo systemctl disable firecrab-helper.service firecrab-api.service",
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
        assert_eq!(is_active(&fake, "firecrab-helper.service"), "unknown");
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
    #[test]
    fn legacy_service_control_works_but_a_loaded_canonical_unit_is_never_hidden() {
        let mut fake = FakeCommandRunner::permissive();
        fake.set(
            "systemctl",
            &["show", "--property=LoadState", "--value", UNITS[0]],
            0,
            "not-found\n",
            "",
        );
        fake.set(
            "systemctl",
            &[
                "show",
                "--property=LoadState",
                "--value",
                LEGACY_HELPER_UNIT,
            ],
            0,
            "loaded\n",
            "",
        );
        assert_eq!(helper_unit(&fake), LEGACY_HELPER_UNIT);
        enable(&Privileged::with_sudo(&fake, true)).unwrap();
        assert!(
            fake.calls().iter().any(|call| call
                == "sudo systemctl enable firecrab-net-helper.service firecrab-api.service")
        );
        fake.set(
            "systemctl",
            &["show", "--property=LoadState", "--value", UNITS[0]],
            0,
            "loaded\n",
            "",
        );
        assert_eq!(helper_unit(&fake), UNITS[0]);
        let dir = tempfile::tempdir().unwrap();
        let env = env_for(dir.path());
        std::fs::create_dir_all(&env.unitdir).unwrap();
        for name in [UNITS[1], LEGACY_HELPER_UNIT] {
            std::fs::write(env.unit_path(name), b"[Unit]\n").unwrap();
        }
        require_installed(&env).unwrap();
    }
}

use super::*;
use crate::shell::FakeCommandRunner;
use std::os::unix::fs::symlink;

#[test]
fn secret_replacement_is_private_and_removal_is_idempotent() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("credentials.json");
    write_atomic(&path, Some(b"previous"), 0o600).unwrap();
    let change = FileChange::new(path.clone(), Some(b"replacement".to_vec()), 0o600).unwrap();
    assert!(change.changed());
    write_atomic(&path, change.next.as_deref(), change.mode).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"replacement");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    write_atomic(&path, change.previous.as_deref(), change.mode).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"previous");
    assert!(
        !FileChange::new(path.clone(), Some(b"previous".to_vec()), 0o600)
            .unwrap()
            .changed()
    );
    write_atomic(&path, None, 0o600).unwrap();
    write_atomic(&path, None, 0o600).unwrap();
    assert!(!path.exists());
}

#[test]
fn rejects_symlink_and_directory_destinations_without_changing_the_target() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("target");
    let link = directory.path().join("link");
    fs::write(&target, b"untouched").unwrap();
    symlink(&target, &link).unwrap();
    assert!(FileChange::new(link, Some(b"replacement".to_vec()), 0o600).is_err());
    assert!(FileChange::new(directory.path().to_owned(), None, 0o600).is_err());
    assert_eq!(fs::read(target).unwrap(), b"untouched");
}

#[test]
fn rejects_paths_that_can_change_unit_syntax_and_non_executable_engines() {
    for path in [
        "relative/nginx",
        "/tmp/a b",
        "/tmp/a%h",
        "/tmp/a;exec",
        "/tmp/a\nExecStart=bad",
    ] {
        assert!(unit_path(Path::new(path)).is_err(), "{path}");
    }
    assert_eq!(
        unit_path(Path::new("/opt/firecrab/nginx-1.2")).unwrap(),
        "/opt/firecrab/nginx-1.2"
    );
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("nginx");
    fs::write(&path, b"engine").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        executable(&path)
            .unwrap_err()
            .to_string()
            .contains("not an executable")
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(executable(&path).unwrap(), fs::canonicalize(path).unwrap());
    assert!(executable(directory.path()).is_err());
}

#[test]
fn command_failures_preserve_recovery_details_and_failed_units_are_inactive() {
    let mut runner = FakeCommandRunner::new();
    runner.set("nginx", &["-t"], 1, "", "invalid native config");
    runner.set(
        "systemctl",
        &["is-active", "--quiet", INGRESS_UNIT],
        1,
        "",
        "",
    );
    assert!(
        command(&runner, "nginx", &["-t"])
            .unwrap_err()
            .to_string()
            .contains("invalid native config")
    );
    assert!(!unit_state(&runner, "is-active", INGRESS_UNIT));
}

fn configured_services() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let pipe = directory.path().join("pipe");
    fs::create_dir_all(pipe.join("secrets")).unwrap();
    fs::write(pipe.join("manifest.json"), serde_json::to_vec(&serde_json::json!({
        "listen": "127.0.0.1:8080",
        "routes": [{ "host": "app.example.com", "upstreams": ["127.0.0.1:9000"] }],
        "tunnel": { "id": "11111111-1111-4111-8111-111111111111", "credentialsFile": "secrets/tunnel-credentials.json" }
    })).unwrap()).unwrap();
    fs::write(pipe.join("ingress.conf"), b"unchanged-nginx-config").unwrap();
    fs::write(pipe.join("tunnel.json"), b"unchanged-tunnel-config").unwrap();
    fs::write(
        pipe.join("secrets/tunnel-credentials.json"),
        b"unchanged-private-credentials",
    )
    .unwrap();
    directory
}

fn unit_runner(unit: &str, load_state: &str) -> FakeCommandRunner {
    let mut runner = FakeCommandRunner::new();
    runner.set(
        "systemctl",
        &["show", "--property=LoadState", "--value", unit],
        if load_state == "loaded" { 0 } else { 1 },
        load_state,
        "",
    );
    runner
}

#[test]
fn service_jobs_target_one_fixed_unit_without_rewriting_files_or_enablement() {
    let directory = configured_services();
    let files = [
        "manifest.json",
        "ingress.conf",
        "tunnel.json",
        "secrets/tunnel-credentials.json",
    ];
    let previous = files.map(|file| fs::read(directory.path().join("pipe").join(file)).unwrap());
    for (service, unit) in [
        (Service::Ingress, INGRESS_UNIT),
        (Service::Tunnel, TUNNEL_UNIT),
    ] {
        for (action, verb) in [
            (ServiceAction::Start, "start"),
            (ServiceAction::Stop, "stop"),
            (ServiceAction::Restart, "restart"),
        ] {
            let mut runner = unit_runner(unit, "loaded");
            runner.set("systemctl", &["--no-block", verb, unit], 0, "", "");
            control_with(
                &runner,
                directory.path(),
                &ServiceRequest { service, action },
            )
            .unwrap();
            assert_eq!(
                runner.calls(),
                [
                    format!("systemctl show --property=LoadState --value {unit}"),
                    format!("systemctl --no-block {verb} {unit}"),
                ]
            );
        }
    }
    for (file, expected) in files.into_iter().zip(previous) {
        assert_eq!(
            fs::read(directory.path().join("pipe").join(file)).unwrap(),
            expected
        );
    }
}

#[test]
fn starting_and_restarting_require_a_saved_manifest_and_loaded_unit() {
    for action in [ServiceAction::Start, ServiceAction::Restart] {
        let empty = tempfile::tempdir().unwrap();
        let runner = FakeCommandRunner::new();
        let error = control_with(
            &runner,
            empty.path(),
            &ServiceRequest {
                service: Service::Ingress,
                action,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("not configured"));
        assert!(runner.calls().is_empty());

        let directory = configured_services();
        for service in [Service::Ingress, Service::Tunnel] {
            let unit = if service == Service::Ingress {
                INGRESS_UNIT
            } else {
                TUNNEL_UNIT
            };
            let runner = unit_runner(unit, "not-found");
            let error = control_with(
                &runner,
                directory.path(),
                &ServiceRequest { service, action },
            )
            .unwrap_err();
            assert!(error.to_string().contains("unit is not loaded"));
            assert_eq!(
                runner.calls(),
                [format!(
                    "systemctl show --property=LoadState --value {unit}"
                )]
            );
        }
    }
}

#[test]
fn tunnel_jobs_require_tunnel_configuration_and_installed_secret_files() {
    let directory = configured_services();
    let manifest = directory.path().join("pipe/manifest.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    let original = config.clone();
    config["tunnel"] = serde_json::Value::Null;
    fs::write(&manifest, serde_json::to_vec(&config).unwrap()).unwrap();
    let runner = FakeCommandRunner::new();
    let error = control_with(
        &runner,
        directory.path(),
        &ServiceRequest {
            service: Service::Tunnel,
            action: ServiceAction::Start,
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Cloudflare Tunnel is not configured")
    );
    assert!(runner.calls().is_empty());

    fs::write(&manifest, serde_json::to_vec(&original).unwrap()).unwrap();
    fs::remove_file(
        directory
            .path()
            .join("pipe/secrets/tunnel-credentials.json"),
    )
    .unwrap();
    let error = control_with(
        &runner,
        directory.path(),
        &ServiceRequest {
            service: Service::Tunnel,
            action: ServiceAction::Restart,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("configuration is incomplete"));
    assert!(runner.calls().is_empty());
}

#[test]
fn ingress_jobs_require_the_installed_native_configuration() {
    let directory = configured_services();
    fs::remove_file(directory.path().join("pipe/ingress.conf")).unwrap();
    let runner = FakeCommandRunner::new();
    let error = control_with(
        &runner,
        directory.path(),
        &ServiceRequest {
            service: Service::Ingress,
            action: ServiceAction::Start,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("configuration is incomplete"));
    assert!(runner.calls().is_empty());
}

#[test]
fn stopping_ignores_broken_configuration_and_missing_units_are_already_stopped() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("pipe")).unwrap();
    fs::write(
        directory.path().join("pipe/manifest.json"),
        b"broken manifest",
    )
    .unwrap();
    for (service, unit) in [
        (Service::Ingress, INGRESS_UNIT),
        (Service::Tunnel, TUNNEL_UNIT),
    ] {
        let request = ServiceRequest {
            service,
            action: ServiceAction::Stop,
        };
        let mut runner = unit_runner(unit, "loaded");
        runner.set("systemctl", &["--no-block", "stop", unit], 0, "", "");
        control_with(&runner, directory.path(), &request).unwrap();
        assert_eq!(runner.calls().len(), 2);

        let runner = unit_runner(unit, "not-found");
        control_with(&runner, directory.path(), &request).unwrap();
        assert_eq!(
            runner.calls(),
            [format!(
                "systemctl show --property=LoadState --value {unit}"
            )]
        );
    }
    assert_eq!(
        fs::read(directory.path().join("pipe/manifest.json")).unwrap(),
        b"broken manifest"
    );
}

#[test]
fn service_job_failures_keep_systemd_recovery_details() {
    let directory = configured_services();
    let mut runner = unit_runner(INGRESS_UNIT, "loaded");
    runner.set(
        "systemctl",
        &["--no-block", "restart", INGRESS_UNIT],
        1,
        "",
        "unit is masked",
    );
    let error = control_with(
        &runner,
        directory.path(),
        &ServiceRequest {
            service: Service::Ingress,
            action: ServiceAction::Restart,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("unit is masked"));
    assert_eq!(runner.calls().len(), 2);
}

fn quick_config() -> Config {
    serde_json::from_value(serde_json::json!({
        "listen": "127.0.0.1:8080", "routes": [], "tunnel": null,
        "quickTunnel": { "upstreams": ["127.0.0.1:9000", "127.0.0.1:9001"] }
    }))
    .unwrap()
}

fn transaction_runner(active: bool) -> FakeCommandRunner {
    let mut runner = FakeCommandRunner::permissive();
    for unit in [INGRESS_UNIT, TUNNEL_UNIT] {
        for verb in ["is-active", "is-enabled"] {
            runner.set(
                "systemctl",
                &[verb, "--quiet", unit],
                if active { 0 } else { 1 },
                "",
                "",
            );
        }
    }
    runner
}

fn apply_fixture(
    runner: &dyn CommandRunner,
    directory: &Path,
    config: &Config,
    verify: impl FnOnce() -> Result<()>,
) -> Result<()> {
    apply_with(
        runner,
        (&directory.join("config"), &directory.join("units")),
        config,
        (
            Path::new("/usr/sbin/nginx"),
            Path::new("/opt/firecrab/cloudflared"),
        ),
        ApplyMode::Configuration,
        |_| Ok(()),
        verify,
    )
}

fn named_config(directory: &Path) -> Config {
    let credentials = directory.join("source-credentials.json");
    fs::write(
        &credentials,
        serde_json::to_vec(&serde_json::json!({
            "AccountTag": "fixture", "TunnelSecret": "private-fixture-secret",
            "TunnelID": "11111111-1111-4111-8111-111111111111"
        }))
        .unwrap(),
    )
    .unwrap();
    serde_json::from_value(serde_json::json!({
        "routes": [{ "host": "app.example.com", "upstreams": ["127.0.0.1:9000"] }],
        "tunnel": { "id": "11111111-1111-4111-8111-111111111111", "credentialsFile": credentials }
    }))
    .unwrap()
}

#[test]
fn quick_tunnel_uses_its_own_nonsecret_config_and_independent_unit() {
    let directory = tempfile::tempdir().unwrap();
    let config = quick_config();
    let runner = transaction_runner(false);
    apply_fixture(&runner, directory.path(), &config, || Ok(())).unwrap();
    let pipe = directory.path().join("config/pipe");
    let unit = fs::read_to_string(directory.path().join("units").join(TUNNEL_UNIT)).unwrap();
    assert!(unit.contains("Type=simple"));
    assert!(unit.contains("RestartSec=60\nRestartSteps=4\nRestartMaxDelaySec=15min"));
    assert!(
        unit.contains("--url http://127.0.0.1:8080 --http-host-header quick.firecrab.internal")
    );
    assert!(unit.contains("--config %d/quick-tunnel.json"));
    assert!(unit.contains("LoadCredential=quick-tunnel.json:"));
    assert!(!unit.contains("tunnel-credentials.json"));
    assert!(!unit.contains("firecrab-api"));
    assert_eq!(fs::read(pipe.join("quick-tunnel.json")).unwrap(), b"{}\n");
    assert_eq!(
        fs::metadata(pipe.join("quick-tunnel.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(!pipe.join("tunnel.json").exists());
    assert!(!pipe.join("secrets/tunnel-credentials.json").exists());
    let saved = Config::read(&pipe.join("manifest.json")).unwrap();
    assert!(saved.routes.is_empty());
    assert_eq!(saved.quick_tunnel, config.quick_tunnel);
    assert!(
        !fs::read_to_string(pipe.join("manifest.json"))
            .unwrap()
            .contains("trycloudflare.com")
    );
    assert!(
        runner
            .calls()
            .contains(&format!("systemctl start {TUNNEL_UNIT}"))
    );
    assert!(
        !runner
            .calls()
            .iter()
            .any(|call| call.starts_with("/opt/firecrab/cloudflared "))
    );
}

#[test]
fn changing_quick_apps_keeps_the_connector_and_its_current_address() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = quick_config();
    apply_fixture(
        &transaction_runner(false),
        directory.path(),
        &config,
        || Ok(()),
    )
    .unwrap();
    let previous_unit = fs::read(directory.path().join("units").join(TUNNEL_UNIT)).unwrap();
    config.quick_tunnel.as_mut().unwrap().upstreams = vec!["127.0.0.1:9010".parse().unwrap()];
    let runner = transaction_runner(true);
    apply_fixture(&runner, directory.path(), &config, || Ok(())).unwrap();
    assert_eq!(
        fs::read(directory.path().join("units").join(TUNNEL_UNIT)).unwrap(),
        previous_unit
    );
    assert!(
        !runner
            .calls()
            .iter()
            .any(|call| call == &format!("systemctl restart {TUNNEL_UNIT}")
                || call == &format!("systemctl start {TUNNEL_UNIT}"))
    );
    assert!(
        fs::read_to_string(directory.path().join("config/pipe/ingress.conf"))
            .unwrap()
            .contains("127.0.0.1:9010")
    );
}

#[test]
fn named_and_quick_modes_replace_only_their_local_connector_files() {
    let directory = tempfile::tempdir().unwrap();
    let named = named_config(directory.path());
    apply_fixture(&transaction_runner(false), directory.path(), &named, || {
        Ok(())
    })
    .unwrap();
    let mut quick = named.clone();
    quick.tunnel = None;
    quick.quick_tunnel = quick_config().quick_tunnel;
    let runner = transaction_runner(true);
    apply_fixture(&runner, directory.path(), &quick, || Ok(())).unwrap();
    let pipe = directory.path().join("config/pipe");
    assert!(pipe.join("quick-tunnel.json").exists());
    assert!(!pipe.join("tunnel.json").exists());
    assert!(!pipe.join("secrets/tunnel-credentials.json").exists());
    assert!(
        runner
            .calls()
            .contains(&format!("systemctl restart {TUNNEL_UNIT}"))
    );
    assert!(
        !runner
            .calls()
            .contains(&format!("systemctl disable --now {TUNNEL_UNIT}"))
    );
    assert_eq!(
        Config::read(&pipe.join("manifest.json")).unwrap().routes,
        named.routes
    );

    let runner = transaction_runner(true);
    apply_fixture(&runner, directory.path(), &named, || Ok(())).unwrap();
    assert!(!pipe.join("quick-tunnel.json").exists());
    assert!(pipe.join("tunnel.json").exists());
    assert!(pipe.join("secrets/tunnel-credentials.json").exists());
    assert!(
        fs::read_to_string(directory.path().join("units").join(TUNNEL_UNIT))
            .unwrap()
            .contains("Type=notify")
    );
    assert!(
        runner
            .calls()
            .contains(&format!("systemctl restart {TUNNEL_UNIT}"))
    );
}

#[test]
fn a_failed_mode_transition_restores_the_named_files_and_running_state() {
    let directory = tempfile::tempdir().unwrap();
    let named = named_config(directory.path());
    apply_fixture(&transaction_runner(false), directory.path(), &named, || {
        Ok(())
    })
    .unwrap();
    let files = [
        "config/pipe/manifest.json",
        "config/pipe/ingress.conf",
        "config/pipe/tunnel.json",
        "config/pipe/secrets/tunnel-credentials.json",
        "units/firecrab-tunnel.service",
    ];
    let previous = files.map(|file| fs::read(directory.path().join(file)).unwrap());
    let mut quick = named.clone();
    quick.tunnel = None;
    quick.quick_tunnel = quick_config().quick_tunnel;
    let runner = transaction_runner(true);
    let error = apply_fixture(&runner, directory.path(), &quick, || {
        Err("verification failed".into())
    })
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("previous pipe configuration restored")
    );
    for (file, expected) in files.into_iter().zip(previous) {
        assert_eq!(
            fs::read(directory.path().join(file)).unwrap(),
            expected,
            "{file}"
        );
    }
    assert!(
        !directory
            .path()
            .join("config/pipe/quick-tunnel.json")
            .exists()
    );
    assert_eq!(
        runner
            .calls()
            .iter()
            .filter(|call| **call == format!("systemctl restart {TUNNEL_UNIT}"))
            .count(),
        2
    );
}

#[test]
fn quick_service_jobs_need_no_named_credentials_but_require_their_isolated_config() {
    let directory = tempfile::tempdir().unwrap();
    let config = quick_config();
    apply_fixture(
        &transaction_runner(false),
        directory.path(),
        &config,
        || Ok(()),
    )
    .unwrap();
    for (action, verb) in [
        (ServiceAction::Start, "start"),
        (ServiceAction::Restart, "restart"),
    ] {
        let mut runner = unit_runner(TUNNEL_UNIT, "loaded");
        runner.set("systemctl", &["--no-block", verb, TUNNEL_UNIT], 0, "", "");
        control_with(
            &runner,
            &directory.path().join("config"),
            &ServiceRequest {
                service: Service::Tunnel,
                action,
            },
        )
        .unwrap();
        assert_eq!(runner.calls().len(), 2);
    }
    fs::remove_file(directory.path().join("config/pipe/quick-tunnel.json")).unwrap();
    let runner = FakeCommandRunner::new();
    let error = control_with(
        &runner,
        &directory.path().join("config"),
        &ServiceRequest {
            service: Service::Tunnel,
            action: ServiceAction::Start,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("configuration is incomplete"));
    assert!(runner.calls().is_empty());
}

fn multiple_config() -> Config {
    serde_json::from_value(serde_json::json!({
        "routes": [{ "host": "pipe.test", "upstreams": ["127.0.0.1:9000"] }],
        "quickTunnels": [
            { "id": "00000000-0000-0000-0000-00000000000b", "name": "apps", "target": { "type": "apps", "upstreams": ["127.0.0.1:9001"] } },
            { "id": "00000000-0000-0000-0000-000000000016", "name": "routes", "target": { "type": "routes", "host": "pipe.test" } }
        ]
    })).unwrap()
}

fn multiple_runner(
    config: &Config,
    ingress_active: bool,
    active: &[uuid::Uuid],
) -> FakeCommandRunner {
    let mut runner = transaction_runner(ingress_active);
    runner.set(
        "systemctl",
        &["is-active", "--quiet", TUNNEL_UNIT],
        1,
        "",
        "",
    );
    runner.set(
        "systemctl",
        &["is-enabled", "--quiet", TUNNEL_UNIT],
        1,
        "",
        "",
    );
    for entry in &config.quick_tunnels {
        let unit = quick_tunnel_unit(entry.id);
        runner.set(
            "systemctl",
            &["is-active", "--quiet", &unit],
            if active.contains(&entry.id) { 0 } else { 1 },
            "",
            "",
        );
        runner.set("systemctl", &["is-enabled", "--quiet", &unit], 0, "", "");
    }
    runner
}

fn manage_fixture(
    runner: &dyn CommandRunner,
    directory: &Path,
    config: &Config,
    verify: impl FnOnce() -> Result<()>,
) -> Result<()> {
    apply_with(
        runner,
        (&directory.join("config"), &directory.join("units")),
        config,
        (
            Path::new("/usr/sbin/nginx"),
            Path::new("/opt/firecrab/cloudflared"),
        ),
        ApplyMode::QuickTunnel,
        |_| Ok(()),
        verify,
    )
}

fn saved_files(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    fn collect(
        root: &Path,
        path: &Path,
        output: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
    ) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect(root, &path, output);
            } else {
                output.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(&path).unwrap(),
                );
            }
        }
    }
    let mut output = std::collections::BTreeMap::new();
    collect(root, root, &mut output);
    output
}

#[test]
fn two_connectors_install_distinct_units_and_reserved_hosts() {
    let directory = tempfile::tempdir().unwrap();
    let config = multiple_config();
    let runner = multiple_runner(&config, false, &[]);
    manage_fixture(&runner, directory.path(), &config, || Ok(())).unwrap();
    for entry in &config.quick_tunnels {
        let unit = quick_tunnel_unit(entry.id);
        let text = fs::read_to_string(directory.path().join("units").join(&unit)).unwrap();
        assert!(text.contains(&format!(
            "--http-host-header {}",
            quick_tunnel_host(entry.id)
        )));
        assert!(runner.calls().contains(&format!("systemctl start {unit}")));
        assert!(runner.calls().contains(&format!("systemctl enable {unit}")));
    }
    assert!(!directory.path().join("units").join(TUNNEL_UNIT).exists());
    assert_eq!(
        fs::read(directory.path().join("config/pipe/quick-tunnel.json")).unwrap(),
        b"{}\n"
    );
    assert_eq!(
        Config::read(&directory.path().join("config/pipe/manifest.json")).unwrap(),
        config
    );
}

#[test]
fn editing_apps_or_names_does_not_restart_connectors_or_resume_a_stopped_peer() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = multiple_config();
    manage_fixture(
        &multiple_runner(&config, false, &[]),
        directory.path(),
        &config,
        || Ok(()),
    )
    .unwrap();
    let first = config.quick_tunnels[0].id;
    let second = config.quick_tunnels[1].id;
    let before = fs::read(
        directory
            .path()
            .join("units")
            .join(quick_tunnel_unit(first)),
    )
    .unwrap();
    config.quick_tunnels[0].target = QuickTunnelTarget::Apps {
        upstreams: vec!["127.0.0.1:9100".parse().unwrap()],
    };
    config.quick_tunnels[1].name = "renamed".into();
    let runner = multiple_runner(&config, true, &[first]);
    manage_fixture(&runner, directory.path(), &config, || Ok(())).unwrap();
    assert_eq!(
        fs::read(
            directory
                .path()
                .join("units")
                .join(quick_tunnel_unit(first))
        )
        .unwrap(),
        before
    );
    for id in [first, second] {
        for action in ["start", "restart", "enable"] {
            assert!(
                !runner
                    .calls()
                    .contains(&format!("systemctl {action} {}", quick_tunnel_unit(id)))
            );
        }
    }
}

#[test]
fn listener_changes_restart_active_connectors_and_keep_stopped_connectors_stopped() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = multiple_config();
    manage_fixture(
        &multiple_runner(&config, false, &[]),
        directory.path(),
        &config,
        || Ok(()),
    )
    .unwrap();
    let first = config.quick_tunnels[0].id;
    let second = config.quick_tunnels[1].id;
    config.listen = "127.0.0.1:8081".parse().unwrap();
    let runner = multiple_runner(&config, true, &[first]);
    manage_fixture(&runner, directory.path(), &config, || Ok(())).unwrap();
    assert!(
        runner
            .calls()
            .contains(&format!("systemctl restart {}", quick_tunnel_unit(first)))
    );
    assert!(
        !runner
            .calls()
            .contains(&format!("systemctl restart {}", quick_tunnel_unit(second)))
    );
    assert!(
        !runner
            .calls()
            .contains(&format!("systemctl start {}", quick_tunnel_unit(second)))
    );
    for entry in config.quick_tunnels {
        assert!(
            fs::read_to_string(
                directory
                    .path()
                    .join("units")
                    .join(quick_tunnel_unit(entry.id))
            )
            .unwrap()
            .contains("--url http://127.0.0.1:8081")
        );
    }
}

#[test]
fn deleting_one_connector_removes_only_its_unit_and_internal_routes() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = multiple_config();
    manage_fixture(
        &multiple_runner(&config, false, &[]),
        directory.path(),
        &config,
        || Ok(()),
    )
    .unwrap();
    let first = config.quick_tunnels[0].id;
    let second = config.quick_tunnels[1].id;
    let runner = multiple_runner(&config, true, &[first, second]);
    config.quick_tunnels.remove(0);
    manage_fixture(&runner, directory.path(), &config, || Ok(())).unwrap();
    assert!(runner.calls().contains(&format!(
        "systemctl disable --now {}",
        quick_tunnel_unit(first)
    )));
    assert!(
        !directory
            .path()
            .join("units")
            .join(quick_tunnel_unit(first))
            .exists()
    );
    assert!(
        directory
            .path()
            .join("units")
            .join(quick_tunnel_unit(second))
            .is_file()
    );
    for action in ["start", "restart", "stop"] {
        assert!(
            !runner
                .calls()
                .contains(&format!("systemctl {action} {}", quick_tunnel_unit(second)))
        );
    }
    let nginx = fs::read_to_string(directory.path().join("config/pipe/ingress.conf")).unwrap();
    assert!(!nginx.contains(&quick_tunnel_host(first)));
    assert!(nginx.contains(&quick_tunnel_host(second)));
    assert!(nginx.contains("server_name pipe.test"));
}

#[test]
fn a_failed_new_connector_restores_files_and_leaves_existing_peers_untouched() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = multiple_config();
    manage_fixture(
        &multiple_runner(&config, false, &[]),
        directory.path(),
        &config,
        || Ok(()),
    )
    .unwrap();
    let first = config.quick_tunnels[0].id;
    let second = config.quick_tunnels[1].id;
    let before = saved_files(directory.path());
    let third = uuid::Uuid::from_u128(33);
    config
        .quick_tunnels
        .push(firecrab_helper_protocol::pipe::QuickTunnelEntry {
            id: third,
            name: "third".into(),
            target: QuickTunnelTarget::Apps {
                upstreams: vec!["127.0.0.1:9200".parse().unwrap()],
            },
        });
    let mut runner = multiple_runner(&config, true, &[first]);
    runner.set(
        "systemctl",
        &["start", &quick_tunnel_unit(third)],
        1,
        "",
        "simulated start failure",
    );
    let error = manage_fixture(&runner, directory.path(), &config, || Ok(())).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("previous pipe configuration restored")
    );
    assert_eq!(saved_files(directory.path()), before);
    assert!(
        runner
            .calls()
            .contains(&format!("systemctl stop {}", quick_tunnel_unit(third)))
    );
    for id in [first, second] {
        assert!(
            !runner
                .calls()
                .contains(&format!("systemctl restart {}", quick_tunnel_unit(id)))
        );
    }
}

#[test]
fn failed_deletion_restores_the_deleted_unit_before_resuming_it() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = multiple_config();
    manage_fixture(
        &multiple_runner(&config, false, &[]),
        directory.path(),
        &config,
        || Ok(()),
    )
    .unwrap();
    let first = config.quick_tunnels[0].id;
    let second = config.quick_tunnels[1].id;
    let before = saved_files(directory.path());
    let runner = multiple_runner(&config, true, &[first, second]);
    config.quick_tunnels.remove(0);
    let error = manage_fixture(&runner, directory.path(), &config, || {
        Err("verification failed".into())
    })
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("previous pipe configuration restored")
    );
    assert_eq!(saved_files(directory.path()), before);
    assert!(
        runner
            .calls()
            .contains(&format!("systemctl restart {}", quick_tunnel_unit(first)))
    );
    assert!(
        !runner
            .calls()
            .contains(&format!("systemctl restart {}", quick_tunnel_unit(second)))
    );
}

#[test]
fn per_connector_controls_cannot_change_another_units_state_or_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let config = multiple_config();
    manage_fixture(
        &multiple_runner(&config, false, &[]),
        directory.path(),
        &config,
        || Ok(()),
    )
    .unwrap();
    let before = saved_files(directory.path());
    let id = config.quick_tunnels[1].id;
    let unit = quick_tunnel_unit(id);
    for (action, verb) in [
        (ServiceAction::Start, "start"),
        (ServiceAction::Stop, "stop"),
        (ServiceAction::Restart, "restart"),
    ] {
        let mut runner = unit_runner(&unit, "loaded");
        runner.set("systemctl", &["--no-block", verb, &unit], 0, "", "");
        control_quick_with(
            &runner,
            &directory.path().join("config"),
            &config,
            id,
            action,
        )
        .unwrap();
        assert_eq!(
            runner.calls(),
            [
                format!("systemctl show --property=LoadState --value {unit}"),
                format!("systemctl --no-block {verb} {unit}")
            ]
        );
    }
    let runner = FakeCommandRunner::new();
    assert!(
        control_quick_with(
            &runner,
            &directory.path().join("config"),
            &config,
            uuid::Uuid::from_u128(99),
            ServiceAction::Stop
        )
        .is_err()
    );
    assert!(runner.calls().is_empty());
    assert_eq!(saved_files(directory.path()), before);
}

#[test]
fn first_legacy_edit_replaces_only_its_shared_unit() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = multiple_config();
    config.quick_tunnel = quick_config().quick_tunnel;
    let mut initial = multiple_runner(&config, false, &[]);
    initial.set(
        "systemctl",
        &["is-enabled", "--quiet", TUNNEL_UNIT],
        1,
        "",
        "",
    );
    manage_fixture(&initial, directory.path(), &config, || Ok(())).unwrap();
    let peers = config
        .quick_tunnels
        .iter()
        .map(|entry| entry.id)
        .collect::<Vec<_>>();
    let mut runner = multiple_runner(&config, true, &peers);
    runner.set(
        "systemctl",
        &["is-active", "--quiet", TUNNEL_UNIT],
        0,
        "",
        "",
    );
    runner.set(
        "systemctl",
        &[
            "is-active",
            "--quiet",
            &quick_tunnel_unit(LEGACY_QUICK_TUNNEL_ID),
        ],
        1,
        "",
        "",
    );
    config.quick_tunnel = None;
    config
        .quick_tunnels
        .push(firecrab_helper_protocol::pipe::QuickTunnelEntry {
            id: LEGACY_QUICK_TUNNEL_ID,
            name: "migrated".into(),
            target: QuickTunnelTarget::Apps {
                upstreams: vec!["127.0.0.1:9300".parse().unwrap()],
            },
        });
    manage_fixture(&runner, directory.path(), &config, || Ok(())).unwrap();
    assert!(
        runner
            .calls()
            .contains(&format!("systemctl disable --now {TUNNEL_UNIT}"))
    );
    assert!(runner.calls().contains(&format!(
        "systemctl start {}",
        quick_tunnel_unit(LEGACY_QUICK_TUNNEL_ID)
    )));
    for id in peers {
        assert!(
            !runner
                .calls()
                .contains(&format!("systemctl restart {}", quick_tunnel_unit(id)))
        );
    }
    assert!(!directory.path().join("units").join(TUNNEL_UNIT).exists());
}

#[test]
fn rollback_restores_a_stopped_nginx_after_resuming_a_deleted_connector() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = multiple_config();
    manage_fixture(
        &multiple_runner(&config, false, &[]),
        directory.path(),
        &config,
        || Ok(()),
    )
    .unwrap();
    let first = config.quick_tunnels[0].id;
    let mut runner = multiple_runner(&config, false, &[first]);
    runner.set_seq(
        "systemctl",
        &["daemon-reload"],
        &[(1, "", "failed to reload"), (0, "", "")],
    );
    config.quick_tunnels.remove(0);
    let error = manage_fixture(&runner, directory.path(), &config, || Ok(())).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("previous pipe configuration restored")
    );
    let calls = runner.calls();
    let restart = calls
        .iter()
        .position(|call| call == &format!("systemctl restart {}", quick_tunnel_unit(first)))
        .unwrap();
    let stop_ingress = calls
        .iter()
        .rposition(|call| call == &format!("systemctl stop {INGRESS_UNIT}"))
        .unwrap();
    assert!(restart < stop_ingress);
    assert!(!calls.contains(&format!("systemctl start {INGRESS_UNIT}")));
}

#[test]
fn disable_recovers_from_broken_manifest_and_stops_only_managed_names() {
    let root = tempfile::tempdir().unwrap();
    let confdir = root.path().join("config");
    let unitdir = root.path().join("units");
    fs::create_dir_all(confdir.join("pipe")).unwrap();
    fs::create_dir_all(&unitdir).unwrap();
    fs::write(confdir.join("pipe/manifest.json"), b"invalid json").unwrap();
    let unit = quick_tunnel_unit(uuid::Uuid::from_u128(2));
    fs::write(unitdir.join(&unit), b"fixture").unwrap();
    fs::write(
        unitdir.join("firecrab-quick-tunnel-not-a-uuid.service"),
        b"untouched",
    )
    .unwrap();
    let mut runner = FakeCommandRunner::new();
    for name in [&unit, TUNNEL_UNIT, INGRESS_UNIT] {
        runner.set(
            "systemctl",
            &["show", "--property=LoadState", "--value", name],
            0,
            "loaded",
            "",
        );
        runner.set("systemctl", &["disable", "--now", name], 0, "", "");
    }
    disable_with(&runner, &confdir, &unitdir).unwrap();
    let calls = runner.calls();
    for name in [&unit, TUNNEL_UNIT, INGRESS_UNIT] {
        assert!(calls.contains(&format!("systemctl disable --now {name}")));
    }
    assert!(!calls.iter().any(|call| call.contains("not-a-uuid")));
    assert_eq!(
        fs::read(confdir.join("pipe/manifest.json")).unwrap(),
        b"invalid json"
    );
}

#[test]
fn named_connect_and_disconnect_keep_independent_quick_connectors_running() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = multiple_config();
    manage_fixture(
        &multiple_runner(&config, false, &[]),
        directory.path(),
        &config,
        || Ok(()),
    )
    .unwrap();
    let peers = config
        .quick_tunnels
        .iter()
        .map(|entry| entry.id)
        .collect::<Vec<_>>();
    let credentials = directory.path().join("named-credentials.json");
    fs::write(&credentials, br#"{"TunnelID":"00000000-0000-0000-0000-000000000009","AccountTag":"account","TunnelSecret":"secret"}"#).unwrap();
    config.tunnel = Some(firecrab_helper_protocol::pipe::Tunnel {
        id: uuid::Uuid::from_u128(9),
        credentials_file: credentials,
        hostname: Some("pipe.test".into()),
    });
    let runner = multiple_runner(&config, true, &peers);
    manage_fixture(&runner, directory.path(), &config, || Ok(())).unwrap();
    assert!(
        runner
            .calls()
            .contains(&format!("systemctl start {TUNNEL_UNIT}"))
    );
    for id in &peers {
        assert!(
            !runner
                .calls()
                .iter()
                .any(|call| call == &format!("systemctl restart {}", quick_tunnel_unit(*id)))
        );
    }
    let mut runner = multiple_runner(&config, true, &peers);
    runner.set(
        "systemctl",
        &["is-active", "--quiet", TUNNEL_UNIT],
        0,
        "",
        "",
    );
    config.tunnel = None;
    manage_fixture(&runner, directory.path(), &config, || Ok(())).unwrap();
    assert!(
        runner
            .calls()
            .contains(&format!("systemctl disable --now {TUNNEL_UNIT}"))
    );
    for id in &peers {
        assert!(
            !runner
                .calls()
                .iter()
                .any(|call| call.ends_with(&quick_tunnel_unit(*id))
                    && (call.contains("restart")
                        || call.contains("stop")
                        || call.contains("disable")))
        );
    }
    let saved = Config::read(&directory.path().join("config/pipe/manifest.json")).unwrap();
    assert_eq!(saved.quick_tunnels, config.quick_tunnels);
}

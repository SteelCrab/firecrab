use super::*;
fn settings() -> ManagerResources {
    ManagerResources {
        cpu: 4,
        memory_mib: 8192,
        disk_gib: 1024,
        network_mode: ManagerNetworkMode::Mirrored,
        network_interface: None,
    }
}
#[test]
fn editing_wsl_preserves_unrelated_keys_sections_and_comments() {
    let text = "# my config\r\n[wsl2]\r\nmemory=4GB\r\nprocessors=2\r\nkernel=C:\\kernel\r\nswap=0\r\n[experimental]\r\nautoMemoryReclaim=gradual\r\n";
    let merged = merge_config(text, &settings());
    assert_eq!(value(&merged, "memory"), Some("8192MB"));
    assert_eq!(value(&merged, "processors"), Some("4"));
    assert_eq!(value(&merged, "networkingMode"), Some("mirrored"));
    assert!(merged.contains("kernel=C:\\kernel"));
    assert!(merged.contains("swap=0"));
    assert!(merged.contains("[experimental]\r\nautoMemoryReclaim=gradual"));
    assert!(merged.contains("# my config"));
    assert_eq!(merge_config(&merged, &settings()), merged);
}
#[test]
fn editing_wsl_adds_missing_section_and_removes_duplicate_owned_values() {
    let merged = merge_config("[experimental]\nfoo=bar\n", &settings());
    assert_eq!(value(&merged, "memory"), Some("8192MB"));
    let merged = merge_config(
        "[wsl2]\nMemory=2GB\nmemory=4GB\n[wsl2]\nprocessors=2\n",
        &settings(),
    );
    assert_eq!(merged.matches("memory=").count(), 1);
    assert_eq!(merged.matches("processors=").count(), 1);
}
#[test]
fn wsl_size_units_are_converted_without_overflow() {
    assert_eq!(size_mib("8GB"), Some(8192));
    assert_eq!(size_mib("512MB"), Some(512));
    assert_eq!(size_mib("1073741824"), Some(1024));
    assert_eq!(size_mib("18446744073709551615GB"), None);
}
#[test]
fn global_apply_refuses_other_running_distributions_before_mutation() {
    let calls = wsl::fake::answer(|_| Ok("Ubuntu\nfirecrab-debian\n".into()));
    let error = apply(
        Path::new("unused"),
        &ManagerResources {
            network_mode: ManagerNetworkMode::Nat,
            ..settings()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("Ubuntu"));
    assert_eq!(calls.calls().len(), 1);
    assert!(!calls.calls().iter().any(|call| call.contains("--shutdown")));
}
const HOST: &str =
    r#"{"cpu":8,"memoryMib":16384,"diskAvailableGib":200,"networkInterfaces":["Ethernet"]}"#;
const ORIGINAL: &str = "# retain\r\n[wsl2]\r\nprocessors=2\r\nmemory=4GB\r\nswap=0\r\n[experimental]\r\nautoMemoryReclaim=gradual\r\n";

fn reply(line: &str) -> wsl::fake::Reply {
    if line.starts_with("schtasks.exe ") {
        return Err("recovery startup failed".into());
    }
    if line.contains("OSVersion.Version.Build") {
        return Ok("26200".into());
    }
    if line.starts_with("powershell.exe ") {
        return Ok(HOST.into());
    }
    if line.contains("df -BG") {
        return Ok("1024G\n".into());
    }
    if line.starts_with("wsl.exe --list") {
        return Ok("firecrab-debian\n".into());
    }
    Ok(String::new())
}

#[test]
fn host_capacity_and_mirrored_support_follow_native_probe_results() {
    let dir = tempfile::tempdir().unwrap();
    let calls = wsl::fake::answer(reply);
    let host = host_capacity(dir.path()).unwrap();
    assert_eq!(host.cpu, 8);
    assert_eq!(host.memory_mib, 16384);
    assert_eq!(
        host.network_modes,
        [ManagerNetworkMode::Nat, ManagerNetworkMode::Mirrored]
    );
    validate_configuration(&settings()).unwrap();
    assert!(
        calls
            .calls()
            .iter()
            .any(|line| line.contains("OSVersion.Version.Build"))
    );
    drop(calls);
    for output in ["22000", "not a build"] {
        let calls = wsl::fake::answer(move |_| Ok(output.into()));
        assert!(validate_configuration(&settings()).is_err());
        assert_eq!(calls.calls().len(), 1);
    }
    let _calls = wsl::fake::answer(|_| Err("PowerShell unavailable".into()));
    assert!(
        host_capacity(dir.path())
            .unwrap_err()
            .to_string()
            .contains("PowerShell unavailable")
    );
}

#[test]
fn defaults_read_only_the_isolated_config_and_current_disk_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join(".wslconfig");
    let _calls = wsl::fake::answer(reply).with_config(&config);
    let host = host_capacity(dir.path()).unwrap();
    let initial = defaults(dir.path(), &host).unwrap();
    assert_eq!(initial.cpu, 8);
    assert_eq!(initial.memory_mib, 8192);
    assert_eq!(initial.disk_gib, 1024);
    fs::write(&config, "\u{feff}[wsl2]\nprocessors=3\nmemory=6GB # retained comment\nnetworkingMode=MiRrOrEd\ndefaultVhdSize=2GB\n").unwrap();
    let saved = defaults(dir.path(), &host).unwrap();
    assert_eq!(saved.cpu, 3);
    assert_eq!(saved.memory_mib, 6144);
    assert_eq!(saved.disk_gib, 2);
    assert_eq!(saved.network_mode, ManagerNetworkMode::Mirrored);
    resource::write(
        &dir.path().join("runtime/resources-active.json"),
        &settings(),
    )
    .unwrap();
    assert_eq!(defaults(dir.path(), &host).unwrap().disk_gib, 1024);
    fs::write(&config, "[wsl2]\nnetworkingMode=none\n").unwrap();
    assert!(
        defaults(dir.path(), &host)
            .unwrap_err()
            .to_string()
            .contains("not supported")
    );
}

#[test]
fn unreadable_wsl_configuration_is_not_replaced_with_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let _calls = wsl::fake::answer(reply).with_config(dir.path());
    assert!(read_config().is_err());
}

#[test]
fn grow_updates_only_owned_settings_and_backs_up_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join(".wslconfig");
    fs::write(&config, ORIGINAL).unwrap();
    let path = config.clone();
    let calls = wsl::fake::answer(move |line| {
        if line == "wsl.exe --shutdown" {
            assert_eq!(fs::read_to_string(&path).unwrap(), ORIGINAL);
        }
        reply(line)
    })
    .with_config(&config);
    let grow = ManagerResources {
        disk_gib: 1536,
        ..settings()
    };
    configure_wsl(dir.path(), &grow).unwrap();
    let updated = fs::read_to_string(&config).unwrap();
    assert_eq!(value(&updated, "memory"), Some("8192MB"));
    assert_eq!(value(&updated, "processors"), Some("4"));
    assert_eq!(value(&updated, "networkingMode"), Some("mirrored"));
    assert!(updated.contains("swap=0"));
    assert!(updated.contains("autoMemoryReclaim=gradual"));
    assert_eq!(
        fs::read_to_string(dir.path().join("runtime/wslconfig.before-resources")).unwrap(),
        ORIGINAL
    );
    assert!(
        calls
            .calls()
            .iter()
            .any(|line| line == "wsl.exe --manage firecrab-debian --resize 1536GB")
    );
    assert!(!dir.path().join("runtime/resources-active.json").exists());
}

#[test]
fn cpu_only_changes_do_not_resize_the_virtual_disk() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join(".wslconfig");
    let calls = wsl::fake::answer(reply).with_config(&config);
    configure_wsl(
        dir.path(),
        &ManagerResources {
            network_mode: ManagerNetworkMode::Nat,
            ..settings()
        },
    )
    .unwrap();
    assert!(!calls.calls().iter().any(|line| line.contains("--resize")));
    assert!(fs::read_to_string(config).unwrap().contains("processors=4"));
}

#[test]
fn failed_disk_growth_restores_the_exact_original_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join(".wslconfig");
    fs::write(&config, ORIGINAL).unwrap();
    let path = config.clone();
    let calls = wsl::fake::answer(move |line| {
        if line.contains("--resize") {
            return Err("disk resize failed".into());
        }
        if line.starts_with("schtasks.exe ") {
            assert_eq!(fs::read_to_string(&path).unwrap(), ORIGINAL);
        }
        reply(line)
    })
    .with_config(&config);
    let grow = ManagerResources {
        disk_gib: 1536,
        ..settings()
    };
    let error = configure_wsl(dir.path(), &grow).unwrap_err();
    assert!(error.to_string().contains("disk resize failed"));
    assert!(!error.to_string().contains("recovery startup failed"));
    assert!(
        calls
            .calls()
            .iter()
            .any(|line| line.starts_with("schtasks.exe /Query"))
    );
    assert_eq!(fs::read_to_string(&config).unwrap(), ORIGINAL);
    assert_eq!(
        fs::read_to_string(dir.path().join("runtime/wslconfig.before-resources")).unwrap(),
        ORIGINAL
    );
}

#[test]
fn invalid_or_shrinking_filesystems_fail_before_shutdown_or_writing() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join(".wslconfig");
    fs::write(&config, ORIGINAL).unwrap();
    for size in ["bad", "2048G"] {
        let calls = wsl::fake::answer(move |line| {
            if line.contains("df -BG") {
                return Ok(size.into());
            }
            reply(line)
        })
        .with_config(&config);
        assert!(configure_wsl(dir.path(), &settings()).is_err());
        assert!(!calls.calls().iter().any(|line| line.contains("--shutdown")));
        assert_eq!(fs::read_to_string(&config).unwrap(), ORIGINAL);
    }
    let calls = wsl::fake::answer(|line| {
        if line == "wsl.exe --list --quiet" {
            return Ok(String::new());
        }
        reply(line)
    })
    .with_config(&config);
    assert!(
        configure_wsl(dir.path(), &settings())
            .unwrap_err()
            .to_string()
            .contains("not installed")
    );
    assert!(!calls.calls().iter().any(|line| line.contains("--shutdown")));
}

#[test]
fn shutdown_failures_leave_the_original_configuration_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join(".wslconfig");
    fs::write(&config, ORIGINAL).unwrap();
    for failed_command in ["--terminate", "--shutdown"] {
        let calls = wsl::fake::answer(move |line| {
            if line.contains(failed_command) {
                return Err("could not stop WSL".into());
            }
            reply(line)
        })
        .with_config(&config);
        assert!(
            configure_wsl(dir.path(), &settings())
                .unwrap_err()
                .to_string()
                .contains("could not stop WSL")
        );
        assert_eq!(fs::read_to_string(&config).unwrap(), ORIGINAL);
        assert!(
            calls
                .calls()
                .iter()
                .any(|line| line.starts_with("schtasks.exe /Query"))
        );
        assert!(
            !dir.path()
                .join("runtime/wslconfig.before-resources")
                .exists()
        );
    }
}

#[test]
fn first_growth_uses_the_actual_filesystem_instead_of_the_default_vhd_size() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join(".wslconfig");
    fs::write(&config, "[wsl2]\ndefaultVhdSize=1024GB\n").unwrap();
    let calls = wsl::fake::answer(|line| {
        if line.contains("df -BG") {
            return Ok("256G\n".into());
        }
        reply(line)
    })
    .with_config(&config);
    configure_wsl(
        dir.path(),
        &ManagerResources {
            disk_gib: 512,
            network_mode: ManagerNetworkMode::Nat,
            ..settings()
        },
    )
    .unwrap();
    assert!(
        calls
            .calls()
            .iter()
            .any(|line| line == "wsl.exe --manage firecrab-debian --resize 512GB")
    );
    assert!(
        !calls
            .calls()
            .iter()
            .any(|line| line.starts_with("powershell.exe "))
    );
}

#[test]
fn backup_and_config_write_failures_restart_without_masking_the_io_error() {
    for fail_backup in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join(".wslconfig");
        fs::write(&config, ORIGINAL).unwrap();
        if fail_backup {
            fs::write(dir.path().join("runtime"), b"blocked").unwrap();
        }
        let path = config.clone();
        let calls = wsl::fake::answer(move |line| {
            if line == "wsl.exe --shutdown" && !fail_backup {
                fs::remove_file(&path).unwrap();
                fs::create_dir(&path).unwrap();
            }
            reply(line)
        })
        .with_config(&config);
        assert!(matches!(
            configure_wsl(dir.path(), &settings()),
            Err(Error::Io(_))
        ));
        assert!(
            calls
                .calls()
                .iter()
                .any(|line| line.starts_with("schtasks.exe /Query"))
        );
        if fail_backup {
            assert_eq!(fs::read_to_string(&config).unwrap(), ORIGINAL);
        }
    }
}

#[test]
fn rollback_and_startup_failures_preserve_the_original_resize_error() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join(".wslconfig");
    fs::write(&config, ORIGINAL).unwrap();
    let path = config.clone();
    let calls = wsl::fake::answer(move |line| {
        if line.contains("--resize") {
            fs::remove_file(&path).unwrap();
            fs::create_dir(&path).unwrap();
            return Err("original resize failure".into());
        }
        reply(line)
    })
    .with_config(&config);
    let error = configure_wsl(
        dir.path(),
        &ManagerResources {
            disk_gib: 1536,
            ..settings()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("original resize failure"));
    assert!(!error.to_string().contains("recovery startup failed"));
    assert!(
        calls
            .calls()
            .iter()
            .any(|line| line.starts_with("schtasks.exe /Query"))
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("runtime/wslconfig.before-resources")).unwrap(),
        ORIGINAL
    );
}

use super::*;
use clap::Parser;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: super::super::Command,
}

#[test]
fn resource_and_requested_resorce_spelling_use_the_same_options() {
    for name in ["resource", "resorce"] {
        let cli = Cli::try_parse_from([
            "firecrab",
            name,
            "--cpu",
            "4",
            "--ram",
            "8192",
            "--disk",
            "32",
            "--network",
            "nat",
            "--apply",
            "--json",
        ])
        .unwrap();
        let super::super::Command::Resource { options } = cli.command else {
            panic!("resource command");
        };
        assert_eq!(options.cpu, Some(4));
        assert_eq!(options.ram, Some(8192));
        assert!(options.apply && options.json);
    }
}

fn host() -> ManagerHostCapacity {
    ManagerHostCapacity {
        cpu: 8,
        memory_mib: 16384,
        disk_available_gib: 100,
        network_interfaces: vec!["en0".into()],
        network_modes: vec![
            ManagerNetworkMode::Nat,
            ManagerNetworkMode::Bridged,
            ManagerNetworkMode::Mirrored,
        ],
    }
}
fn settings() -> ManagerResources {
    ManagerResources {
        cpu: 4,
        memory_mib: 8192,
        disk_gib: 32,
        network_mode: ManagerNetworkMode::Nat,
        network_interface: None,
    }
}
#[test]
fn allocations_cannot_exceed_host_or_shrink_disk() {
    let mut settings = settings();
    validate(&settings, &host(), HostOs::Macos, 16).unwrap();
    settings.cpu = 9;
    assert!(validate(&settings, &host(), HostOs::Macos, 16).is_err());
    settings.cpu = 4;
    settings.memory_mib = 16385;
    assert!(validate(&settings, &host(), HostOs::Macos, 16).is_err());
    settings.memory_mib = 8192;
    assert!(validate(&settings, &host(), HostOs::Macos, 64).is_err());
}
#[test]
fn network_validation_follows_platform_and_interface() {
    let mut settings = settings();
    settings.network_mode = ManagerNetworkMode::Bridged;
    assert!(validate(&settings, &host(), HostOs::Macos, 16).is_err());
    settings.network_interface = Some("en0".into());
    validate(&settings, &host(), HostOs::Macos, 16).unwrap();
    assert!(validate(&settings, &host(), HostOs::Windows, 16).is_err());
    settings.network_mode = ManagerNetworkMode::Mirrored;
    settings.network_interface = None;
    validate(&settings, &host(), HostOs::Windows, 16).unwrap();
    assert!(validate(&settings, &host(), HostOs::Macos, 16).is_err());
}
#[test]
fn persisted_allocations_round_trip_and_bad_json_does_not_reset_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("resources.json");
    assert!(read::<ManagerResources>(&path).unwrap().is_none());
    write(&path, &settings()).unwrap();
    assert_eq!(read::<ManagerResources>(&path).unwrap(), Some(settings()));
    fs::write(&path, b"bad json").unwrap();
    assert!(read::<ManagerResources>(&path).is_err());
}
#[test]
fn resource_changes_are_serialized() {
    let dir = tempfile::tempdir().unwrap();
    let first = lock(dir.path()).unwrap();
    let blocked = lock(dir.path());
    assert!(matches!(blocked, Err(Error::Busy)), "{blocked:?}");
    drop(first);
    lock(dir.path()).unwrap();
}

#[test]
fn lock_file_errors_are_not_reported_as_contention() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("runtime/resource.lock")).unwrap();
    let result = lock(dir.path());
    assert!(matches!(result, Err(Error::Io(_))), "{result:?}");
}

#[cfg(unix)]
#[test]
fn resource_lock_release_does_not_wait_for_inherited_descriptors() {
    let dir = tempfile::tempdir().unwrap();
    let held = lock(dir.path()).unwrap();
    // A forked child shares the same open-file description, just like try_clone.
    let inherited = held.0.try_clone().unwrap();
    drop(held);
    let reacquired = lock(dir.path());
    drop(inherited);
    assert!(reacquired.is_ok(), "{reacquired:?}");
    assert!(matches!(lock(dir.path()), Err(Error::Busy)));
}

#[test]
fn controller_identity_is_persistent_and_distinguishes_managed_homes() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let id = controller_id(first.path()).unwrap();
    assert_eq!(controller_id(first.path()).unwrap(), id);
    assert_ne!(controller_id(second.path()).unwrap(), id);
}
pub(crate) fn platform() -> Platform {
    Platform {
        os: HostOs::Macos,
        capacity: |_| Ok(host()),
        defaults: |_, _| Ok(settings()),
        validate: |_, _| Ok(()),
        apply: |home, allocation| write(&home.join("applied.json"), allocation),
    }
}

#[test]
fn saved_resources_remain_usable_when_defaults_are_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let platform = Platform {
        defaults: |_, _| Err(Error::Invalid("defaults unavailable".into())),
        ..platform()
    };
    write(&dir.path().join("resources.json"), &settings()).unwrap();
    write(
        &dir.path().join("runtime/resources-active.json"),
        &settings(),
    )
    .unwrap();
    assert_eq!(platform.status(dir.path()).unwrap().settings, settings());
    let next = ManagerResources {
        cpu: 6,
        ..settings()
    };
    platform.save(dir.path(), &next).unwrap();
    run_apply_in(dir.path(), "saved", platform).unwrap();
    let status = platform.status(dir.path()).unwrap();
    assert_eq!(status.current, Some(next));
    assert!(!status.pending);
}

#[test]
fn missing_settings_or_disk_floor_still_require_readable_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let platform = Platform {
        defaults: |_, _| Err(Error::Invalid("defaults unavailable".into())),
        ..platform()
    };
    assert!(platform.status(dir.path()).is_err());
    write(&dir.path().join("resources.json"), &settings()).unwrap();
    assert!(platform.status(dir.path()).is_ok());
    assert!(platform.save(dir.path(), &settings()).is_err());
    assert!(platform.apply_saved(dir.path()).is_err());
    assert!(!dir.path().join("applied.json").exists());
}

#[test]
fn saved_and_current_allocations_determine_pending_state() {
    let dir = tempfile::tempdir().unwrap();
    let platform = platform();
    let initial = platform.status(dir.path()).unwrap();
    assert!(initial.pending);
    assert!(initial.current.is_none());
    assert_eq!(initial.settings, settings());
    platform.save(dir.path(), &settings()).unwrap();
    assert!(!dir.path().join("applied.json").exists());
    run_apply_in(dir.path(), "apply-1", platform).unwrap();
    let applied = platform.status(dir.path()).unwrap();
    assert!(!applied.pending);
    assert_eq!(applied.current, Some(settings()));
    assert_eq!(applied.operation.unwrap().state, "complete");
    assert_eq!(
        read::<ManagerResources>(&dir.path().join("applied.json")).unwrap(),
        Some(settings())
    );
    let mut next = settings();
    next.cpu = 6;
    platform.save(dir.path(), &next).unwrap();
    let pending = platform.status(dir.path()).unwrap();
    assert!(pending.pending);
    assert_eq!(pending.settings.cpu, 6);
    assert_eq!(pending.current.unwrap().cpu, 4);
}

#[test]
fn rejected_allocations_and_native_validation_preserve_saved_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let platform = platform();
    platform.save(dir.path(), &settings()).unwrap();
    let path = dir.path().join("resources.json");
    let before = fs::read(&path).unwrap();
    let invalid = ManagerResources {
        cpu: 0,
        ..settings()
    };
    assert!(platform.save(dir.path(), &invalid).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    let rejecting = Platform {
        validate: |_, _| Err(Error::Invalid("native limits exceeded".into())),
        ..platform
    };
    assert!(
        rejecting
            .save(dir.path(), &settings())
            .unwrap_err()
            .to_string()
            .contains("native limits")
    );
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn failed_apply_keeps_the_previous_active_snapshot_and_records_the_error() {
    let dir = tempfile::tempdir().unwrap();
    let platform = platform();
    run_apply_in(dir.path(), "first", platform).unwrap();
    let next = ManagerResources {
        cpu: 6,
        ..settings()
    };
    platform.save(dir.path(), &next).unwrap();
    let failing = Platform {
        apply: |_, _| Err(Error::Invalid("host restart failed".into())),
        ..platform
    };
    assert!(run_apply_in(dir.path(), "failed", failing).is_err());
    let status = platform.status(dir.path()).unwrap();
    assert_eq!(status.current.unwrap().cpu, 4);
    assert!(status.pending);
    let operation = status.operation.unwrap();
    assert_eq!(operation.id, "failed");
    assert_eq!(operation.state, "failed");
    assert_eq!(operation.error.as_deref(), Some("host restart failed"));
}

#[test]
fn invalid_saved_settings_do_not_reach_the_native_apply() {
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir.path().join("resources.json"),
        &ManagerResources {
            cpu: 0,
            ..settings()
        },
    )
    .unwrap();
    assert!(run_apply_in(dir.path(), "invalid", platform()).is_err());
    assert!(!dir.path().join("applied.json").exists());
    assert!(!dir.path().join("runtime/resources-active.json").exists());
}

#[test]
fn abandoned_workers_fail_only_after_the_lock_is_released() {
    use std::time::{Duration, SystemTime};
    let dir = tempfile::tempdir().unwrap();
    let platform = platform();
    let path = dir.path().join("runtime/resource-operation.json");
    write(
        &path,
        &ManagerResourceOperation {
            id: "lost".into(),
            state: "applying".into(),
            error: None,
        },
    )
    .unwrap();
    let now = platform.status(dir.path()).unwrap();
    assert_eq!(now.operation.unwrap().state, "applying");
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(31)))
        .unwrap();
    let held = lock(dir.path()).unwrap();
    assert_eq!(
        platform
            .status(dir.path())
            .unwrap()
            .operation
            .unwrap()
            .state,
        "applying"
    );
    assert!(matches!(
        platform.save(dir.path(), &settings()),
        Err(Error::Busy)
    ));
    drop(held);
    let recovered = platform.status(dir.path()).unwrap();
    assert_eq!(recovered.operation.unwrap().state, "failed");
    assert!(recovered.pending);
    platform.save(dir.path(), &settings()).unwrap();
}

#[test]
fn live_operation_blocks_saving_even_between_worker_spawn_and_lock() {
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir.path().join("runtime/resource-operation.json"),
        &ManagerResourceOperation {
            id: "starting".into(),
            state: "applying".into(),
            error: None,
        },
    )
    .unwrap();
    assert!(matches!(
        platform().save(dir.path(), &settings()),
        Err(Error::Busy)
    ));
    assert!(!dir.path().join("resources.json").exists());
}

#[test]
fn cli_options_save_apply_and_switch_network_modes() {
    let dir = tempfile::tempdir().unwrap();
    let platform = platform();
    run_in(
        dir.path(),
        Options {
            cpu: Some(6),
            ram: Some(12288),
            disk: Some(64),
            network: Some(NetworkMode::Bridged),
            interface: Some("en0".into()),
            apply: true,
            json: false,
        },
        platform,
    )
    .unwrap();
    let status = platform.status(dir.path()).unwrap();
    assert_eq!(status.settings.cpu, 6);
    assert_eq!(status.settings.memory_mib, 12288);
    assert_eq!(status.settings.disk_gib, 64);
    assert_eq!(status.settings.network_interface.as_deref(), Some("en0"));
    assert!(!status.pending);
    run_in(
        dir.path(),
        Options {
            network: Some(NetworkMode::Nat),
            json: true,
            ..Options::default()
        },
        platform,
    )
    .unwrap();
    assert!(
        platform
            .status(dir.path())
            .unwrap()
            .settings
            .network_interface
            .is_none()
    );
    let windows = Platform {
        os: HostOs::Windows,
        ..platform
    };
    run_in(
        dir.path(),
        Options {
            network: Some(NetworkMode::Mirrored),
            ..Options::default()
        },
        windows,
    )
    .unwrap();
    assert_eq!(
        windows.status(dir.path()).unwrap().settings.network_mode,
        ManagerNetworkMode::Mirrored
    );
    run_in(dir.path(), Options::default(), windows).unwrap();
}

#[test]
fn unreadable_capacity_and_corrupt_settings_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let failing = Platform {
        capacity: |_| Err(Error::Invalid("host unavailable".into())),
        ..platform()
    };
    assert!(
        failing
            .status(dir.path())
            .unwrap_err()
            .to_string()
            .contains("host unavailable")
    );
    fs::write(dir.path().join("resources.json"), b"not json").unwrap();
    assert!(platform().status(dir.path()).is_err());
    assert!(!dir.path().join("runtime/resources-active.json").exists());
}

#[cfg(not(target_os = "windows"))]
#[test]
fn default_disk_capacity_tracks_the_existing_sparse_image() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(defaults(dir.path(), &host()).unwrap().disk_gib, 16);
    fs::create_dir(dir.path().join("data")).unwrap();
    let file = File::create(dir.path().join("data/firecrab-data.raw")).unwrap();
    file.set_len((17 << 30) + 1).unwrap();
    assert_eq!(defaults(dir.path(), &host()).unwrap().disk_gib, 18);
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[test]
fn unsupported_native_hosts_never_write_an_active_allocation() {
    let dir = tempfile::tempdir().unwrap();
    assert!(home().is_err());
    assert!(run(Options::default()).is_err());
    assert!(run_apply("unsupported").is_err());
    let native = Platform::native();
    assert!((native.capacity)(dir.path()).is_err());
    let probe = Platform {
        capacity: platform().capacity,
        defaults: platform().defaults,
        ..native
    };
    assert!(probe.save(dir.path(), &settings()).is_err());
    assert!(run_apply_in(dir.path(), "unsupported", probe).is_err());
    assert!(!dir.path().join("resources.json").exists());
    assert!(!dir.path().join("runtime/resources-active.json").exists());
}

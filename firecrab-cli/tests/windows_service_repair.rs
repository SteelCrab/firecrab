//! CLI contract checks that need no installed WSL distribution.
#![cfg(target_os = "windows")]

use std::process::Command;

#[test]
fn repair_is_available_without_install_or_purge_options() {
    let help = Command::new(env!("CARGO_BIN_EXE_firecrab"))
        .args(["service", "repair", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("service repair"));
    for flag in ["--yes", "--purge"] {
        let output = Command::new(env!("CARGO_BIN_EXE_firecrab"))
            .args(["service", "repair", flag])
            .output()
            .unwrap();
        assert!(!output.status.success());
    }
}

#[test]
fn missing_disk_fails_at_validation_without_creating_registration() {
    let directory = tempfile::tempdir().unwrap();
    let managed_home = directory.path().join("micromanager");
    let output = Command::new(env!("CARGO_BIN_EXE_firecrab"))
        .args(["service", "repair"])
        .env("FIRECRAB_MICROMANAGER_HOME", &managed_home)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let progress = String::from_utf8_lossy(&output.stdout);
    assert!(progress.contains("[REPAIR 1/6]"), "{progress}");
    assert!(!progress.contains("[REPAIR 2/6]"), "{progress}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("firecrab service reinstall"));
    assert!(!managed_home.exists());
}

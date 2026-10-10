//! Recovery messages need no running management VM or nested virtualization.
#![cfg(target_os = "macos")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

fn service(root: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_firecrab"))
        .arg("service")
        .args(arguments)
        .env("HOME", root)
        .env("FIRECRAB_INSTALL_DIR", root.join("bin"))
        .env("FIRECRAB_MICROMANAGER_HOME", root.join("micromanager"))
        .env_remove("FIRECRAB_MICROMANAGER_HELPER")
        .output()
        .unwrap()
}

fn completed_install(root: &Path) {
    let managed = root.join("micromanager");
    for file in [
        root.join("bin/firecrab"),
        root.join("bin/firecrab-micromanager-macos"),
        managed.join("system/debian-system.raw"),
        managed.join("data/firecrab-data.raw"),
        managed.join("runtime/manager_ed25519"),
    ] {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, b"preserved").unwrap();
    }
    for name in ["firecrab", "firecrab-micromanager-macos"] {
        fs::set_permissions(
            root.join("bin").join(name),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    fs::write(
        managed.join("runtime/provisioned"),
        "schema=5\nkvm=usable\nfirecrab_api=active\nfirecrab_net_helper=active\nnested_firecracker=passed\n",
    ).unwrap();
    let mut image = [0_u8; 64];
    image[56..60].copy_from_slice(b"ARMd");
    fs::write(managed.join("system/Image"), image).unwrap();
    fs::write(managed.join("system/initrd.img"), b"initrd").unwrap();
}

#[test]
fn start_with_lost_registration_suggests_repair_without_changing_assets() {
    let root = tempfile::tempdir().unwrap();
    completed_install(root.path());
    let output = service(root.path(), &["start"]);
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("service registration is missing"), "{error}");
    assert!(error.contains("firecrab service repair"), "{error}");
    assert!(!error.contains("firecrab service install"), "{error}");
    assert_eq!(
        fs::read(root.path().join("micromanager/data/firecrab-data.raw")).unwrap(),
        b"preserved"
    );
    assert!(!root.path().join("micromanager/runtime/daemon.sh").exists());
    assert!(!root.path().join("Library/LaunchAgents").exists());
}

#[test]
fn start_on_a_new_host_suggests_install() {
    let root = tempfile::tempdir().unwrap();
    let output = service(root.path(), &["start"]);
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("firecrab service install"), "{error}");
    assert!(!error.contains("firecrab service repair"), "{error}");
    assert!(!root.path().join("micromanager").exists());
}

#[test]
fn failed_repair_does_not_suggest_repair_again() {
    let root = tempfile::tempdir().unwrap();
    let output = service(root.path(), &["repair"]);
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("firecrab service reinstall"), "{error}");
    assert!(!error.contains("Recovery:"), "{error}");
}

#[test]
fn debug_json_includes_recovery_advice_inside_valid_json() {
    let root = tempfile::tempdir().unwrap();
    completed_install(root.path());
    let output = service(root.path(), &["debug", "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        report["service"]["detail"]
            .as_str()
            .unwrap()
            .contains("firecrab service repair")
    );
    assert!(
        report["api"]["detail"]
            .as_str()
            .unwrap()
            .contains("firecrab service repair")
    );
    assert!(output.stderr.is_empty());
}

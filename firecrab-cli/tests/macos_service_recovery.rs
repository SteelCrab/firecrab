//! Recovery messages need no running management VM or nested virtualization.
#![cfg(target_os = "macos")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

fn service_command(root: &Path, arguments: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_firecrab"));
    command
        .arg("service")
        .args(arguments)
        .current_dir(root)
        .env_remove("FIRECRAB_DEV_INSTANCE_ID")
        .env("HOME", root)
        .env("FIRECRAB_INSTALL_DIR", root.join("bin"))
        .env("FIRECRAB_MICROMANAGER_HOME", root.join("micromanager"))
        .env_remove("FIRECRAB_MICROMANAGER_HELPER");
    command
}

fn service(root: &Path, arguments: &[&str]) -> Output {
    service_command(root, arguments).output().unwrap()
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
fn dev_controls_require_a_running_guest_without_installing_or_building() {
    let root = tempfile::tempdir().unwrap();
    completed_install(root.path());
    for action in ["start", "restart", "clean"] {
        let output = service(root.path(), &["dev", action]);
        assert!(!output.status.success());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("management VM is not running"), "{error}");
        assert!(error.contains("firecrab service start"), "{error}");
        assert!(output.stdout.is_empty());
        assert_eq!(
            fs::read(root.path().join("micromanager/data/firecrab-data.raw")).unwrap(),
            b"preserved"
        );
        assert!(!root.path().join("micromanager/runtime/daemon.sh").exists());
        assert!(!root.path().join("Library/LaunchAgents").exists());
    }
}

#[test]
fn dev_stop_succeeds_after_the_management_vm_is_stopped() {
    let root = tempfile::tempdir().unwrap();
    completed_install(root.path());
    let output = service(root.path(), &["dev", "stop"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(!root.path().join("micromanager/runtime/daemon.sh").exists());
    assert!(!root.path().join("Library/LaunchAgents").exists());

    // Dashboard cleanup still runs when the VM is already stopped.
    fs::write(
        root.path().join("micromanager/runtime/dev-dashboard.json"),
        "invalid session",
    )
    .unwrap();
    let output = service(root.path(), &["dev", "stop"]);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.starts_with("dashboard:"), "{error}");
}

#[test]
fn private_dev_failures_never_recommend_primary_service_commands() {
    use sha2::{Digest, Sha256};
    use std::net::TcpListener;

    let directory = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    let repository = root.join("repository");
    fs::create_dir(&repository).unwrap();
    let git = |arguments: &[&std::ffi::OsStr], cwd: &Path| {
        let output = Command::new("git")
            .args(["-c", "core.fsmonitor=false", "-c", "commit.gpgsign=false"])
            .args(arguments)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    git(&["init".as_ref()], &repository);
    fs::write(repository.join("README.md"), "fixture\n").unwrap();
    git(&["add".as_ref(), "README.md".as_ref()], &repository);
    git(
        &[
            "-c".as_ref(),
            "user.name=fixture".as_ref(),
            "-c".as_ref(),
            "user.email=fixture@example.invalid".as_ref(),
            "commit".as_ref(),
            "-m".as_ref(),
            "fixture".as_ref(),
        ],
        &repository,
    );
    let checkout = root.join("linked checkout");
    git(
        &[
            "worktree".as_ref(),
            "add".as_ref(),
            "-b".as_ref(),
            "linked".as_ref(),
            checkout.as_os_str(),
        ],
        &repository,
    );
    let git_dir = fs::canonicalize(git(
        &["rev-parse".as_ref(), "--absolute-git-dir".as_ref()],
        &checkout,
    ))
    .unwrap();
    let id = format!(
        "{:x}",
        Sha256::digest(git_dir.as_os_str().as_encoded_bytes())
    )[..16]
        .to_owned();
    let primary_home = root.join("micromanager");
    let instance_root = root.join("dev-instances").join(&id);
    let private_home = instance_root.join("micromanager");
    let runtime = private_home.join("runtime");
    fs::create_dir_all(&runtime).unwrap();
    let listeners: Vec<_> = (0..3)
        .map(|_| TcpListener::bind("127.0.0.1:0").unwrap())
        .collect();
    let ports: Vec<_> = listeners
        .iter()
        .map(|listener| listener.local_addr().unwrap().port())
        .collect();
    drop(listeners);
    let instance = serde_json::json!({
        "id": id, "source": checkout, "git_dir": git_dir,
        "primary_home": primary_home, "managed_home": private_home,
        "install_dir": instance_root.join("bin"), "api_port": ports[0], "resource_port": ports[1],
    });
    let ownership = private_home.join("dev-instance.json");
    fs::write(&ownership, serde_json::to_vec(&instance).unwrap()).unwrap();
    fs::write(
        runtime.join("dev-dashboard.json"),
        serde_json::to_vec(&serde_json::json!({
            "source": checkout, "session": uuid::Uuid::new_v4(), "port": ports[2],
            "api_port": ports[0], "resource_port": ports[1], "isolated": true,
        }))
        .unwrap(),
    )
    .unwrap();
    for action in ["start", "restart", "clean"] {
        let output = service_command(&root, &["dev", action])
            .current_dir(&checkout)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let message = String::from_utf8(output.stderr).unwrap();
        assert!(
            message.contains("management VM is not running"),
            "{message}"
        );
        assert!(message.contains(runtime.to_str().unwrap()), "{message}");
        for primary_action in ["start", "repair", "reinstall", "install", "debug"] {
            assert!(
                !message.contains(&format!("firecrab service {primary_action}")),
                "{message}"
            );
        }
        assert!(!primary_home.exists());
        assert!(!runtime.join("daemon.sh").exists());
        assert!(!root.join("Library/LaunchAgents").exists());
    }
    fs::write(&ownership, b"invalid").unwrap();
    let output = service_command(&root, &["dev", "start"])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let message = String::from_utf8(output.stderr).unwrap();
    for primary_action in ["start", "repair", "reinstall", "install", "debug"] {
        assert!(
            !message.contains(&format!("firecrab service {primary_action}")),
            "{message}"
        );
    }
    assert!(!primary_home.exists());
    assert!(!runtime.join("daemon.sh").exists());
}

#[test]
fn failed_repair_does_not_suggest_repair_again() {
    let root = tempfile::tempdir().unwrap();
    let output = service(root.path(), &["repair"]);
    assert!(!output.status.success());
    let progress = String::from_utf8(output.stdout).unwrap();
    assert!(progress.contains("[REPAIR 1/6]"), "{progress}");
    assert!(
        !progress.contains("[REPAIR 2/6]"),
        "validation must precede stopping: {progress}"
    );
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

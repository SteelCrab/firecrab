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

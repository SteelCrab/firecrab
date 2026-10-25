use super::*;
use std::os::unix::fs::symlink;
#[test]
fn paths_and_symlinks_cannot_escape_or_expose_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret"), "private").unwrap();
    let root = Root::open(dir.path()).unwrap();
    for path in [
        "../secret",
        "/etc/passwd",
        "a/../../secret",
        ".git/config",
        ".env",
        "config/.env.prod",
        ".ssh/id_ed25519",
        ".aws/credentials",
        ".netrc",
    ] {
        assert!(root.read(path).is_err(), "{path}");
        assert!(root.write(path, b"bad").is_err(), "{path}");
    }
    symlink(outside.path(), dir.path().join("escape")).unwrap();
    symlink(outside.path().join("secret"), dir.path().join("link")).unwrap();
    assert!(root.read("escape/secret").is_err());
    assert!(root.write("escape/secret", b"bad").is_err());
    assert!(root.write("link", b"bad").is_err());
    assert_eq!(
        fs::read_to_string(outside.path().join("secret")).unwrap(),
        "private"
    );
    root.write("src/main.rs", b"fn main() {}").unwrap();
    assert_eq!(root.files().unwrap(), vec!["src/main.rs"]);
}
#[test]
fn bounded_files_preserve_permissions_and_reject_special_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = Root::open(dir.path()).unwrap();
    root.write("test.sh", b"echo ok").unwrap();
    root.set_mode("test.sh", 0o755).unwrap();
    root.write("test.sh", b"echo updated").unwrap();
    assert_eq!(root.mode("test.sh").unwrap(), 0o755);
    assert!(root.write("too-large", &vec![0; FILE_LIMIT + 1]).is_err());
    assert!(root.read("missing").is_err());
    let name = CString::new(dir.path().join("fifo").to_str().unwrap()).unwrap();
    // SAFETY: fixture FIFO resides in this test's temporary directory.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(root.read("fifo").is_err());
}

#[test]
fn hard_links_do_not_import_shared_private_host_files() {
    let source = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("private"), "key").unwrap();
    fs::hard_link(outside.path().join("private"), source.path().join("alias")).unwrap();
    assert!(Root::open(source.path()).unwrap().read("alias").is_err());
}

#[test]
fn atomic_source_writes_preserve_ownership_and_new_directories_inherit_the_project_owner() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("original");
    fs::write(&path, "old").unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o750)).unwrap();
    if unsafe { libc::geteuid() } == 0 {
        for path in [dir.path(), path.as_path()] {
            let name = CString::new(path.to_str().unwrap()).unwrap();
            assert_eq!(unsafe { libc::chown(name.as_ptr(), 65534, 65534) }, 0);
        }
    }
    let owner = fs::metadata(dir.path()).unwrap();
    let original = fs::metadata(&path).unwrap();
    let root = Root::open(dir.path()).unwrap();
    root.write("original", b"updated").unwrap();
    root.write("new/sub/file", b"new").unwrap();
    let updated = fs::metadata(path).unwrap();
    assert_eq!(
        (updated.uid(), updated.gid()),
        (original.uid(), original.gid())
    );
    for path in ["new", "new/sub", "new/sub/file"] {
        let metadata = fs::metadata(dir.path().join(path)).unwrap();
        assert_eq!((metadata.uid(), metadata.gid()), (owner.uid(), owner.gid()));
    }
    assert_eq!(
        fs::metadata(dir.path().join("new")).unwrap().mode() & 0o777,
        0o750
    );
}

#[test]
fn generated_caches_are_excluded_while_project_cargo_configuration_remains_available() {
    for path in [
        "node_modules/pkg/a.js",
        "dist/app.wasm",
        "coverage/report.json",
        ".npm/cache/data",
        ".cargo/registry/cache/package",
        ".cargo/git/db/pkg",
        ".yarn/cache/pkg.zip",
        "src/__pycache__/compiled.pyc",
    ] {
        assert!(!allowed(path), "{path}");
    }
    for path in [
        ".cargo/config.toml",
        "src/registry/client.rs",
        "scripts/build",
        "src/lib.rs",
    ] {
        assert!(allowed(path), "{path}");
    }
}

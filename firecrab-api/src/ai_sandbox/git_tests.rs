use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

fn git(source: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("/usr/bin/git")
        .current_dir(source)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
            "-c",
            "core.hooksPath=/dev/null",
            // Fixture construction must finish all writes before adversarial metadata edits.
            "-c",
            "maintenance.auto=false",
            "-c",
            "gc.auto=0",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn repository(format: &str) -> (tempfile::TempDir, PathBuf) {
    let fixture = tempfile::tempdir().unwrap();
    let source = fs::canonicalize(fixture.path()).unwrap().join("source");
    fs::create_dir(&source).unwrap();
    git(
        &source,
        &["init", "-q", &format!("--object-format={format}")],
    );
    fs::write(source.join("visible.txt"), "authorized repository").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-qm", "fixture"]);
    (fixture, source)
}

#[test]
fn normal_sha1_sha256_and_packed_repositories_use_only_the_private_snapshot() {
    for (format, length) in [("sha1", 40), ("sha256", 64)] {
        let (_fixture, source) = repository(format);
        let expected = git(&source, &["rev-parse", "HEAD"]);
        assert_eq!(std::str::from_utf8(&expected).unwrap().trim().len(), length);
        let snapshot = Snapshot::open(&source).unwrap();
        assert_eq!(snapshot.run(&["rev-parse", "HEAD"], 128).unwrap(), expected);
        assert_eq!(
            snapshot
                .run(&["cat-file", "blob", "HEAD:visible.txt"], 1024)
                .unwrap(),
            b"authorized repository"
        );
        git(&source, &["gc", "--quiet"]);
        let packed = Snapshot::open(&source).unwrap();
        assert_eq!(packed.run(&["rev-parse", "HEAD"], 128).unwrap(), expected);
        assert_eq!(
            packed
                .run(&["cat-file", "blob", "HEAD:visible.txt"], 1024)
                .unwrap(),
            b"authorized repository"
        );
    }
}

#[test]
fn regular_maintenance_lock_is_unread_advisory_metadata() {
    let (_fixture, source) = repository("sha1");
    let lock = source.join(".git/objects/maintenance.lock");
    let content = b"advisory coordination must never become an object";
    fs::write(&lock, content).unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o000)).unwrap();
    let snapshot = Snapshot::open(&source).unwrap();
    assert_eq!(
        snapshot
            .run(&["cat-file", "blob", "HEAD:visible.txt"], 1024)
            .unwrap(),
        b"authorized repository"
    );
    assert!(!snapshot.path.join("objects/maintenance.lock").exists());
    assert_eq!(fs::metadata(&lock).unwrap().permissions().mode() & 0o777, 0);
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(fs::read(&lock).unwrap(), content);
}

#[test]
fn maintenance_lock_links_directories_and_other_unknown_entries_stay_rejected() {
    let (_fixture, source) = repository("sha1");
    let lock = source.join(".git/objects/maintenance.lock");
    symlink(source.join("visible.txt"), &lock).unwrap();
    assert!(Snapshot::open(&source).is_err());
    fs::remove_file(&lock).unwrap();
    fs::create_dir(&lock).unwrap();
    assert!(Snapshot::open(&source).is_err());
    fs::remove_dir(&lock).unwrap();
    fs::write(source.join(".git/objects/unknown"), b"not an object").unwrap();
    assert!(Snapshot::open(&source).is_err());
}

#[test]
fn registered_linked_worktree_keeps_its_head_and_shared_objects() {
    let (fixture, source) = repository("sha1");
    let linked = fs::canonicalize(fixture.path()).unwrap().join("linked");
    git(
        &source,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked-feature",
            linked.to_str().unwrap(),
        ],
    );
    let expected = git(&linked, &["rev-parse", "HEAD"]);
    let snapshot = Snapshot::open(&linked).unwrap();
    assert_eq!(snapshot.run(&["rev-parse", "HEAD"], 128).unwrap(), expected);
    assert_eq!(
        snapshot
            .run(&["cat-file", "blob", "HEAD:visible.txt"], 1024)
            .unwrap(),
        b"authorized repository"
    );
    // Another source cannot borrow this registered metadata by forging its own marker.
    let forged = fs::canonicalize(fixture.path()).unwrap().join("forged");
    fs::create_dir(&forged).unwrap();
    fs::copy(linked.join(".git"), forged.join(".git")).unwrap();
    assert!(Snapshot::open(&forged).is_err());
}

#[test]
fn arbitrary_external_git_directory_pointers_are_rejected_before_git_runs() {
    let (fixture, private) = repository("sha1");
    let authorized = fs::canonicalize(fixture.path()).unwrap().join("authorized");
    fs::create_dir(&authorized).unwrap();
    symlink(private.join(".git"), authorized.join(".git")).unwrap();
    assert!(Snapshot::open(&authorized).is_err());
    fs::remove_file(authorized.join(".git")).unwrap();
    fs::write(
        authorized.join(".git"),
        format!("gitdir: {}\n", private.join(".git").display()),
    )
    .unwrap();
    assert!(Snapshot::open(&authorized).is_err());
}

#[test]
fn alternates_object_symlinks_and_hardlinks_cannot_import_external_content() {
    let (fixture, source) = repository("sha1");
    let private = fs::canonicalize(fixture.path()).unwrap().join("private");
    fs::create_dir(&private).unwrap();
    fs::write(private.join("private-object"), b"private").unwrap();
    let info = source.join(".git/objects/info");
    fs::create_dir_all(&info).unwrap();
    for name in ["alternates", "http-alternates"] {
        fs::write(info.join(name), format!("{}\n", private.display())).unwrap();
        assert!(Snapshot::open(&source).is_err());
        fs::remove_file(info.join(name)).unwrap();
    }
    let object = String::from_utf8(git(&source, &["rev-parse", "HEAD:visible.txt"])).unwrap();
    let object = object.trim();
    let path = source
        .join(".git/objects")
        .join(&object[..2])
        .join(&object[2..]);
    fs::remove_file(&path).unwrap();
    symlink(private.join("private-object"), &path).unwrap();
    assert!(Snapshot::open(&source).is_err());
    fs::remove_file(&path).unwrap();
    fs::hard_link(private.join("private-object"), &path).unwrap();
    assert!(Snapshot::open(&source).is_err());
}

#[test]
fn copying_metadata_closes_source_ref_and_object_replacement_races() {
    let (fixture, source) = repository("sha1");
    let snapshot = Snapshot::open(&source).unwrap();
    fs::write(source.join(".git/HEAD"), "ref: /outside/authority\n").unwrap();
    let objects = source.join(".git/objects");
    fs::rename(&objects, source.join(".git/original-objects")).unwrap();
    symlink(fs::canonicalize(fixture.path()).unwrap(), &objects).unwrap();
    assert_eq!(
        snapshot
            .run(&["cat-file", "blob", "HEAD:visible.txt"], 1024)
            .unwrap(),
        b"authorized repository"
    );
    assert!(Snapshot::open(&source).is_err());
}

#[test]
fn source_config_includes_commands_and_worktree_settings_are_never_used() {
    let (fixture, source) = repository("sha1");
    let marker = fs::canonicalize(fixture.path())
        .unwrap()
        .join("must-not-execute");
    let script = fs::canonicalize(fixture.path())
        .unwrap()
        .join("fsmonitor.sh");
    fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let external = fs::canonicalize(fixture.path())
        .unwrap()
        .join("external-config");
    fs::write(
        &external,
        "[extensions]\nobjectFormat=unsupported-external-value\n",
    )
    .unwrap();
    let mut config = fs::OpenOptions::new()
        .append(true)
        .open(source.join(".git/config"))
        .unwrap();
    writeln!(
        config,
        "\n[include]\npath={}\n[core]\nfsmonitor={}\nworktree=/outside/authority",
        external.display(),
        script.display()
    )
    .unwrap();
    let snapshot = Snapshot::open(&source).unwrap();
    assert_eq!(
        snapshot
            .run(&["cat-file", "blob", "HEAD:visible.txt"], 1024)
            .unwrap(),
        b"authorized repository"
    );
    assert!(!marker.exists());
    let config = fs::read_to_string(snapshot.path.join("config")).unwrap();
    assert!(
        !config.contains("include")
            && !config.contains("outside")
            && !config.contains("fsmonitor.sh")
    );
}

#[test]
fn oversized_metadata_and_reftable_require_an_explicit_supported_repository() {
    let (_fixture, source) = repository("sha1");
    let pack = source.join(".git/objects/pack");
    fs::create_dir_all(&pack).unwrap();
    let path = pack.join(format!("pack-{}.pack", "a".repeat(40)));
    fs::File::create(&path)
        .unwrap()
        .set_len((MAX_BYTES + 1) as u64)
        .unwrap();
    assert!(Snapshot::open(&source).is_err());
    fs::remove_file(&path).unwrap();
    let mut config = fs::OpenOptions::new()
        .append(true)
        .open(source.join(".git/config"))
        .unwrap();
    writeln!(config, "\n[extensions]\nrefStorage=reftable").unwrap();
    assert!(Snapshot::open(&source).is_err());
}

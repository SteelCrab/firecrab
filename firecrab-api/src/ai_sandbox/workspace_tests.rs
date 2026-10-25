use super::*;
#[test]
fn isolated_edits_are_reviewed_and_apply_only_to_unchanged_selected_sources() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let attempt = target.path().join("attempt");
    fs::write(source.path().join("main.rs"), "old").unwrap();
    fs::write(source.path().join("test.sh"), "echo old").unwrap();
    fs::set_permissions(
        source.path().join("test.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    prepare(source.path(), &attempt, Uuid::new_v4(), None, &[]).unwrap();
    let work = Root::open(&attempt.join("work")).unwrap();
    work.write("main.rs", b"new").unwrap();
    work.write("added.rs", b"new file").unwrap();
    work.write("test.sh", b"echo new").unwrap();
    assert_eq!(
        fs::read_to_string(source.path().join("main.rs")).unwrap(),
        "old"
    );
    let mut changes = changes(&attempt).unwrap();
    assert_eq!(changes.len(), 3);
    fs::write(source.path().join("main.rs"), "user edit").unwrap();
    assert!(
        apply(
            source.path(),
            &mut changes,
            &["added.rs".into(), "main.rs".into()]
        )
        .is_err()
    );
    assert!(!source.path().join("added.rs").exists());
    apply(
        source.path(),
        &mut changes,
        &["test.sh".into(), "added.rs".into()],
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(source.path().join("test.sh")).unwrap(),
        "echo new"
    );
    assert_eq!(
        Root::open(source.path()).unwrap().mode("test.sh").unwrap(),
        0o755
    );
    assert_eq!(
        fs::read_to_string(source.path().join("main.rs")).unwrap(),
        "user edit"
    );
    assert_eq!(changes.iter().filter(|c| c.applied).count(), 2);
}
#[test]
fn guest_archives_preserve_edits_and_reject_links() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let root = Root::open(a.path()).unwrap();
    root.write("src/main.rs", b"updated").unwrap();
    root.write("test.sh", b"echo ok").unwrap();
    root.set_mode("test.sh", 0o700).unwrap();
    import(b.path(), &archive(a.path()).unwrap()).unwrap();
    assert_eq!(
        fs::read_to_string(b.path().join("src/main.rs")).unwrap(),
        "updated"
    );
    assert_ne!(
        Root::open(b.path()).unwrap().mode("test.sh").unwrap() & 0o100,
        0
    );
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    header.set_mode(0o777);
    header.set_link_name("/etc/passwd").unwrap();
    header.set_cksum();
    builder.append_data(&mut header, "escape", &[][..]).unwrap();
    assert!(import(b.path(), &builder.into_inner().unwrap()).is_err());
    assert!(!b.path().join("escape").exists());
}
#[test]
fn source_permission_changes_are_conflicts_and_new_executables_keep_their_mode() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let attempt = target.path().join("task");
    fs::write(source.path().join("x"), "x").unwrap();
    prepare(source.path(), &attempt, Uuid::new_v4(), None, &[]).unwrap();
    let root = Root::open(&attempt.join("work")).unwrap();
    root.write("x", b"changed").unwrap();
    root.write("run.sh", b"echo ok").unwrap();
    root.set_mode("run.sh", 0o700).unwrap();
    let mut changes = changes(&attempt).unwrap();
    fs::set_permissions(source.path().join("x"), fs::Permissions::from_mode(0o400)).unwrap();
    assert!(apply(source.path(), &mut changes, &["x".into()]).is_err());
    apply(source.path(), &mut changes, &["run.sh".into()]).unwrap();
    assert_eq!(
        Root::open(source.path()).unwrap().mode("run.sh").unwrap(),
        0o744
    );
}

#[test]
fn configured_private_database_and_sidecars_are_never_copied_into_project_context() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let attempt = target.path().join("task");
    let database = source.path().join("custom-private-store.sqlite");
    fs::write(&database, "provider-token").unwrap();
    fs::write(
        source.path().join("custom-private-store.sqlite-wal"),
        "rotated-token",
    )
    .unwrap();
    fs::write(source.path().join("main.rs"), "code").unwrap();
    fs::write(
        source.path().join("accidental-token.txt"),
        "secret=connected-key",
    )
    .unwrap();
    prepare(
        source.path(),
        &attempt,
        Uuid::new_v4(),
        Some(&database),
        &["connected-key".into()],
    )
    .unwrap();
    assert_eq!(
        Root::open(&attempt.join("work")).unwrap().files().unwrap(),
        vec!["main.rs"]
    );
}

use super::tests::registration;
use super::*;
#[test]
fn nested_rule_discovery_is_root_to_leaf_and_rejects_unrelated_or_unsafe_paths() {
    assert_eq!(instructions::paths("").unwrap(), vec!["AGENTS.md"]);
    assert_eq!(
        instructions::paths("api/auth").unwrap(),
        vec!["AGENTS.md", "api/AGENTS.md", "api/auth/AGENTS.md"]
    );
    for path in [
        "../api",
        "/api",
        "api//auth",
        ".firecrab-workspace",
        "api/../web",
        "api/",
    ] {
        assert!(instructions::paths(path).is_err());
    }
}
#[test]
fn saved_rules_are_immutable_private_and_survive_reopen_and_later_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state/projects");
    let mut binding = registration();
    let db = db::ProjectDb::open(&root, &binding, Uuid::new_v4(), true).unwrap();
    let actor = Caller::default();
    let other: Caller = serde_json::from_value(
        serde_json::json!({"principal":SandboxPrincipal::Host(Uuid::new_v4()),"operator":false}),
    )
    .unwrap();
    let version = db.observe_file("AGENTS.md", b"Saved rules A\n").unwrap();
    let files = vec![ProjectInstructionFile {
        path: "AGENTS.md".into(),
        version: version.metadata.id,
        sha256: version.metadata.sha256,
        content: "Saved rules A\n".into(),
    }];
    let mut saved = ProjectInstructionSnapshot {
        id: Uuid::new_v4(),
        directory: "api".into(),
        created_at: 1,
        sha256: instructions::digest("api", &files).unwrap(),
        files,
    };
    db.save_instruction_snapshot(actor, &saved).unwrap();
    db.save_instruction_snapshot(actor, &saved).unwrap();
    assert!(matches!(
        db.instruction_snapshot(other, saved.id),
        Err(Error::Missing)
    ));
    assert!(matches!(
        db.save_instruction_snapshot(other, &saved),
        Err(Error::Missing)
    ));
    db.observe_file("AGENTS.md", b"Saved rules B\n").unwrap();
    assert_eq!(db.instruction_snapshot(actor, saved.id).unwrap(), saved);
    drop(db);
    binding.provisioning = false;
    let db = db::ProjectDb::open(&root, &binding, Uuid::new_v4(), false).unwrap();
    assert_eq!(db.instruction_snapshot(actor, saved.id).unwrap(), saved);
    saved.directory = "web".into();
    saved.sha256 = instructions::digest("web", &saved.files).unwrap();
    assert!(matches!(
        db.save_instruction_snapshot(actor, &saved),
        Err(Error::Conflict(_))
    ));
}
#[test]
fn snapshots_reject_forged_versions_and_altered_context_checksums() {
    let dir = tempfile::tempdir().unwrap();
    let binding = registration();
    let db = db::ProjectDb::open(
        &dir.path().join("state/projects"),
        &binding,
        Uuid::new_v4(),
        true,
    )
    .unwrap();
    let v = db
        .observe_file("AGENTS.md", b"real saved instructions")
        .unwrap();
    let file = ProjectInstructionFile {
        path: "AGENTS.md".into(),
        version: v.metadata.id,
        sha256: format!("{:x}", Sha256::digest(b"forged")),
        content: "forged".into(),
    };
    let mut saved = ProjectInstructionSnapshot {
        id: Uuid::new_v4(),
        directory: "".into(),
        created_at: 1,
        sha256: instructions::digest("", std::slice::from_ref(&file)).unwrap(),
        files: vec![file],
    };
    assert!(
        db.save_instruction_snapshot(Caller::default(), &saved)
            .is_err()
    );
    saved.files[0].content = "changed after hash".into();
    assert!(instructions::validate(&saved).is_err());
}

#[test]
fn additive_schema_six_preserves_saved_files_and_private_views() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state/projects");
    let mut binding = registration();
    let db = db::ProjectDb::open(&root, &binding, Uuid::new_v4(), true).unwrap();
    let version = db
        .observe_file("AGENTS.md", b"Retained before migration")
        .unwrap();
    let view_id = Uuid::new_v4();
    db.save_view(
        Caller::default(),
        view_id,
        &SaveProjectView {
            operation_id: Uuid::new_v4(),
            expected_revision: 0,
            document: serde_json::json!({"panels":["folder"]}),
        },
    )
    .unwrap();
    db.conn
        .execute_batch("DROP TABLE instruction_snapshots; PRAGMA user_version=5")
        .unwrap();
    drop(db);
    binding.provisioning = false;
    let db = db::ProjectDb::open(&root, &binding, Uuid::new_v4(), false).unwrap();
    assert_eq!(
        db.file_version("AGENTS.md", version.metadata.id)
            .unwrap()
            .bytes,
        b"Retained before migration"
    );
    assert_eq!(
        db.view(Caller::default(), view_id).unwrap().document["panels"][0],
        "folder"
    );
    let schema: u32 = db
        .conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(schema, 6);
}

#[test]
fn instruction_snapshots_reject_inconsistent_retained_directory() {
    let dir = tempfile::tempdir().unwrap();
    let db = db::ProjectDb::open(
        &dir.path().join("state/projects"),
        &registration(),
        Uuid::new_v4(),
        true,
    )
    .unwrap();
    let snapshot = ProjectInstructionSnapshot {
        id: Uuid::new_v4(),
        directory: String::new(),
        created_at: 1,
        sha256: instructions::digest("", &[]).unwrap(),
        files: vec![],
    };
    db.save_instruction_snapshot(Caller::default(), &snapshot)
        .unwrap();
    db.conn
        .execute(
            "UPDATE instruction_snapshots SET directory='different' WHERE id=?1",
            [snapshot.id.to_string()],
        )
        .unwrap();
    assert!(matches!(
        db.instruction_snapshot(Caller::default(), snapshot.id),
        Err(Error::Conflict(_))
    ));
}

#[test]
fn instruction_quota_rejects_new_context_but_keeps_accepted_retry() {
    let dir = tempfile::tempdir().unwrap();
    let db = db::ProjectDb::open(
        &dir.path().join("state/projects"),
        &registration(),
        Uuid::new_v4(),
        true,
    )
    .unwrap();
    let mut snapshot = ProjectInstructionSnapshot {
        id: Uuid::new_v4(),
        directory: String::new(),
        created_at: 1,
        sha256: instructions::digest("", &[]).unwrap(),
        files: vec![],
    };
    let actor = Caller::default();
    db.save_instruction_snapshot(actor, &snapshot).unwrap();
    // Fill the remaining rows without expensive 1023 durable fixture transactions.
    db.conn.execute("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1023) INSERT INTO instruction_snapshots(id,principal,directory,document) SELECT 'fixture-'||x,principal,directory,document FROM n,instruction_snapshots WHERE id=?1", [snapshot.id.to_string()]).unwrap();
    db.save_instruction_snapshot(actor, &snapshot).unwrap();
    assert_eq!(
        db.instruction_snapshot(actor, snapshot.id).unwrap(),
        snapshot
    );
    snapshot.id = Uuid::new_v4();
    assert!(matches!(
        db.save_instruction_snapshot(actor, &snapshot),
        Err(Error::Conflict(_))
    ));
}

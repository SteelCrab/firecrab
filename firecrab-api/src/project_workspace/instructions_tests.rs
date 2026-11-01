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
        .execute_batch("DROP TABLE private_editor_snapshots; DROP TABLE private_editor_clients; DROP TABLE instruction_snapshots; PRAGMA user_version=5")
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
    assert_eq!(schema, db::SCHEMA_VERSION);
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

#[tokio::test]
async fn accepted_instruction_context_replays_while_stopped_and_remains_caller_private() {
    use super::tests::{bind, call, fixture};
    use axum::http::StatusCode;
    use serde_json::json;
    let (_dir, state, b) = fixture().await;
    bind(&state, &b).await;
    state.vms.lock().unwrap().get_mut(&b.vm_id).unwrap().state = VmState::Stopped;
    let host = Uuid::new_v4();
    let actor: Caller =
        serde_json::from_value(json!({"principal":SandboxPrincipal::Host(host),"operator":false}))
            .unwrap();
    state
        .store
        .save_sandbox_access(
            host,
            &SandboxAccessPolicy {
                grants: vec![SandboxResourceGrant {
                    resource: SandboxResource::Project { id: b.project_id },
                    permissions: vec![SandboxPermission::Read],
                }],
            },
        )
        .unwrap();
    let snapshot = {
        let db = open(&state, &binding(&state, b.project_id).unwrap()).unwrap();
        let version = db
            .observe_file("AGENTS.md", b"Saved rules before VM stop")
            .unwrap();
        let files = vec![ProjectInstructionFile {
            path: "AGENTS.md".into(),
            version: version.metadata.id,
            sha256: version.metadata.sha256,
            content: "Saved rules before VM stop".into(),
        }];
        let saved = ProjectInstructionSnapshot {
            id: Uuid::new_v4(),
            directory: "api".into(),
            created_at: 1,
            sha256: instructions::digest("api", &files).unwrap(),
            files,
        };
        db.save_instruction_snapshot(actor, &saved).unwrap();
        saved
    };
    let path = format!(
        "/api/ai-sandbox/projects/{}/workspace/instructions/snapshots/{}",
        b.project_id, snapshot.id
    );
    for (method, body) in [("GET", json!({})), ("PUT", json!({"directory":"api"}))] {
        let (status, response) = call(&state, method, &path, body, Some(host)).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert_eq!(
            serde_json::from_value::<ProjectInstructionSnapshot>(response).unwrap(),
            snapshot
        );
    }
    // Operator permission does not bypass another caller's private snapshot identity.
    assert_eq!(
        call(&state, "GET", &path, json!({}), None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&state, "PUT", &path, json!({"directory":"web"}), Some(host))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let new_path = format!(
        "/api/ai-sandbox/projects/{}/workspace/instructions/snapshots/{}",
        b.project_id,
        Uuid::new_v4()
    );
    assert_eq!(
        call(
            &state,
            "PUT",
            &new_path,
            json!({"directory":"api"}),
            Some(host)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(state.vms.lock().unwrap()[&b.vm_id].state, VmState::Stopped);
    state
        .store
        .save_sandbox_access(host, &SandboxAccessPolicy::default())
        .unwrap();
    assert_eq!(
        call(&state, "GET", &path, json!({}), Some(host)).await.0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn instruction_capture_rejects_supplied_context_identity_with_json_error() {
    use super::tests::{call, fixture};
    use axum::http::StatusCode;
    use serde_json::json;
    let (_dir, state, b) = fixture().await;
    let path = format!(
        "/api/ai-sandbox/projects/{}/workspace/instructions/snapshots/{}",
        b.project_id,
        Uuid::new_v4()
    );
    let (status, body) = call(&state,"PUT",&path,json!({"directory":"api","principal":"forged","files":[{"content":"unsaved forged rules"}]}),None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.get("error").is_some());
}

#[test]
fn a_replaced_database_owner_cannot_admit_or_reacknowledge_instruction_context() {
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
    db.conn
        .execute("UPDATE identity SET epoch=epoch+1 WHERE singleton=1", [])
        .unwrap();
    assert!(matches!(
        db.save_instruction_snapshot(actor, &snapshot),
        Err(Error::Busy)
    ));
    snapshot.id = Uuid::new_v4();
    assert!(matches!(
        db.save_instruction_snapshot(actor, &snapshot),
        Err(Error::Busy)
    ));
    assert_eq!(
        db.conn
            .query_row::<i64, _, _>("SELECT count(*) FROM instruction_snapshots", [], |row| row
                .get(0))
            .unwrap(),
        1
    );
}

use super::tests::{bind, call, fixture, registration};
use super::*;
use axum::http::StatusCode;
use serde_json::json;

fn person(id: Uuid) -> Caller {
    serde_json::from_value(json!({"principal":SandboxPrincipal::Host(id),"operator":false}))
        .unwrap()
}
fn view(document: serde_json::Value, expected_revision: u64) -> SaveProjectView {
    SaveProjectView {
        operation_id: Uuid::new_v4(),
        expected_revision,
        document,
    }
}
fn draft(bytes: &[u8], expected_revision: u64) -> SaveProjectDraft {
    SaveProjectDraft {
        operation_id: Uuid::new_v4(),
        expected_revision,
        base_version: 0,
        content_base64: STANDARD.encode(bytes),
    }
}

#[test]
fn private_views_and_buffers_survive_reopen_without_exposing_another_user_or_tab() {
    let dir = tempfile::tempdir().unwrap();
    let mut binding = registration();
    let root = dir.path().join("state/projects");
    let db = db::ProjectDb::open(&root, &binding, Uuid::new_v4(), true).unwrap();
    let alice = person(Uuid::new_v4());
    let bob = person(Uuid::new_v4());
    let tab = Uuid::new_v4();
    let other = Uuid::new_v4();
    for (caller, id, name, text) in [
        (alice, tab, "Alice", b"private A".as_slice()),
        (bob, tab, "Bob", b"private B"),
        (alice, other, "Blank tab", b"other tab"),
    ] {
        db.save_view(caller, id, &view(json!({"name":name,"nodes":[]}), 0))
            .unwrap();
        db.save_draft(caller, id, "main.rs", &draft(text, 0), false)
            .unwrap();
    }
    assert!(matches!(
        db.view(Caller::default(), tab),
        Err(Error::Missing)
    ));
    assert!(matches!(
        db.draft(bob, other, "main.rs"),
        Err(Error::Missing)
    ));
    drop(db);
    binding.provisioning = false;
    let db = db::ProjectDb::open(&root, &binding, Uuid::new_v4(), false).unwrap();
    assert_eq!(db.view(alice, tab).unwrap().document["name"], "Alice");
    assert_eq!(db.view(bob, tab).unwrap().document["name"], "Bob");
    assert_eq!(db.views(alice).unwrap().len(), 2);
    assert_eq!(db.views(bob).unwrap().len(), 1);
    assert_eq!(
        STANDARD
            .decode(
                db.draft(alice, tab, "main.rs")
                    .unwrap()
                    .content_base64
                    .unwrap()
            )
            .unwrap(),
        b"private A"
    );
    assert_eq!(
        STANDARD
            .decode(
                db.draft(bob, tab, "main.rs")
                    .unwrap()
                    .content_base64
                    .unwrap()
            )
            .unwrap(),
        b"private B"
    );
}

#[test]
fn lost_private_save_or_delete_response_never_overwrites_newer_work() {
    let dir = tempfile::tempdir().unwrap();
    let binding = registration();
    let db = db::ProjectDb::open(
        &dir.path().join("state/projects"),
        &binding,
        Uuid::new_v4(),
        true,
    )
    .unwrap();
    let caller = person(Uuid::new_v4());
    let tab = Uuid::new_v4();
    let first = view(json!({"x":1}), 0);
    assert_eq!(db.save_view(caller, tab, &first).unwrap().revision, 1);
    db.save_view(caller, tab, &view(json!({"x":2}), 1)).unwrap();
    assert_eq!(db.save_view(caller, tab, &first).unwrap().revision, 1);
    assert_eq!(db.view(caller, tab).unwrap().document["x"], 2);
    assert!(matches!(
        db.save_view(caller, tab, &view(json!({"x":3}), 1)),
        Err(Error::Conflict(_))
    ));
    let mut reused = first.clone();
    reused.document = json!({"x":4});
    assert!(db.save_view(caller, tab, &reused).is_err());
    let old = draft(b"unsaved first", 0);
    db.save_draft(caller, tab, "file.txt", &old, false).unwrap();
    let remove = draft(b"", 1);
    assert_eq!(
        db.save_draft(caller, tab, "file.txt", &remove, true)
            .unwrap()
            .revision,
        2
    );
    assert!(
        db.draft(caller, tab, "file.txt")
            .unwrap()
            .content_base64
            .is_none()
    );
    // A refreshed client sees the discarded draft revision and cannot revive an older create.
    assert_eq!(
        db.save_draft(caller, tab, "file.txt", &draft(b"unsaved new", 2), false)
            .unwrap()
            .revision,
        3
    );
    assert_eq!(
        db.save_draft(caller, tab, "file.txt", &remove, true)
            .unwrap()
            .revision,
        2
    );
    assert_eq!(
        db.save_draft(caller, tab, "file.txt", &old, false)
            .unwrap()
            .revision,
        1
    );
    assert_eq!(db.draft(caller, tab, "file.txt").unwrap().revision, 3);
    assert_eq!(
        STANDARD
            .decode(
                db.draft(caller, tab, "file.txt")
                    .unwrap()
                    .content_base64
                    .unwrap()
            )
            .unwrap(),
        b"unsaved new"
    );
}

#[test]
fn private_limits_do_not_evict_existing_buffers_and_reject_invalid_bases_or_paths() {
    let dir = tempfile::tempdir().unwrap();
    let binding = registration();
    let db = db::ProjectDb::open(
        &dir.path().join("state/projects"),
        &binding,
        Uuid::new_v4(),
        true,
    )
    .unwrap();
    let caller = person(Uuid::new_v4());
    let tab = Uuid::new_v4();
    db.save_view(caller, tab, &view(json!({}), 0)).unwrap();
    assert!(
        db.save_view(caller, Uuid::new_v4(), &view(json!([]), 0))
            .is_err()
    );
    assert!(
        db.save_view(
            caller,
            Uuid::new_v4(),
            &view(json!({"text":"x".repeat(65536)}), 0)
        )
        .is_err()
    );
    let mut bad = draft(b"file", 0);
    bad.base_version = 99;
    assert!(db.save_draft(caller, tab, "a.rs", &bad, false).is_err());
    for path in ["../etc/passwd", ".firecrab-workspace/secret", "/absolute"] {
        assert!(
            db.save_draft(caller, tab, path, &draft(b"bad", 0), false)
                .is_err()
        );
    }
    assert!(
        db.save_draft(
            caller,
            tab,
            "large",
            &draft(&vec![0; 1024 * 1024 + 1], 0),
            false
        )
        .is_err()
    );
    for n in 0..32 {
        db.save_draft(
            caller,
            tab,
            &format!("file{n}.txt"),
            &draft(b"retained", 0),
            false,
        )
        .unwrap();
    }
    assert!(
        db.save_draft(caller, tab, "overflow.txt", &draft(b"overflow", 0), false)
            .is_err()
    );
    assert_eq!(db.drafts(caller, tab).unwrap().len(), 32);
    db.save_draft(caller, tab, "file0.txt", &draft(b"updated", 1), false)
        .unwrap();
    assert_eq!(db.draft(caller, tab, "file0.txt").unwrap().revision, 2);
    assert!(db.draft(caller, tab, "overflow.txt").is_err());
}

#[tokio::test]
async fn private_http_authorization_is_independent_of_a_running_vm() {
    let (_dir, state, b) = fixture().await;
    bind(&state, &b).await;
    let editor = Uuid::new_v4();
    let reader = Uuid::new_v4();
    for (host, permissions) in [
        (
            editor,
            vec![SandboxPermission::Read, SandboxPermission::Write],
        ),
        (reader, vec![SandboxPermission::Read]),
    ] {
        state
            .store
            .save_sandbox_access(
                host,
                &SandboxAccessPolicy {
                    grants: vec![SandboxResourceGrant {
                        resource: SandboxResource::Project { id: b.project_id },
                        permissions,
                    }],
                },
            )
            .unwrap();
    }
    let tab = Uuid::new_v4();
    let base = format!(
        "/api/ai-sandbox/projects/{}/workspace/views/{tab}",
        b.project_id
    );
    assert_eq!(
        call(
            &state,
            "PUT",
            &base,
            json!(view(json!({"nodes":[]}), 0)),
            Some(editor)
        )
        .await
        .0,
        StatusCode::OK
    );
    let path = format!("{base}/draft?path=source.rs");
    assert_eq!(
        call(
            &state,
            "PUT",
            &path,
            json!(draft(b"unsaved", 0)),
            Some(editor)
        )
        .await
        .0,
        StatusCode::OK
    );
    for caller in [None, Some(reader)] {
        assert_eq!(
            call(&state, "GET", &base, json!({}), caller).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(&state, "GET", &path, json!({}), caller).await.0,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        call(&state, "GET", &path, json!({}), Some(editor)).await.1["contentBase64"],
        STANDARD.encode(b"unsaved")
    );
    assert_eq!(
        call(
            &state,
            "PUT",
            &base,
            json!(view(json!({"reader":"own layout"}), 0)),
            Some(reader)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &state,
            "PUT",
            &path,
            json!(draft(b"denied", 0)),
            Some(reader)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&state, "GET", &base, json!({}), Some(Uuid::new_v4()))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let other = format!(
        "/api/ai-sandbox/projects/{}/workspace/views/{tab}",
        Uuid::new_v4()
    );
    assert_eq!(
        call(&state, "GET", &other, json!({}), Some(editor)).await.0,
        StatusCode::FORBIDDEN
    );
}

#[test]
fn additive_v3_migration_preserves_files_and_old_project_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = registration();
    let root = dir.path().join("state/projects");
    let db = db::ProjectDb::open(&root, &b, Uuid::new_v4(), true).unwrap();
    db.save_document(0, &json!({"retained":"schema3"})).unwrap();
    db.conn.execute_batch("DROP TABLE private_editor_attempts; DROP TABLE private_editors; DROP TABLE private_receipts;DROP TABLE private_drafts;DROP TABLE private_views;PRAGMA user_version=3;").unwrap();
    drop(db);
    b.provisioning = false;
    let db = db::ProjectDb::open(&root, &b, Uuid::new_v4(), false).unwrap();
    assert_eq!(db.document().unwrap().1["retained"], "schema3");
    let caller = person(Uuid::new_v4());
    let tab = Uuid::new_v4();
    db.save_view(caller, tab, &view(json!({}), 0)).unwrap();
    assert_eq!(db.view(caller, tab).unwrap().revision, 1);
    assert_eq!(
        db.conn
            .pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        db::SCHEMA_VERSION
    );
}

#[test]
fn receipt_compaction_allows_continued_saves_but_does_not_revive_a_discarded_buffer() {
    let dir = tempfile::tempdir().unwrap();
    let binding = registration();
    let db = db::ProjectDb::open(
        &dir.path().join("state/projects"),
        &binding,
        Uuid::new_v4(),
        true,
    )
    .unwrap();
    let caller = person(Uuid::new_v4());
    let tab = Uuid::new_v4();
    db.save_view(caller, tab, &view(json!({}), 0)).unwrap();
    let original = draft(b"old", 0);
    db.save_draft(caller, tab, "file.txt", &original, false)
        .unwrap();
    db.save_draft(caller, tab, "file.txt", &draft(b"", 1), true)
        .unwrap();
    let tx = db.conn.unchecked_transaction().unwrap();
    for _ in 0..4096 {
        db.conn
            .execute(
                "INSERT INTO private_receipts VALUES('fixture',?1,'fixture','fixture',1)",
                [Uuid::new_v4().to_string()],
            )
            .unwrap();
    }
    tx.commit().unwrap();
    db.save_view(caller, tab, &view(json!({"continued":true}), 1))
        .unwrap();
    assert!(matches!(
        db.save_draft(caller, tab, "file.txt", &original, false),
        Err(Error::Conflict(_))
    ));
    let discarded = db.draft(caller, tab, "file.txt").unwrap();
    assert!(discarded.content_base64.is_none());
    assert_eq!(discarded.revision, 2);
    db.save_draft(
        caller,
        tab,
        "file.txt",
        &draft(b"new", discarded.revision),
        false,
    )
    .unwrap();
    assert_eq!(db.draft(caller, tab, "file.txt").unwrap().revision, 3);
    let retained: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM private_receipts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(retained, 4096);
}

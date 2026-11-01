use super::super::tests::{bind, call, fixture, registration};
use super::tests::{person, private_gateway, record};
use super::{clients::*, *};
use axum::http::StatusCode;
use serde_json::json;
fn prepare(db: &super::super::db::ProjectDb, b: &Binding, caller: Caller, view: Uuid) -> Editor {
    db.save_view(
        caller,
        view,
        &SaveProjectView {
            operation_id: Uuid::new_v4(),
            expected_revision: 0,
            document: json!({"retained":true}),
        },
    )
    .unwrap();
    let mut e = record(caller, view, b);
    e.browser_protocol = Some(1);
    e.status = "ready".into();
    db.save_editor(&e).unwrap();
    e
}
fn save(db: &super::super::db::ProjectDb, e: &Editor, text: &[u8], revision: u64) -> FileProof {
    let saved = db
        .save_draft(
            e.caller,
            e.view,
            "note.txt",
            &SaveProjectDraft {
                operation_id: Uuid::new_v4(),
                expected_revision: revision,
                base_version: 0,
                content_base64: STANDARD.encode(text),
            },
            false,
        )
        .unwrap();
    FileProof {
        path: "note.txt".into(),
        revision: saved.revision,
        sha256: Some(format!("{:x}", Sha256::digest(text))),
    }
}
fn proof() -> Uuid {
    Uuid::parse_str("55000000-0000-4000-8000-000000000099").unwrap()
}
fn register(db: &super::super::db::ProjectDb, e: &Editor, c: Uuid) {
    db.register_editor_client(e, c, &instance(e)).unwrap();
    db.activate_editor_client(e, c, proof()).unwrap();
}
fn control(revision: u64, action: &str) -> Control {
    Control {
        operation_id: Uuid::new_v4(),
        expected_revision: revision,
        action: action.into(),
        window_proof: proof(),
    }
}
fn ack(revision: u64, file: FileProof) -> Report {
    Report::Flushed {
        revision,
        window_proof: proof(),
        files: vec![file],
        open_files: vec!["note.txt".into()],
        active_file: Some("note.txt".into()),
    }
}
#[test]
fn close_snapshot_survives_later_draft_writes_and_database_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = registration();
    let root = dir.path().join("state/projects");
    let db = super::super::db::ProjectDb::open(&root, &b, Uuid::new_v4(), true).unwrap();
    let alice = person();
    let e = prepare(&db, &b, alice, Uuid::new_v4());
    let c = Uuid::new_v4();
    register(&db, &e, c);
    let first = save(&db, &e, b"first private draft", 0);
    let close = control(0, "close");
    assert_eq!(db.control_editor_client(&e, c, &close).unwrap().revision, 1);
    let report = ack(1, first.clone());
    assert_eq!(
        db.report_editor_client(&e, c, &report).unwrap().status,
        "ready"
    );
    save(&db, &e, b"later window draft", 1);
    // Replaying an accepted acknowledgement cannot recapture newer private bytes.
    assert_eq!(
        db.report_editor_client(&e, c, &report).unwrap().status,
        "ready"
    );
    let snapshot = db.closed_editor_snapshot(&e, c, 1).unwrap().0;
    assert_eq!(
        snapshot.files[0].content_base64.as_deref(),
        Some(STANDARD.encode(b"first private draft").as_str())
    );
    assert_eq!(
        db.draft(alice, e.view, "note.txt").unwrap().content_base64,
        Some(STANDARD.encode(b"later window draft"))
    );
    assert_eq!(
        db.control_editor_client(&e, c, &control(1, "detach"))
            .unwrap()
            .status,
        "detached"
    );
    assert!(matches!(
        db.control_editor_client(&e, c, &close),
        Err(Error::Conflict(_))
    ));
    let other = prepare(&db, &b, person(), e.view);
    assert!(matches!(
        db.closed_editor_snapshot(&other, c, 1),
        Err(Error::Missing)
    ));
    assert!(db.closed_editor_snapshots(&other).unwrap().is_empty());
    assert_eq!(db.editor(alice, e.view, true).unwrap().attempt, e.attempt);
    drop(db);
    b.provisioning = false;
    let db = super::super::db::ProjectDb::open(&root, &b, Uuid::new_v4(), false).unwrap();
    assert_eq!(db.editor_client(&e, c).unwrap().status, "detached");
    assert_eq!(
        db.closed_editor_snapshot(&e, c, 1).unwrap().0.sha256,
        snapshot.sha256
    );
}
#[test]
fn close_cancel_and_lost_acknowledgements_cannot_detach_a_resumed_browser() {
    let dir = tempfile::tempdir().unwrap();
    let b = registration();
    let db = super::super::db::ProjectDb::open(
        &dir.path().join("state/projects"),
        &b,
        Uuid::new_v4(),
        true,
    )
    .unwrap();
    let e = prepare(&db, &b, person(), Uuid::new_v4());
    let c = Uuid::new_v4();
    register(&db, &e, c);
    let file = save(&db, &e, b"retained", 0);
    let close = control(0, "close");
    db.control_editor_client(&e, c, &close).unwrap();
    assert!(
        db.control_editor_client(&e, c, &control(1, "detach"))
            .is_err()
    );
    let report = ack(1, file.clone());
    db.report_editor_client(&e, c, &report).unwrap();
    assert_eq!(
        db.report_editor_client(
            &e,
            c,
            &Report::Error {
                revision: 1,
                window_proof: proof(),
                message: "late lost response".into()
            }
        )
        .unwrap()
        .status,
        "ready"
    );
    db.control_editor_client(&e, c, &control(1, "cancel"))
        .unwrap();
    db.report_editor_client(
        &e,
        c,
        &Report::Active {
            revision: 2,
            window_proof: proof(),
        },
    )
    .unwrap();
    assert_eq!(
        db.report_editor_client(&e, c, &report).unwrap().status,
        "active"
    );
    assert!(
        db.control_editor_client(&e, c, &control(1, "detach"))
            .is_err()
    );
    assert!(matches!(
        db.control_editor_client(&e, c, &close),
        Err(Error::Conflict(_))
    ));
    db.control_editor_client(&e, c, &control(2, "close"))
        .unwrap();
    assert_eq!(
        db.report_editor_client(&e, c, &report).unwrap().status,
        "flushing"
    );
    let mut forged = file.clone();
    forged.sha256 = Some("a".repeat(64));
    assert!(db.report_editor_client(&e, c, &ack(3, forged)).is_err());
    assert_eq!(db.editor_client(&e, c).unwrap().status, "flushing");
    db.report_editor_client(&e, c, &ack(3, file)).unwrap();
    assert!(
        db.control_editor_client(&e, c, &control(3, "close"))
            .is_ok()
    );
}
#[test]
fn close_snapshots_are_private_immutable_fenced_and_quota_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let b = registration();
    let db = super::super::db::ProjectDb::open(
        &dir.path().join("state/projects"),
        &b,
        Uuid::new_v4(),
        true,
    )
    .unwrap();
    let e = prepare(&db, &b, person(), Uuid::new_v4());
    let c = Uuid::new_v4();
    register(&db, &e, c);
    let file = save(&db, &e, b"accepted", 0);
    db.control_editor_client(&e, c, &control(0, "close"))
        .unwrap();
    let report = ack(1, file.clone());
    db.report_editor_client(&e, c, &report).unwrap();
    let mut changed = file.clone();
    changed.path = "other.txt".into();
    assert!(db.report_editor_client(&e, c, &ack(1, changed)).is_err());
    db.conn.execute("WITH RECURSIVE n(x) AS (VALUES(100) UNION ALL SELECT x+1 FROM n WHERE x<1122) INSERT INTO private_editor_snapshots SELECT ?1,x,'quota-fixture','{}' FROM n",[c.to_string()]).unwrap();
    assert_eq!(
        db.report_editor_client(&e, c, &report).unwrap().status,
        "ready"
    );
    db.control_editor_client(&e, c, &control(1, "close"))
        .unwrap();
    assert!(matches!(
        db.report_editor_client(&e, c, &ack(2, file)),
        Err(Error::Conflict(_))
    ));
    assert_eq!(db.editor_client(&e, c).unwrap().status, "flushing");
    assert_eq!(
        db.closed_editor_snapshot(&e, c, 1).unwrap().0.files[0].content_base64,
        Some(STANDARD.encode(b"accepted"))
    );
    db.conn
        .execute("UPDATE identity SET epoch=epoch+1", [])
        .unwrap();
    assert!(db.report_editor_client(&e, c, &report).is_err());
    assert!(
        db.control_editor_client(&e, c, &control(2, "cancel"))
            .is_err()
    );
}
#[tokio::test]
async fn browser_close_http_remains_private_and_durable_with_a_stopped_vm() {
    let (_dir, state, b) = fixture().await;
    bind(&state, &b).await;
    let alice = person();
    let bob = person();
    let SandboxPrincipal::Host(a) = alice.principal else {
        unreachable!()
    };
    let SandboxPrincipal::Host(bb) = bob.principal else {
        unreachable!()
    };
    for host in [a, bb] {
        state
            .store
            .save_sandbox_access(
                host,
                &SandboxAccessPolicy {
                    grants: vec![SandboxResourceGrant {
                        resource: SandboxResource::Project { id: b.project_id },
                        permissions: vec![SandboxPermission::Read, SandboxPermission::Write],
                    }],
                },
            )
            .unwrap();
    }
    let registered = binding(&state, b.project_id).unwrap();
    let db = open(&state, &registered).unwrap();
    let e = prepare(&db, &registered, alice, Uuid::new_v4());
    let c = Uuid::new_v4();
    register(&db, &e, c);
    let file = save(&db, &e, b"private while stopped", 0);
    drop(db);
    let base = format!(
        "/api/ai-sandbox/projects/{}/workspace/editors/{}",
        b.project_id, e.view
    );
    let client = format!("{base}/clients/{c}");
    assert_eq!(
        call(
            &state,
            "PUT",
            &client,
            json!({"instance":instance(&e)}),
            Some(a)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&state, "GET", &client, json!({}), Some(bb)).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &state,
            "PUT",
            &format!("{base}/clients/{}", Uuid::new_v4()),
            json!({"instance":instance(&e)}),
            Some(a)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let command = control(0, "close");
    let body = serde_json::to_value(&command).unwrap();
    assert_eq!(
        call(
            &state,
            "PUT",
            &(client.clone() + "/control"),
            body.clone(),
            Some(a)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&state, "PUT", &(client.clone() + "/control"), body, Some(a))
            .await
            .1["revision"],
        1
    );
    let result = call(
        &state,
        "POST",
        &(client.clone() + "/report"),
        serde_json::to_value(ack(1, file)).unwrap(),
        Some(a),
    )
    .await;
    assert_eq!(result.0, StatusCode::OK, "{}", result.1);
    assert_eq!(result.1["status"], "ready");
    let snapshot = format!("{base}/closed-snapshots/{c}/1");
    let result = call(&state, "GET", &snapshot, json!({}), Some(a)).await;
    assert_eq!(result.0, StatusCode::OK);
    assert_eq!(
        result.1["files"][0]["contentBase64"],
        STANDARD.encode(b"private while stopped")
    );
    let host = proxy::host(b.project_id, e.id);
    assert_eq!(
        private_gateway(
            state.clone(),
            &host,
            &format!("/_firecrab/clients/{c}"),
            "GET",
            json!({}),
            Some(a)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        private_gateway(
            state.clone(),
            &host,
            &format!("/_firecrab/closed-snapshots/{c}/1"),
            "GET",
            json!({}),
            Some(bb)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &state,
            "POST",
            &(client.clone() + "/report"),
            json!({"action":"active","revision":1,"caller":a}),
            Some(a)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    state
        .store
        .save_sandbox_access(
            a,
            &SandboxAccessPolicy {
                grants: vec![SandboxResourceGrant {
                    resource: SandboxResource::Project { id: b.project_id },
                    permissions: vec![SandboxPermission::Read],
                }],
            },
        )
        .unwrap();
    assert_eq!(
        call(&state, "GET", &snapshot, json!({}), Some(a)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &state,
            "PUT",
            &(client.clone() + "/control"),
            serde_json::to_value(control(1, "detach")).unwrap(),
            Some(a)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    state
        .store
        .save_sandbox_access(a, &SandboxAccessPolicy { grants: vec![] })
        .unwrap();
    assert_eq!(
        call(&state, "GET", &snapshot, json!({}), Some(a)).await.0,
        StatusCode::FORBIDDEN
    );
}
#[test]
fn schema_seven_adds_browser_close_records_without_replacing_legacy_editors() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = registration();
    let root = dir.path().join("state/projects");
    let db = super::super::db::ProjectDb::open(&root, &b, Uuid::new_v4(), true).unwrap();
    let e = prepare(&db, &b, person(), Uuid::new_v4());
    db.conn.execute_batch("DROP TABLE private_editor_snapshots; DROP TABLE private_editor_clients; PRAGMA user_version=6;").unwrap();
    drop(db);
    b.provisioning = false;
    let db = super::super::db::ProjectDb::open(&root, &b, Uuid::new_v4(), false).unwrap();
    let restored = db.editor(e.caller, e.view, true).unwrap();
    assert_eq!(restored.attempt, e.attempt);
    assert_eq!(restored.token, e.token);
    assert_eq!(
        db.conn
            .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        7
    );
    assert!(db.closed_editor_snapshots(&restored).unwrap().is_empty());
}

#[test]
fn a_copied_workbench_url_cannot_acknowledge_or_detach_the_original_window() {
    let dir = tempfile::tempdir().unwrap();
    let b = registration();
    let db = super::super::db::ProjectDb::open(
        &dir.path().join("state/projects"),
        &b,
        Uuid::new_v4(),
        true,
    )
    .unwrap();
    let e = prepare(&db, &b, person(), Uuid::new_v4());
    let c = Uuid::new_v4();
    register(&db, &e, c);
    let other = Uuid::new_v4();
    db.activate_editor_client(&e, c, other).unwrap();
    let file = save(&db, &e, b"only the original window can close", 0);
    db.control_editor_client(&e, c, &control(0, "close"))
        .unwrap();
    let forged = Report::Flushed {
        revision: 1,
        window_proof: other,
        files: vec![file.clone()],
        open_files: vec!["note.txt".into()],
        active_file: Some("note.txt".into()),
    };
    assert!(db.report_editor_client(&e, c, &forged).is_err());
    assert_eq!(db.editor_client(&e, c).unwrap().status, "flushing");
    db.report_editor_client(&e, c, &ack(1, file)).unwrap();
    let mut detach = control(1, "detach");
    detach.window_proof = other;
    assert!(db.control_editor_client(&e, c, &detach).is_err());
    assert_eq!(
        db.closed_editor_snapshot(&e, c, 1).unwrap().0.window_proof,
        proof()
    );
    assert_eq!(
        db.control_editor_client(&e, c, &control(1, "detach"))
            .unwrap()
            .status,
        "detached"
    );
}

#[test]
fn closing_a_clean_window_preserves_another_windows_private_draft() {
    let dir = tempfile::tempdir().unwrap();
    let b = registration();
    let db = super::super::db::ProjectDb::open(
        &dir.path().join("state/projects"),
        &b,
        Uuid::new_v4(),
        true,
    )
    .unwrap();
    let e = prepare(&db, &b, person(), Uuid::new_v4());
    let c = Uuid::new_v4();
    register(&db, &e, c);
    save(&db, &e, b"another window still owns this dirty buffer", 0);
    db.control_editor_client(&e, c, &control(0, "close"))
        .unwrap();
    db.report_editor_client(
        &e,
        c,
        &Report::Flushed {
            revision: 1,
            window_proof: proof(),
            files: vec![],
            open_files: vec!["note.txt".into()],
            active_file: Some("note.txt".into()),
        },
    )
    .unwrap();
    assert!(
        db.closed_editor_snapshot(&e, c, 1)
            .unwrap()
            .0
            .files
            .is_empty()
    );
    assert_eq!(
        db.draft(e.caller, e.view, "note.txt")
            .unwrap()
            .content_base64,
        Some(STANDARD.encode(b"another window still owns this dirty buffer"))
    );
}

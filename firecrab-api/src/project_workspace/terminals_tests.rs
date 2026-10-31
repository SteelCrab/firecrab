//! Test durable replay, input fencing and authenticated HTTP boundaries without a guest mock.
use super::terminals_db::{Proof, Terminal};
use super::tests::{bind, call, fixture, registration};
use super::*;
use axum::http::StatusCode;
use serde_json::json;

pub(super) fn record(binding: &Binding) -> Terminal {
    Terminal {
        id: Uuid::new_v4(),
        project: binding.project_id,
        request: CreateProjectTerminal {
            label: "Terminal".into(),
            working_directory: String::new(),
            rows: 24,
            columns: 80,
        },
        creator: Caller::default(),
        runtime: guest::Binding {
            vm_id: binding.vm_id,
            runtime_id: Uuid::new_v4(),
        },
        generation: binding.generation,
        proof: None,
        status: ProjectTerminalStatus::Pending,
        start: 0,
        end: 0,
        guest_end: 0,
        truncated: false,
        cleanup_required: false,
        exit: None,
        dimensions: None,
    }
}
fn reply(start: u64, bytes: &[u8], proof: Option<Proof>) -> terminals_guest::Reply {
    terminals_guest::Reply {
        status: ProjectTerminalStatus::Running,
        proof: proof.unwrap_or(Proof {
            boot: Uuid::new_v4(),
            uid: 100001,
            supervisor: 42,
            supervisor_ticks: 100,
            child: 43,
            child_ticks: 101,
        }),
        start,
        end: start + bytes.len() as u64,
        exit: None,
        truncated: false,
        cleanup_required: false,
        bytes: bytes.into(),
        dimensions: Some((24, 80)),
    }
}
fn principal(id: Uuid) -> Caller {
    serde_json::from_value(json!({"principal":SandboxPrincipal::Host(id),"operator":false}))
        .unwrap()
}

#[test]
fn terminal_output_and_identity_survive_database_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = registration();
    let root = dir.path().join("state/projects");
    let db = db::ProjectDb::open(&root, &b, Uuid::new_v4(), true).unwrap();
    let mut terminal = record(&b);
    db.admit_terminal(&terminal).unwrap();
    db.cache_terminal(&mut terminal, reply(0, b"hello", None))
        .unwrap();
    let proof = terminal.proof.clone();
    db.cache_terminal(&mut terminal, reply(5, b" world", proof))
        .unwrap();
    let expected = terminal.clone();
    drop(db);
    b.provisioning = false;
    let db = db::ProjectDb::open(&root, &b, Uuid::new_v4(), false).unwrap();
    let saved = db.terminal(expected.id).unwrap();
    assert_eq!(saved.proof, expected.proof);
    assert_eq!(saved.runtime, expected.runtime);
    let page = db.terminal_output(&saved, 2).unwrap();
    assert_eq!(STANDARD.decode(page.content_base64).unwrap(), b"llo world");
    assert_eq!(page.next, 11);
    assert!(db.terminal_output(&saved, 12).is_err());
}
#[test]
fn replay_detects_guest_truncation_and_rejects_changed_process_or_reversed_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let b = registration();
    let db =
        db::ProjectDb::open(&dir.path().join("state/projects"), &b, Uuid::new_v4(), true).unwrap();
    let mut terminal = record(&b);
    db.admit_terminal(&terminal).unwrap();
    db.cache_terminal(&mut terminal, reply(0, b"old", None))
        .unwrap();
    let mut next = reply(100, b"new", terminal.proof.clone());
    next.truncated = true;
    db.cache_terminal(&mut terminal, next).unwrap();
    let output = db.terminal_output(&terminal, 0).unwrap();
    assert_eq!(output.start, 100);
    assert_eq!(output.next, 103);
    assert!(output.truncated);
    assert_eq!(STANDARD.decode(output.content_base64).unwrap(), b"new");
    assert!(
        db.cache_terminal(&mut terminal, reply(103, b"wrong process", None))
            .is_err()
    );
    let proof = terminal.proof.clone();
    assert!(
        db.cache_terminal(&mut terminal, reply(2, b"backwards", proof))
            .is_err()
    );
    db.conn
        .execute("UPDATE identity SET epoch=epoch+1", [])
        .unwrap();
    assert!(matches!(db.terminal(terminal.id), Err(Error::Busy)));
}
#[test]
fn input_leases_fence_takeover_stale_release_other_principal_expiry_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let b = registration();
    let db =
        db::ProjectDb::open(&dir.path().join("state/projects"), &b, Uuid::new_v4(), true).unwrap();
    let mut terminal = record(&b);
    db.admit_terminal(&terminal).unwrap();
    db.cache_terminal(&mut terminal, reply(0, b"", None))
        .unwrap();
    let owner = Uuid::new_v4();
    let alice = principal(Uuid::new_v4());
    let bob = principal(Uuid::new_v4());
    let request = ClaimProjectTerminal {
        client_id: Uuid::new_v4(),
        expected_generation: 0,
        take_over: false,
    };
    let first = db
        .claim_terminal(terminal.id, owner, alice, request.clone())
        .unwrap();
    let control = ProjectTerminalControl {
        client_id: first.client_id,
        generation: first.generation,
        token: first.token,
    };
    db.check_terminal_control(terminal.id, owner, alice, &control)
        .unwrap();
    assert!(
        db.check_terminal_control(terminal.id, owner, bob, &control)
            .is_err()
    );
    assert!(
        db.check_terminal_control(terminal.id, Uuid::new_v4(), alice, &control)
            .is_err()
    );
    assert!(db.claim_terminal(terminal.id, owner, bob, request).is_err());
    assert!(
        db.claim_terminal(
            terminal.id,
            owner,
            bob,
            ClaimProjectTerminal {
                client_id: Uuid::new_v4(),
                expected_generation: first.generation,
                take_over: false
            }
        )
        .is_err()
    );
    let second = db
        .claim_terminal(
            terminal.id,
            owner,
            bob,
            ClaimProjectTerminal {
                client_id: Uuid::new_v4(),
                expected_generation: first.generation,
                take_over: true,
            },
        )
        .unwrap();
    assert!(
        db.renew_terminal(terminal.id, owner, alice, &control, true)
            .is_err()
    );
    let next = ProjectTerminalControl {
        client_id: second.client_id,
        generation: second.generation,
        token: second.token,
    };
    db.check_terminal_control(terminal.id, owner, bob, &next)
        .unwrap();
    let public = serde_json::to_string(&db.terminal_response(&terminal, owner).unwrap()).unwrap();
    assert!(
        !public.contains(&first.token.to_string()) && !public.contains(&second.token.to_string())
    );
    assert!(!public.contains("tokenSha") && !public.contains("token_sha"));
    db.renew_terminal(terminal.id, owner, bob, &next, true)
        .unwrap();
    assert!(
        db.renew_terminal(terminal.id, owner, bob, &next, false)
            .is_err()
    );
}
#[test]
fn terminal_creation_and_history_are_bounded_without_silent_eviction() {
    let dir = tempfile::tempdir().unwrap();
    let b = registration();
    let db =
        db::ProjectDb::open(&dir.path().join("state/projects"), &b, Uuid::new_v4(), true).unwrap();
    let first = record(&b);
    db.admit_terminal(&first).unwrap();
    for _ in 1..32 {
        db.admit_terminal(&record(&b)).unwrap();
    }
    assert!(db.admit_terminal(&record(&b)).is_err());
    assert_eq!(db.terminals().unwrap().len(), 32);
    assert_eq!(db.terminal(first.id).unwrap().request, first.request);
}
#[test]
fn terminal_protocol_rejects_invalid_requests_and_untrusted_response_identity() {
    assert!(terminals_guest::frame(Uuid::nil(), 1, 0, 24, 80, b"").is_err());
    assert!(terminals_guest::frame(Uuid::new_v4(), 4, 0, 1, 80, b"").is_err());
    assert!(terminals_guest::frame(Uuid::new_v4(), 3, 0, 0, 0, &vec![0; 16385]).is_err());
    assert!(terminals_guest::decode(b"FCTR").is_err());
    for code in 1..=9u32 {
        let mut frame = b"FCTE".to_vec();
        frame.extend_from_slice(&code.to_be_bytes());
        assert!(terminals_guest::decode(&frame).is_err());
    }
    let mut bytes = vec![0u8; 104];
    bytes[..4].copy_from_slice(b"FCTR");
    bytes[4..8].copy_from_slice(&96u32.to_be_bytes());
    bytes[8..12].copy_from_slice(b"FCTS");
    assert!(terminals_guest::decode(&bytes).is_err());
}
#[tokio::test]
async fn stopped_terminal_history_remains_available_and_readers_cannot_claim_or_send_input() {
    let (_dir, state, b) = fixture().await;
    bind(&state, &b).await;
    let binding = binding(&state, b.project_id).unwrap();
    let db = open(&state, &binding).unwrap();
    let mut terminal = record(&binding);
    db.admit_terminal(&terminal).unwrap();
    db.cache_terminal(&mut terminal, reply(0, b"retained output", None))
        .unwrap();
    drop(db);
    let reader = Uuid::new_v4();
    state
        .store
        .save_sandbox_access(
            reader,
            &SandboxAccessPolicy {
                grants: vec![SandboxResourceGrant {
                    resource: SandboxResource::Project { id: b.project_id },
                    permissions: vec![SandboxPermission::Read],
                }],
            },
        )
        .unwrap();
    let base = format!(
        "/api/ai-sandbox/projects/{}/workspace/terminals/{}",
        b.project_id, terminal.id
    );
    let (status, data) = call(&state, "GET", &base, json!({}), Some(reader)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data["status"], "interrupted");
    let (status, data) = call(
        &state,
        "GET",
        &(base.clone() + "/output?cursor=0"),
        json!({}),
        Some(reader),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        STANDARD
            .decode(data["contentBase64"].as_str().unwrap())
            .unwrap(),
        b"retained output"
    );
    let control = ProjectTerminalControl {
        client_id: Uuid::new_v4(),
        generation: 0,
        token: Uuid::new_v4(),
    };
    for (suffix, body) in [
        (
            "/control",
            json!(ClaimProjectTerminal {
                client_id: Uuid::new_v4(),
                expected_generation: 0,
                take_over: false
            }),
        ),
        (
            "/input",
            json!(ProjectTerminalMessage::Input {
                control: control.clone(),
                content_base64: STANDARD.encode(b"id\n")
            }),
        ),
        ("/stop", json!(control)),
    ] {
        assert_eq!(
            call(&state, "POST", &(base.clone() + suffix), body, Some(reader))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        call(&state, "GET", &base, json!({}), Some(Uuid::new_v4()))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn another_live_service_cannot_take_ownership_between_project_requests() {
    let (_dir, state, b) = fixture().await;
    bind(&state, &b).await;
    let path = format!("/api/ai-sandbox/projects/{}/workspace", b.project_id);
    let mut other = state.clone();
    other.workspaces = std::sync::Arc::new(Runtime::new(state.workspaces.root.clone()));
    let (status, _) = call(&other, "GET", &path, json!({}), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(&state, "GET", &path, json!({}), None).await;
    assert_eq!(status, StatusCode::OK);
    drop(state);
    let (status, response) = call(&other, "GET", &path, json!({}), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["projectId"], b.project_id.to_string());
}

#[test]
fn shared_terminal_dimensions_survive_reopen_and_legacy_json_without_dimensions() {
    let dir = tempfile::tempdir().unwrap();
    let mut binding = registration();
    let root = dir.path().join("state/projects");
    let db = db::ProjectDb::open(&root, &binding, Uuid::new_v4(), true).unwrap();
    let mut terminal = record(&binding);
    db.admit_terminal(&terminal).unwrap();
    db.cache_terminal(&mut terminal, reply(0, b"", None))
        .unwrap();
    let mut resized = reply(0, b"", terminal.proof.clone());
    resized.dimensions = Some((42, 111));
    db.cache_terminal(&mut terminal, resized).unwrap();
    let id = terminal.id;
    drop(db);
    binding.provisioning = false;
    let owner = Uuid::new_v4();
    let db = db::ProjectDb::open(&root, &binding, owner, false).unwrap();
    let recovered = db.terminal(id).unwrap();
    let view = db.terminal_response(&recovered, owner).unwrap();
    assert_eq!((view.rows, view.columns), (42, 111));
    let mut legacy = serde_json::to_value(recovered).unwrap();
    legacy.as_object_mut().unwrap().remove("dimensions");
    db.conn
        .execute(
            "UPDATE terminal_sessions SET record=?2 WHERE id=?1",
            rusqlite::params![id.to_string(), legacy.to_string()],
        )
        .unwrap();
    let legacy = db.terminal(id).unwrap();
    let view = db.terminal_response(&legacy, owner).unwrap();
    assert_eq!((view.rows, view.columns), (24, 80));
}

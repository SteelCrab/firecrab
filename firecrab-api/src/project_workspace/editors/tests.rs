use super::super::tests::{bind, fixture, registration};
use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::json;
pub(super) fn person() -> Caller {
    serde_json::from_value(
        json!({"principal":SandboxPrincipal::Host(Uuid::new_v4()),"operator":false}),
    )
    .unwrap()
}
pub(super) fn record(caller: Caller, view: Uuid, b: &Binding) -> Editor {
    Editor {
        id: Uuid::new_v4(),
        view,
        caller,
        directory: String::new(),
        file: None,
        open_request: Uuid::new_v4(),
        runtime: guest::Binding {
            vm_id: b.vm_id,
            runtime_id: Uuid::new_v4(),
        },
        generation: b.generation,
        attempt: Uuid::new_v4(),
        proof: None,
        token: "b".repeat(64),
        parent_origin: "http://localhost:5523".into(),
        bundle_revision: None,
        browser_protocol: None,
        status: "starting".into(),
    }
}
#[test]
fn editor_private_identity_survives_reopen_and_native_attempts_cannot_be_aliased() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = registration();
    let root = dir.path().join("state/projects");
    let db = super::super::db::ProjectDb::open(&root, &b, Uuid::new_v4(), true).unwrap();
    let alice = person();
    let bob = person();
    let view = Uuid::new_v4();
    for caller in [alice, bob] {
        db.save_view(
            caller,
            view,
            &SaveProjectView {
                operation_id: Uuid::new_v4(),
                expected_revision: 0,
                document: json!({}),
            },
        )
        .unwrap();
    }
    let mut first = record(alice, view, &b);
    first.bundle_revision = Some("1".repeat(64));
    let mut second = record(bob, view, &b);
    second.bundle_revision = Some("2".repeat(64));
    db.save_editor(&first).unwrap();
    db.save_editor(&second).unwrap();
    assert!(matches!(
        db.editor(bob, first.id, false),
        Err(Error::Missing)
    ));
    assert!(matches!(
        db.editor(Caller::default(), first.id, false),
        Err(Error::Missing)
    ));
    assert_ne!(
        db.editor(alice, view, true).unwrap().id,
        db.editor(bob, view, true).unwrap().id
    );
    assert!(db.private_attempt(first.attempt).unwrap());
    let mut public = super::super::terminals_tests::record(&b);
    public.id = first.attempt;
    assert!(matches!(
        db.admit_terminal(&public),
        Err(Error::Conflict(_))
    ));
    assert!(!format!("{first:?}").contains(&first.token));
    drop(db);
    b.provisioning = false;
    let db = super::super::db::ProjectDb::open(&root, &b, Uuid::new_v4(), false).unwrap();
    assert_eq!(db.editor(alice, view, true).unwrap().id, first.id);
    assert_eq!(db.editor(bob, view, true).unwrap().id, second.id);
    assert_eq!(
        db.editor(alice, view, true).unwrap().bundle_revision,
        first.bundle_revision
    );
    assert_eq!(
        db.editor(bob, view, true).unwrap().bundle_revision,
        second.bundle_revision
    );
    let public = response(b.project_id, &first).to_string();
    assert!(!public.contains(&first.token));
    assert!(!public.contains(&first.attempt.to_string()));
}

async fn proxy_request(
    state: AppState,
    host: &str,
    uri: &str,
    origin: Option<&str>,
    authenticated: bool,
) -> (StatusCode, String) {
    use tower::ServiceExt;
    let app = Router::new()
        .fallback(|| async { "controller route" })
        .with_state(state.clone())
        .layer(axum::middleware::from_fn_with_state(state, proxy::dispatch))
        .layer(Extension(RequestId(Uuid::new_v4())));
    let mut request = Request::builder().uri(uri).header("host", host);
    if let Some(origin) = origin {
        request = request.header("origin", origin);
    }
    let mut request = request.body(Body::empty()).unwrap();
    if authenticated {
        request
            .extensions_mut()
            .insert(crate::ai_sandbox::access::LocalOperator);
    }
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, String::from_utf8_lossy(&body).into())
}
#[tokio::test]
async fn reserved_editor_hosts_never_reach_controller_routes_or_trust_identity_headers() {
    let (_dir, state, b) = fixture().await;
    bind(&state, &b).await;
    let host = proxy::host(b.project_id, Uuid::new_v4());
    for path in [
        "/api/vms",
        "/api/secrets",
        "/api/micromanager/stop",
        "/_firecrab/api/vms",
        "/",
    ] {
        let (status, body) = proxy_request(state.clone(), &host, path, None, true).await;
        assert_ne!(status, StatusCode::OK);
        assert!(!body.contains("controller route"));
    }
    assert_eq!(
        proxy_request(
            state.clone(),
            "invalid.editor.localhost",
            "/api/vms",
            None,
            true
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        proxy_request(state.clone(), &host, "/api/vms", None, false)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        proxy_request(
            state.clone(),
            "localhost:5523",
            "/api/vms",
            Some(&format!("http://{host}")),
            true
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        proxy_request(
            state.clone(),
            &host,
            "http://localhost:5523/api/vms",
            None,
            true
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        proxy_request(state, "localhost:5523", "/api/vms", None, true)
            .await
            .1,
        "controller route"
    );
}

#[tokio::test]
async fn editor_metadata_checks_private_owner_before_any_guest_access() {
    let (_dir, state, b) = fixture().await;
    bind(&state, &b).await;
    let alice = person();
    let bob = person();
    let view = Uuid::new_v4();
    for caller in [alice, bob] {
        let SandboxPrincipal::Host(host) = caller.principal else {
            unreachable!()
        };
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
    }
    let registered = binding(&state, b.project_id).unwrap();
    let db = open(&state, &registered).unwrap();
    db.save_view(
        alice,
        view,
        &SaveProjectView {
            operation_id: Uuid::new_v4(),
            expected_revision: 0,
            document: json!({}),
        },
    )
    .unwrap();
    db.save_editor(&record(alice, view, &registered)).unwrap();
    drop(db);
    let SandboxPrincipal::Host(a) = alice.principal else {
        unreachable!()
    };
    let SandboxPrincipal::Host(bob_id) = bob.principal else {
        unreachable!()
    };
    let path = format!(
        "/api/ai-sandbox/projects/{}/workspace/editors/{view}",
        b.project_id
    );
    assert_eq!(
        super::super::tests::call(&state, "GET", &path, json!({}), Some(a))
            .await
            .1["status"],
        "interrupted"
    );
    assert_eq!(
        super::super::tests::call(&state, "GET", &path, json!({}), Some(bob_id))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[test]
fn editor_bundles_preserve_legacy_routes_and_reject_untrusted_paths() {
    let b = registration();
    let editor = record(person(), Uuid::new_v4(), &b);
    let mut old = serde_json::to_value(&editor).unwrap();
    old.as_object_mut().unwrap().remove("bundle_revision");
    let legacy: Editor = serde_json::from_value(old).unwrap();
    assert!(legacy.bundle_revision.is_none());
    assert!(
        package::guest_package(legacy.bundle_revision.as_deref())
            .unwrap()
            .contains("openvscode-server-v")
    );
    let revision = package::revision().unwrap();
    assert!(
        package::guest_package(Some(&revision))
            .unwrap()
            .ends_with(&format!("/bundles/{revision}"))
    );
    for bad in [
        "",
        "../node",
        &"A".repeat(64),
        &"a".repeat(63),
        &format!("{};", "a".repeat(63)),
    ] {
        assert!(package::guest_package(Some(bad)).is_err());
    }
    assert_eq!(package::revision().unwrap(), revision);
}

#[tokio::test]
async fn editor_stop_is_private_idempotent_and_cannot_terminate_a_replacement() {
    let (_dir, state, b) = fixture().await;
    bind(&state, &b).await;
    let alice = person();
    let bob = person();
    let SandboxPrincipal::Host(alice_id) = alice.principal else {
        unreachable!()
    };
    let SandboxPrincipal::Host(bob_id) = bob.principal else {
        unreachable!()
    };
    for host in [alice_id, bob_id] {
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
    }
    let registered = binding(&state, b.project_id).unwrap();
    let db = open(&state, &registered).unwrap();
    let view = Uuid::new_v4();
    db.save_view(
        alice,
        view,
        &SaveProjectView {
            operation_id: Uuid::new_v4(),
            expected_revision: 0,
            document: json!({"retained":true}),
        },
    )
    .unwrap();
    let mut editor = record(alice, view, &registered);
    editor.status = "stopped".into();
    db.save_editor(&editor).unwrap();
    drop(db);
    let path = format!(
        "/api/ai-sandbox/projects/{}/workspace/editors/{view}",
        b.project_id
    );
    let stop = format!("{path}/stop");
    let body = json!({"expectedInstance":instance(&editor)});
    for _ in 0..2 {
        let result =
            super::super::tests::call(&state, "POST", &stop, body.clone(), Some(alice_id)).await;
        assert_eq!(result.0, StatusCode::OK, "{}", result.1);
        assert_eq!(result.1["status"], "stopped");
        assert_eq!(result.1["instance"], instance(&editor));
        assert!(!result.1.to_string().contains(&editor.attempt.to_string()));
    }
    assert_eq!(
        super::super::tests::call(&state, "GET", &path, json!({}), Some(alice_id))
            .await
            .1["status"],
        "stopped"
    );
    assert_eq!(
        super::super::tests::call(&state, "POST", &stop, body.clone(), Some(bob_id))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    for forged in [
        json!({"expectedInstance":"A".repeat(64)}),
        json!({"expectedInstance":instance(&editor),"caller":alice_id}),
        json!({}),
    ] {
        assert_eq!(
            super::super::tests::call(&state, "POST", &stop, forged, Some(alice_id))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    // An explicit reopen has a new native execution even if it reuses the file-opening request UUID.
    let previous = editor.clone();
    editor.attempt = Uuid::new_v4();
    editor.status = "ready".into();
    let db = open(&state, &registered).unwrap();
    db.save_editor(&editor).unwrap();
    drop(db);
    assert_ne!(instance(&previous), instance(&editor));
    let late = super::super::tests::call(&state, "POST", &stop, body, Some(alice_id)).await;
    assert_eq!(late.0, StatusCode::CONFLICT);
    assert!(late.1.to_string().contains("instance changed"));
    // A read-only grant can query completed state, but cannot admit a new stop.
    assert_eq!(
        super::super::tests::call(
            &state,
            "POST",
            &stop,
            json!({"expectedInstance":instance(&editor)}),
            Some(alice_id)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let db = open(&state, &registered).unwrap();
    let retained = db.editor(alice, view, true).unwrap();
    assert_eq!(retained.attempt, editor.attempt);
    assert_eq!(retained.status, "ready");
    assert_eq!(db.view(alice, view).unwrap().document["retained"], true);
}

pub(super) async fn private_gateway(
    state: AppState,
    host: &str,
    uri: &str,
    method: &str,
    body: serde_json::Value,
    caller: Option<Uuid>,
) -> (StatusCode, serde_json::Value) {
    use tower::ServiceExt;
    let app = Router::new()
        .fallback(|| async { "controller route" })
        .with_state(state.clone())
        .layer(axum::middleware::from_fn_with_state(state, proxy::dispatch))
        .layer(Extension(RequestId(Uuid::new_v4())));
    let mut request = Request::builder()
        .uri(uri)
        .method(method)
        .header("host", host)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    request
        .extensions_mut()
        .insert(crate::ai_sandbox::access::LocalOperator);
    if let Some(host_id) = caller {
        request.extensions_mut().insert(axum::extract::ConnectInfo(
            crate::tls::AuthenticatedHost { host_id },
        ));
    }
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn stopped_editor_drafts_remain_writable_without_opening_a_guest_or_exposing_other_callers() {
    let (_dir, state, b) = fixture().await;
    bind(&state, &b).await;
    let alice = person();
    let SandboxPrincipal::Host(alice_id) = alice.principal else {
        unreachable!()
    };
    state
        .store
        .save_sandbox_access(
            alice_id,
            &SandboxAccessPolicy {
                grants: vec![SandboxResourceGrant {
                    resource: SandboxResource::Project { id: b.project_id },
                    permissions: vec![SandboxPermission::Read, SandboxPermission::Write],
                }],
            },
        )
        .unwrap();
    let registered = binding(&state, b.project_id).unwrap();
    let db = open(&state, &registered).unwrap();
    let view = Uuid::new_v4();
    db.save_view(
        alice,
        view,
        &SaveProjectView {
            operation_id: Uuid::new_v4(),
            expected_revision: 0,
            document: json!({}),
        },
    )
    .unwrap();
    let mut editor = record(alice, view, &registered);
    editor.status = "stopped".into();
    db.save_editor(&editor).unwrap();
    drop(db);
    let host = proxy::host(b.project_id, editor.id);
    let request = json!({"operationId":Uuid::new_v4(),"expectedRevision":0,"baseVersion":0,"contentBase64":STANDARD.encode(b"private pending text")});
    let result = private_gateway(
        state.clone(),
        &host,
        "/_firecrab/draft?path=note.txt",
        "PUT",
        request.clone(),
        Some(alice_id),
    )
    .await;
    assert_eq!(result.0, StatusCode::OK, "{}", result.1);
    assert_eq!(result.1["revision"], 1);
    let replay = private_gateway(
        state.clone(),
        &host,
        "/_firecrab/draft?path=note.txt",
        "PUT",
        request,
        Some(alice_id),
    )
    .await;
    assert_eq!(replay.0, StatusCode::OK);
    assert_eq!(replay.1["revision"], 1);
    let result = private_gateway(
        state.clone(),
        &host,
        "/_firecrab/draft?path=note.txt",
        "GET",
        json!({}),
        Some(alice_id),
    )
    .await;
    assert_eq!(result.0, StatusCode::OK);
    assert_eq!(
        result.1["contentBase64"],
        STANDARD.encode(b"private pending text")
    );
    let result = private_gateway(
        state.clone(),
        &host,
        "/_firecrab/session",
        "GET",
        json!({}),
        Some(alice_id),
    )
    .await;
    assert_eq!(result.0, StatusCode::OK);
    assert_eq!(result.1["status"], "stopped");
    assert_eq!(
        private_gateway(
            state.clone(),
            &host,
            "/_firecrab/drafts",
            "GET",
            json!({}),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        private_gateway(
            state.clone(),
            &host,
            "/_firecrab/files?path=note.txt",
            "GET",
            json!({}),
            Some(alice_id)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    state
        .store
        .save_sandbox_access(alice_id, &SandboxAccessPolicy { grants: vec![] })
        .unwrap();
    assert_eq!(
        private_gateway(
            state.clone(),
            &host,
            "/_firecrab/drafts",
            "GET",
            json!({}),
            Some(alice_id)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let db = open(&state, &registered).unwrap();
    assert_eq!(db.draft(alice, view, "note.txt").unwrap().revision, 1);
    assert_eq!(db.editor(alice, view, true).unwrap().status, "stopped");
    let db_path = db.directory.join("project.db");
    drop(db);
    state
        .store
        .save_sandbox_access(
            alice_id,
            &SandboxAccessPolicy {
                grants: vec![SandboxResourceGrant {
                    resource: SandboxResource::Project { id: b.project_id },
                    permissions: vec![SandboxPermission::Read],
                }],
            },
        )
        .unwrap();
    let retained_path = db_path.with_extension("retained");
    fs::rename(&db_path, &retained_path).unwrap();
    let missing = private_gateway(
        state.clone(),
        &host,
        "/_firecrab/session",
        "GET",
        json!({}),
        Some(alice_id),
    )
    .await;
    assert_eq!(missing.0, StatusCode::CONFLICT, "{}", missing.1);
    fs::rename(retained_path, db_path).unwrap();
}

#[test]
fn maintenance_rechecks_the_current_execution_and_keeps_newer_open_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let b = registration();
    let db = super::super::db::ProjectDb::open(
        &dir.path().join("state/projects"),
        &b,
        Uuid::new_v4(),
        true,
    )
    .unwrap();
    let caller = person();
    let view = Uuid::new_v4();
    db.save_view(
        caller,
        view,
        &SaveProjectView {
            operation_id: Uuid::new_v4(),
            expected_revision: 0,
            document: json!({}),
        },
    )
    .unwrap();
    let observed = record(caller, view, &b);
    db.save_editor(&observed).unwrap();
    let mut latest = observed.clone();
    latest.file = Some("newer.txt".into());
    latest.open_request = Uuid::new_v4();
    latest.status = "stopping".into();
    db.save_editor(&latest).unwrap();
    let recovered = db.recovery_editor(&observed).unwrap().unwrap();
    assert_eq!(recovered.open_request, latest.open_request);
    assert_eq!(recovered.file, latest.file);
    assert_eq!(recovered.status, "stopping");
    latest.status = "stopped".into();
    db.save_editor(&latest).unwrap();
    assert!(db.recovery_editor(&observed).unwrap().is_none());
    latest.attempt = Uuid::new_v4();
    latest.status = "ready".into();
    db.save_editor(&latest).unwrap();
    assert!(db.recovery_editor(&observed).unwrap().is_none());
    assert_eq!(
        db.editor(caller, view, true).unwrap().attempt,
        latest.attempt
    );
}

#[tokio::test]
async fn an_ended_or_replaced_editor_cannot_keep_its_old_proxy_stream_authorized() {
    use axum::response::IntoResponse;
    let (_dir, state, b) = fixture().await;
    let caller = person();
    let original = record(caller, Uuid::new_v4(), &b);
    let key = (b.project_id, original.id);
    for status in ["stopping", "stopped", "interrupted"] {
        let mut ended = original.clone();
        ended.status = status.into();
        state
            .workspaces
            .editor_cache
            .lock()
            .unwrap()
            .insert(key, ended);
        let error = current(&state, caller, &b, &original, Uuid::new_v4()).unwrap_err();
        let response = error.into_response();
        let body = to_bytes(response.into_body(), 16384).await.unwrap();
        assert!(
            String::from_utf8(body.to_vec())
                .unwrap()
                .contains("execution ended or changed")
        );
    }
    let mut replacement = original.clone();
    replacement.attempt = Uuid::new_v4();
    replacement.status = "ready".into();
    state
        .workspaces
        .editor_cache
        .lock()
        .unwrap()
        .insert(key, replacement);
    let response = current(&state, caller, &b, &original, Uuid::new_v4())
        .unwrap_err()
        .into_response();
    let body = to_bytes(response.into_body(), 16384).await.unwrap();
    assert!(
        String::from_utf8(body.to_vec())
            .unwrap()
            .contains("execution ended or changed")
    );
}

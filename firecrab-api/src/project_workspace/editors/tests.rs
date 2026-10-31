use super::super::tests::{bind, fixture, registration};
use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::json;
fn person() -> Caller {
    serde_json::from_value(
        json!({"principal":SandboxPrincipal::Host(Uuid::new_v4()),"operator":false}),
    )
    .unwrap()
}
fn record(caller: Caller, view: Uuid, b: &Binding) -> Editor {
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
    let first = record(alice, view, &b);
    let second = record(bob, view, &b);
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

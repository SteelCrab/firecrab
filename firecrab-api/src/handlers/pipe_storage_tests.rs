use super::*;
use crate::{
    pipe_storage::tests::{fixture, input, keys, listing, record, reply},
    templates::TemplateRegistry,
};
use axum::{Router, body::Body, response::IntoResponse, routing::get};
use firecrab_api_types::{
    DeployMicroServiceRequest, MicroServiceDeployment, ServiceInstance, ServicePhase, VmState,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[test]
fn object_queries_preserve_form_spaces_literal_plus_percent_and_question_marks() {
    let uri = axum::http::Uri::from_static("/object?key=qa%2Fname+%2B%25%3F.bin");
    let Query(query) = Query::<ObjectQuery>::try_from_uri(&uri).unwrap();
    assert_eq!(query.key, "qa/name +%?.bin");
}

async fn state(dir: &std::path::Path) -> AppState {
    AppState::with_db_file(
        TemplateRegistry::from_specs(dir, std::iter::empty()).unwrap(),
        dir.join("state.db"),
    )
    .await
    .unwrap()
}

fn connect_image_account(state: &AppState) {
    state
        .store
        .put_pipe_auth(&crate::pipe_auth::SavedAuth::Github {
            username: "team".into(),
            token: "test-only-secret".into(),
        })
        .unwrap();
}

fn spec(connection: Uuid) -> MicroServiceSpec {
    serde_json::from_value(serde_json::json!({"name":"s3-app","image":"ghcr.io/team/app:v1","port":8080,"ram":128,"cpu":1,"storageConnectionId":connection})).unwrap()
}

#[tokio::test]
async fn crud_verifies_before_replacement_and_never_returns_credentials() {
    let root = tempfile::tempdir().unwrap();
    let mut state = state(root.path()).await;
    let fixture = fixture(vec![
        reply(StatusCode::OK, listing("", "")),
        reply(
            StatusCode::FORBIDDEN,
            "<Error><Code>AccessDenied</Code><Message>never-expose-test-secret</Message></Error>",
        ),
    ])
    .await;
    state.s3_test_origin = Some(fixture.origin.clone());
    let id = RequestId(Uuid::new_v4());
    let (_, Json(saved)) = create(State(state.clone()), Extension(id), ValidatedJson(input()))
        .await
        .unwrap();
    let public = serde_json::to_string(&saved).unwrap();
    assert!(!public.contains("never-expose"));
    let mut replacement = input();
    replacement.spec.name = "Changed".into();
    assert!(
        update(
            State(state.clone()),
            Extension(id),
            Path(saved.id.to_string()),
            ValidatedJson(replacement)
        )
        .await
        .is_err()
    );
    assert_eq!(
        state
            .store
            .pipe_storage(saved.id)
            .unwrap()
            .unwrap()
            .spec
            .name,
        "Test bucket"
    );
    let Json(rows) = list(State(state.clone()), Extension(id)).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert!(
        !serde_json::to_string(&rows)
            .unwrap()
            .contains("never-expose")
    );
    assert_eq!(
        delete(
            State(state.clone()),
            Extension(id),
            Path(saved.id.to_string())
        )
        .await
        .unwrap(),
        StatusCode::NO_CONTENT
    );
    assert!(state.store.list_pipe_storage().unwrap().is_empty());
    // Disconnect requires no upstream request and leaves external data alone.
}

#[tokio::test]
async fn services_block_disconnect_and_credential_values_stay_out_of_persisted_specs() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let saved = record();
    state.store.save_pipe_storage(&saved).unwrap();
    let request_id = RequestId(Uuid::new_v4());
    connect_image_account(&state);
    let (_, Json(service)) = super::super::micro_services::create(
        State(state.clone()),
        Extension(request_id),
        ValidatedJson(spec(saved.id)),
    )
    .await
    .unwrap();
    let Json(metadata) = super::get(
        State(state.clone()),
        Extension(request_id),
        Path(saved.id.to_string()),
    )
    .await
    .unwrap();
    assert_eq!(metadata.services[0].id, service.id);
    let response = delete(
        State(state.clone()),
        Extension(request_id),
        Path(saved.id.to_string()),
    )
    .await
    .unwrap_err()
    .into_response();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(
        update(
            State(state.clone()),
            Extension(request_id),
            Path(saved.id.to_string()),
            ValidatedJson(input())
        )
        .await
        .is_err()
    );
    let vm_id = Uuid::new_v4();
    let deployment = MicroServiceDeployment {
        recovery: None,
        started_at: 0,
        finished_at: None,
        update: None,
        id: Uuid::new_v4(),
        service_id: service.id,
        phase: ServicePhase::Running,
        spec: service.spec.clone(),
        request: DeployMicroServiceRequest {
            micro_network_id: None,
            host_port: 30180,
        },
        digest: None,
        instances: vec![ServiceInstance {
            vm_id,
            ipv4: "10.0.0.2".into(),
            host_port: 30180,
            state: VmState::Running,
            healthy: true,
        }],
        log: vec![],
        domain_route_owned: false,
        internal_route_owned: false,
        error: None,
    };
    state.store.save_service_deployment(&deployment).unwrap();
    let env = crate::pipe_storage::environment_for_vm(&state.store, vm_id).unwrap();
    assert_eq!(env["AWS_SECRET_ACCESS_KEY"], keys().secret_access_key);
    assert_eq!(env["FIRECRAB_S3_PREFIX"], "team/");
    assert!(
        crate::pipe_storage::environment_for_vm(&state.store, Uuid::new_v4())
            .unwrap()
            .is_empty()
    );
    assert!(
        !serde_json::to_string(&state.store.service_deployment(service.id).unwrap())
            .unwrap()
            .contains("never-expose")
    );
    assert!(
        !serde_json::to_string(&state.store.micro_service(service.id).unwrap())
            .unwrap()
            .contains("never-expose")
    );
}

#[tokio::test]
async fn service_bindings_require_existing_connections_and_reject_credential_overrides() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let saved = record();
    state.store.save_pipe_storage(&saved).unwrap();
    let id = Uuid::new_v4();
    assert!(
        validate_binding(&state, &spec(Uuid::new_v4()), id)
            .await
            .is_err()
    );
    for key in [
        "AWS_ACCESS_KEY_ID",
        "AWS_ENDPOINT_URL_S3",
        "FIRECRAB_S3_BUCKET",
    ] {
        let mut spec = spec(saved.id);
        spec.env.insert(key.into(), "override".into());
        assert!(validate_binding(&state, &spec, id).await.is_err());
    }
    assert!(validate_binding(&state, &spec(saved.id), id).await.is_ok());
}

#[tokio::test]
async fn shared_auth_cannot_be_removed_while_storage_references_it() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut saved = record();
    saved.credentials = None;
    saved.spec.credential_ref = StorageCredentialRef::PipeAuthAws;
    state.store.save_pipe_storage(&saved).unwrap();
    state
        .store
        .put_pipe_auth(&crate::pipe_auth::SavedAuth::Aws {
            account_id: "123456789012".into(),
            credentials: keys(),
        })
        .unwrap();
    let error = super::super::pipe_auth::delete(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(crate::pipe_auth::Provider::Aws),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    assert_eq!(state.store.pipe_auths().unwrap().len(), 1);
}

#[tokio::test]
async fn http_uploads_cross_the_json_limit_and_keep_the_streamed_download_length() {
    let root = tempfile::tempdir().unwrap();
    let mut state = state(root.path()).await;
    let data = "test-payload".repeat(20000);
    let fixture = fixture(vec![
        reply(StatusCode::OK, ""),
        reply(StatusCode::OK, data.clone()),
    ])
    .await;
    state.s3_test_origin = Some(fixture.origin.clone());
    let saved = record();
    state.store.save_pipe_storage(&saved).unwrap();
    let app = Router::new()
        .route("/storage/{id}/object", get(download).put(upload))
        .layer(Extension(RequestId(Uuid::new_v4())))
        .with_state(state);
    let uri = format!("/storage/{}/object?key=team%2Ffile", saved.id);
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("PUT")
                .uri(&uri)
                .header(header::CONTENT_LENGTH, data.len())
                .body(Body::from(data.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri(&uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_LENGTH],
        data.len().to_string()
    );
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        data.as_bytes()
    );
}

#[tokio::test]
async fn http_transfer_guards_do_not_contact_s3_and_expired_sessions_hide_upstream_secrets() {
    let root = tempfile::tempdir().unwrap();
    let mut state = state(root.path()).await;
    let mut fixture = fixture(vec![reply(
        StatusCode::BAD_REQUEST,
        "<Error><Code>ExpiredToken</Code><Message>never-expose-test-secret</Message></Error>",
    )])
    .await;
    state.s3_test_origin = Some(fixture.origin.clone());
    let saved = record();
    state.store.save_pipe_storage(&saved).unwrap();
    let app = Router::new()
        .route("/storage/{id}/object", get(download).put(upload))
        .layer(Extension(RequestId(Uuid::new_v4())))
        .with_state(state);
    let uri = format!("/storage/{}/object?key=team%2Ffile", saved.id);
    for (length, expected) in [
        (None, StatusCode::LENGTH_REQUIRED),
        (Some(MAX_OBJECT_BYTES + 1), StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let mut request = axum::http::Request::builder().method("PUT").uri(&uri);
        if let Some(length) = length {
            request = request.header(header::CONTENT_LENGTH, length);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        assert!(fixture.requests.try_recv().is_err());
    }
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri(&uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("storage_credentials_expired"));
    assert!(!body.contains("never-expose"));
    assert_eq!(
        fixture.requests.recv().await.unwrap().method,
        axum::http::Method::GET
    );
    assert!(fixture.requests.try_recv().is_err());
}

#[tokio::test]
async fn guest_credentials_fail_closed_when_bindings_are_lost_and_resolve_rotated_shared_keys() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut saved = record();
    state.store.save_pipe_storage(&saved).unwrap();
    connect_image_account(&state);
    let (_, Json(service)) = super::super::micro_services::create(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        ValidatedJson(spec(saved.id)),
    )
    .await
    .unwrap();
    let vm_id = Uuid::new_v4();
    state
        .store
        .save_service_deployment(&MicroServiceDeployment {
            recovery: None,
            started_at: 0,
            finished_at: None,
            update: None,
            id: Uuid::new_v4(),
            service_id: service.id,
            phase: ServicePhase::Running,
            spec: service.spec,
            request: DeployMicroServiceRequest {
                micro_network_id: None,
                host_port: 30180,
            },
            digest: None,
            instances: vec![ServiceInstance {
                vm_id,
                ipv4: "10.0.0.2".into(),
                host_port: 30180,
                state: VmState::Running,
                healthy: true,
            }],
            log: vec![],
            domain_route_owned: false,
            internal_route_owned: false,
            error: None,
        })
        .unwrap();

    // A missing connection or its signing material must stop boot, not omit the binding.
    state.store.delete_pipe_storage(saved.id).unwrap();
    assert!(crate::pipe_storage::environment_for_vm(&state.store, vm_id).is_err());
    saved.credentials = None;
    state.store.save_pipe_storage(&saved).unwrap();
    assert!(crate::pipe_storage::environment_for_vm(&state.store, vm_id).is_err());
    saved.spec.credential_ref = StorageCredentialRef::PipeAuthAws;
    state.store.save_pipe_storage(&saved).unwrap();
    assert!(crate::pipe_storage::environment_for_vm(&state.store, vm_id).is_err());

    for secret in ["original-shared-secret", "rotated-shared-secret"] {
        let mut credentials = keys();
        credentials.secret_access_key = secret.into();
        state
            .store
            .put_pipe_auth(&crate::pipe_auth::SavedAuth::Aws {
                account_id: "123456789012".into(),
                credentials,
            })
            .unwrap();
        let env = crate::pipe_storage::environment_for_vm(&state.store, vm_id).unwrap();
        assert_eq!(env["AWS_SECRET_ACCESS_KEY"], secret);
        assert_eq!(env["FIRECRAB_S3_BUCKET"], saved.spec.bucket);
    }
}

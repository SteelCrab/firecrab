use super::*;
use crate::{
    ai_sandbox::access::LocalOperator,
    server::{HttpConfig, build_router, ci_deploy_router},
    templates::TemplateRegistry,
};
use axum::{Router, body::Body, http::Request};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn fixture(root: &std::path::Path) -> (AppState, MicroServiceDeployment, Router, Router) {
    let templates = TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
    let state = AppState::with_db_file(templates, root.join("state.db"))
        .await
        .unwrap();
    state
        .store
        .put_pipe_auth(&crate::pipe_auth::SavedAuth::Github {
            username: "team".into(),
            token: "fixture-registry-secret".into(),
        })
        .unwrap();
    let record = running();
    state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id: record.service_id,
            spec: record.spec.clone(),
        })
        .unwrap();
    state.store.save_service_deployment(&record).unwrap();
    let config = HttpConfig {
        bind_addr: "127.0.0.1:3000".parse().unwrap(),
        allowed_origins: vec![],
        max_concurrent_requests: 128,
        request_timeout: std::time::Duration::from_secs(10),
        static_root: None,
    };
    let operator = build_router(state.clone(), &config).layer(Extension(LocalOperator));
    let ci = ci_deploy_router(state.clone());
    (state, record, operator, ci)
}

fn running() -> MicroServiceDeployment {
    serde_json::from_value(json!({"id":Uuid::new_v4(),"serviceId":Uuid::new_v4(),"phase":"running","spec":{
        "name":"orders","image":"ghcr.io/team/app:v1","port":8080,"ram":256,"cpu":1,"domain":"orders.example.test",
        "env":{"PRIVATE_VALUE":"do-not-disclose"},"secrets":{"PASSWORD":Uuid::new_v4()}},
        "request":{"hostPort":0},"digest":format!("sha256:{}","a".repeat(64)),"instances":[],"log":["private-runtime-log"],
        "domainRouteOwned":true,"error":"private-error","startedAt":0,"finishedAt":0})).unwrap()
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    assert!(
        response.headers()[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("no-store")
    );
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}

async fn issue(operator: &Router, service: Uuid) -> CreatedDeployToken {
    let (status, body) = call(
        operator,
        "POST",
        &format!("/api/micro-services/{service}/deploy-tokens"),
        None,
        json!({"name":"CI"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    serde_json::from_value(body).unwrap()
}

fn request(previous: &MicroServiceDeployment) -> CiDeployRequest {
    CiDeployRequest {
        operation_id: Uuid::new_v4(),
        expected_release_id: previous.id,
        image: format!("ghcr.io/team/app@sha256:{}", "b".repeat(64)),
        strategy: DeployStrategy::Rolling,
    }
}

fn queued(previous: &MicroServiceDeployment, request: &CiDeployRequest) -> MicroServiceDeployment {
    let mut record = previous.clone();
    record.id = request.operation_id;
    record.phase = ServicePhase::Queued;
    record.spec.image = request.image.clone();
    record.digest = None;
    record.update = Some(Box::new(ServiceUpdate {
        previous: Box::new(previous.clone()),
        requested_spec: record.spec.clone(),
        strategy: request.strategy,
        rollback_of: None,
        restoring: false,
        routed_new: false,
        transition: None,
        retiring: vec![],
        completed: false,
        failure: None,
        cancelled: false,
    }));
    record
}

#[tokio::test]
async fn credentials_are_hash_only_restart_safe_revocable_and_bound_to_one_service() {
    let root = tempfile::tempdir().unwrap();
    let (state, record, operator, ci) = fixture(root.path()).await;
    let created = issue(&operator, record.service_id).await;
    assert_eq!(created.token.len(), 80);
    let saved = state
        .store
        .deploy_token(created.credential.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        saved.hash,
        Sha256::digest(created.token.as_bytes()).as_slice()
    );
    let path = format!("/api/ci/services/{}/deployments", record.service_id);
    let (status, body) = call(&ci, "GET", &path, Some(&created.token), Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    for secret in [
        "PRIVATE_VALUE",
        "do-not-disclose",
        "PASSWORD",
        "private-runtime-log",
        "private-error",
        &created.token,
    ] {
        assert!(!body.to_string().contains(secret));
    }
    let (_, listed) = call(
        &operator,
        "GET",
        &format!("/api/micro-services/{}/deploy-tokens", record.service_id),
        None,
        Value::Null,
    )
    .await;
    assert_eq!(listed[0]["repository"], "ghcr.io/team/app");
    assert!(!listed.to_string().contains(&created.token));
    let restarted = AppState::with_db_file(
        TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap(),
        root.path().join("state.db"),
    )
    .await
    .unwrap();
    let restarted_router = ci_deploy_router(restarted);
    assert_eq!(
        call(
            &restarted_router,
            "GET",
            &path,
            Some(&created.token),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &ci,
            "GET",
            &format!("/api/ci/services/{}/deployments", Uuid::new_v4()),
            Some(&created.token),
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for bad in [
        None,
        Some("fcd_bad"),
        Some(&created.token.replace("fcd_", "bad_")),
    ] {
        assert_eq!(
            call(&ci, "GET", &path, bad, Value::Null).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    let mut wrong = created.token.clone();
    wrong.replace_range(79..80, if wrong.ends_with('A') { "B" } else { "A" });
    assert_eq!(
        call(&ci, "GET", &path, Some(&wrong), Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    let revoke = format!(
        "/api/micro-services/{}/deploy-tokens/{}",
        record.service_id, created.credential.id
    );
    assert_eq!(
        call(&operator, "DELETE", &revoke, None, Value::Null)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&operator, "DELETE", &revoke, None, Value::Null)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&ci, "GET", &path, Some(&created.token), Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn ci_cannot_reach_operator_paths_change_configuration_or_submit_mutable_images() {
    let root = tempfile::tempdir().unwrap();
    let (_, record, operator, ci) = fixture(root.path()).await;
    let created = issue(&operator, record.service_id).await;
    for path in [
        "/api/vms".into(),
        "/api/secrets".into(),
        format!("/api/micro-services/{}/deploy-tokens", record.service_id),
        format!("/api/micro-services/{}/updates", record.service_id),
    ] {
        assert_eq!(
            call(&ci, "POST", &path, Some(&created.token), json!({}))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }
    let path = format!("/api/ci/services/{}/deployments", record.service_id);
    let mut body = serde_json::to_value(request(&record)).unwrap();
    body["env"] = json!({"SHOULD_NOT_CHANGE":"yes"});
    assert_eq!(
        call(&ci, "POST", &path, Some(&created.token), body).await.0,
        StatusCode::BAD_REQUEST
    );
    for image in [
        "ghcr.io/team/app:latest",
        &format!("ghcr.io/team/other@sha256:{}", "b".repeat(64)),
    ] {
        let mut body = request(&record);
        body.image = image.into();
        assert_eq!(
            call(
                &ci,
                "POST",
                &path,
                Some(&created.token),
                serde_json::to_value(body).unwrap()
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    let manual = format!("{path}/{}", record.id);
    assert_eq!(
        call(&ci, "GET", &manual, Some(&created.token), Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn token_management_requires_verified_operator_and_bounds_names_lifetimes_and_count() {
    let root = tempfile::tempdir().unwrap();
    let (state, record, operator, _) = fixture(root.path()).await;
    let path = format!("/api/micro-services/{}/deploy-tokens", record.service_id);
    for body in [
        json!({"name":""}),
        json!({"name":"a\nb"}),
        json!({"name":"CI","expiresInSeconds":59}),
        json!({"name":"CI","expiresInSeconds":90*86400+1}),
    ] {
        assert_eq!(
            call(&operator, "POST", &path, None, body).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let config = HttpConfig {
        bind_addr: "127.0.0.1:3000".parse().unwrap(),
        allowed_origins: vec![],
        max_concurrent_requests: 128,
        request_timeout: std::time::Duration::from_secs(10),
        static_root: None,
    };
    assert_eq!(
        call(
            &build_router(state.clone(), &config),
            "POST",
            &path,
            None,
            json!({"name":"CI"})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    for _ in 0..20 {
        issue(&operator, record.service_id).await;
    }
    assert_eq!(
        call(&operator, "POST", &path, None, json!({"name":"overflow"}))
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn authorization_and_exact_request_commit_atomically_and_delete_cleans_credentials() {
    let root = tempfile::tempdir().unwrap();
    let (state, previous, operator, ci) = fixture(root.path()).await;
    let created = issue(&operator, previous.service_id).await;
    let body = request(&previous);
    let record = queued(&previous, &body);
    let admission = Admission {
        token_id: created.credential.id,
        request: body.clone(),
    };
    state.store.save_ci_deployment(&record, &admission).unwrap();
    assert_eq!(
        state
            .store
            .ci_request(record.service_id, admission.token_id, record.id)
            .unwrap(),
        Some(body.clone())
    );
    let path = format!(
        "/api/ci/services/{}/deployments/{}",
        record.service_id, record.id
    );
    let (status, result) = call(&ci, "GET", &path, Some(&created.token), Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["completed"], false);
    assert!(!result.to_string().contains("do-not-disclose"));
    let changed = request(&previous);
    let next = queued(&previous, &changed);
    state
        .store
        .revoke_deploy_token(record.service_id, admission.token_id)
        .unwrap();
    assert!(
        state
            .store
            .save_ci_deployment(
                &next,
                &Admission {
                    token_id: admission.token_id,
                    request: changed
                }
            )
            .is_err()
    );
    assert!(state.store.service_attempt(next.id).unwrap().is_none());
    assert!(
        state
            .store
            .ci_request(next.service_id, admission.token_id, next.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        state
            .store
            .service_deployment(record.service_id)
            .unwrap()
            .unwrap()
            .id,
        record.id
    );
    state.store.delete_micro_service(record.service_id).unwrap();
    assert!(
        state
            .store
            .deploy_tokens(record.service_id)
            .unwrap()
            .is_empty()
    );
    assert!(
        state
            .store
            .ci_request(record.service_id, admission.token_id, record.id)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn expired_credentials_and_changed_scope_cannot_commit_or_poll() {
    let root = tempfile::tempdir().unwrap();
    let (state, previous, operator, ci) = fixture(root.path()).await;
    let created = issue(&operator, previous.service_id).await;
    let hash = state
        .store
        .deploy_token(created.credential.id)
        .unwrap()
        .unwrap()
        .hash;
    state
        .store
        .revoke_deploy_token(previous.service_id, created.credential.id)
        .unwrap();
    let mut expired = created.credential;
    expired.expires_at = 0;
    state.store.insert_deploy_token(&expired, &hash).unwrap();
    assert_eq!(
        call(
            &ci,
            "GET",
            &format!("/api/ci/services/{}/deployments", previous.service_id),
            Some(&created.token),
            Value::Null
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let body = request(&previous);
    let record = queued(&previous, &body);
    assert!(
        state
            .store
            .save_ci_deployment(
                &record,
                &Admission {
                    token_id: expired.id,
                    request: body
                }
            )
            .is_err()
    );
    assert_eq!(
        state
            .store
            .service_deployment(previous.service_id)
            .unwrap()
            .unwrap()
            .id,
        previous.id
    );
}

#[test]
fn failed_exact_attempt_stays_failed_after_the_previous_release_is_restored() {
    let previous = running();
    let body = request(&previous);
    let mut failed = queued(&previous, &body);
    failed.phase = ServicePhase::Failed;
    failed.update.as_mut().unwrap().completed = true;
    failed.update.as_mut().unwrap().failure = Some("private runtime failure".into());
    let status = outcome(&failed, &body);
    assert!(status.completed);
    assert!(!status.succeeded);
    let raw = serde_json::to_string(&status).unwrap();
    assert!(!raw.contains("private"));
}

#[tokio::test]
async fn terminal_receipts_survive_stop_history_pruning_restart_and_later_recovery() {
    let root = tempfile::tempdir().unwrap();
    let (state, previous, operator, ci) = fixture(root.path()).await;
    let created = issue(&operator, previous.service_id).await;
    let body = request(&previous);
    let mut record = queued(&previous, &body);
    let admission = Admission {
        token_id: created.credential.id,
        request: body.clone(),
    };
    state.store.save_ci_deployment(&record, &admission).unwrap();
    record.phase = ServicePhase::Running;
    record.digest = Some(format!("sha256:{}", "b".repeat(64)));
    record.update.as_mut().unwrap().completed = true;
    state.store.save_service_deployment(&record).unwrap();
    let path = format!(
        "/api/ci/services/{}/deployments/{}",
        record.service_id, record.id
    );
    record.phase = ServicePhase::Stopped;
    record.update = None;
    state.store.save_service_deployment(&record).unwrap();
    assert_eq!(
        call(&ci, "GET", &path, Some(&created.token), Value::Null)
            .await
            .1["succeeded"],
        true
    );
    for _ in 0..22 {
        record.id = Uuid::new_v4();
        record.phase = ServicePhase::Running;
        state.store.save_service_deployment(&record).unwrap();
    }
    assert!(
        !state
            .store
            .service_releases(record.service_id)
            .unwrap()
            .iter()
            .any(|r| r.id == body.operation_id)
    );
    let restarted = AppState::with_db_file(
        TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap(),
        root.path().join("state.db"),
    )
    .await
    .unwrap();
    assert_eq!(
        call(
            &ci_deploy_router(restarted),
            "GET",
            &path,
            Some(&created.token),
            Value::Null
        )
        .await
        .1["succeeded"],
        true
    );
    let failed_request = request(&record);
    let mut failed = queued(&record, &failed_request);
    state
        .store
        .save_ci_deployment(
            &failed,
            &Admission {
                token_id: admission.token_id,
                request: failed_request.clone(),
            },
        )
        .unwrap();
    failed.phase = ServicePhase::Failed;
    failed.update.as_mut().unwrap().failure = Some("private failure".into());
    failed.update.as_mut().unwrap().completed = true;
    state
        .store
        .restore_service_release(&failed, &record)
        .unwrap();
    failed.phase = ServicePhase::Running;
    failed.update.as_mut().unwrap().failure = None;
    state.store.save_service_deployment(&failed).unwrap();
    let receipt = state
        .store
        .ci_result(record.service_id, admission.token_id, failed.id)
        .unwrap()
        .unwrap();
    assert!(receipt.completed);
    assert!(!receipt.succeeded);
    assert_eq!(receipt.phase, ServicePhase::Failed);
}

#[tokio::test]
async fn resubmission_uses_original_inputs_and_ownership_changes_fence_atomic_admission() {
    let root = tempfile::tempdir().unwrap();
    let (state, previous, operator, ci) = fixture(root.path()).await;
    let created = issue(&operator, previous.service_id).await;
    let body = request(&previous);
    let mut record = queued(&previous, &body);
    state
        .store
        .save_ci_deployment(
            &record,
            &Admission {
                token_id: created.credential.id,
                request: body.clone(),
            },
        )
        .unwrap();
    record.phase = ServicePhase::Running;
    record.digest = Some(format!("sha256:{}", "b".repeat(64)));
    record.update.as_mut().unwrap().completed = true;
    state.store.save_service_deployment(&record).unwrap();
    let path = format!("/api/ci/services/{}/deployments", record.service_id);
    assert_eq!(
        call(
            &ci,
            "POST",
            &path,
            Some(&created.token),
            serde_json::to_value(&body).unwrap()
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    let mut changed = body.clone();
    changed.strategy = DeployStrategy::BlueGreen;
    assert_eq!(
        call(
            &ci,
            "POST",
            &path,
            Some(&created.token),
            serde_json::to_value(changed).unwrap()
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let mut stale = request(&record);
    stale.expected_release_id = previous.id;
    assert_eq!(
        call(
            &ci,
            "POST",
            &path,
            Some(&created.token),
            serde_json::to_value(stale).unwrap()
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let connection = rusqlite::Connection::open(root.path().join("state.db")).unwrap();
    connection
        .execute(
            "INSERT INTO cluster_service_workers VALUES (?1,'{}')",
            [record.service_id.to_string()],
        )
        .unwrap();
    assert_eq!(
        call(
            &ci,
            "POST",
            &path,
            Some(&created.token),
            serde_json::to_value(&body).unwrap()
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let next = request(&record);
    let next_record = queued(&record, &next);
    assert!(
        state
            .store
            .save_ci_deployment(
                &next_record,
                &Admission {
                    token_id: created.credential.id,
                    request: next
                }
            )
            .is_err()
    );
    assert!(
        state
            .store
            .service_attempt(next_record.id)
            .unwrap()
            .is_none()
    );
    connection
        .execute("DELETE FROM cluster_service_workers", [])
        .unwrap();
    let stack = state.store.register_stack("git-owner").unwrap();
    state
        .store
        .bind_stack_resource(
            "git-owner",
            &firecrab_stack::Binding {
                kind: firecrab_stack::ResourceKind::Service,
                key: "app".into(),
                resource_id: record.service_id,
            },
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO stack_git_sources VALUES (?1,'{}')",
            [stack.id.to_string()],
        )
        .unwrap();
    assert_eq!(
        call(
            &ci,
            "POST",
            &path,
            Some(&created.token),
            serde_json::to_value(body).unwrap()
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let next = request(&record);
    let next_record = queued(&record, &next);
    assert!(
        state
            .store
            .save_ci_deployment(
                &next_record,
                &Admission {
                    token_id: created.credential.id,
                    request: next
                }
            )
            .is_err()
    );
    assert!(
        state
            .store
            .service_attempt(next_record.id)
            .unwrap()
            .is_none()
    );
}

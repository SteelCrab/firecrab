use super::*;
mod fixture;
mod relay;
use axum::body::{Body, to_bytes};
use axum::http::Request;
use fixture::*;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt;
fn body(f: &Fixture) -> CodeAgentRunRequest {
    CodeAgentRunRequest {
        expected_revision: 1,
        vm_id: f.record.runtime.vm_id,
        runtime_id: f.record.runtime.runtime_id.unwrap(),
        operation_id: f.record.runtime.operation_id,
        model: "fixture-model".into(),
        prompt: "Create the requested file".into(),
    }
}
struct Done(AtomicUsize);
impl Driver for Done {
    async fn execute(
        &self,
        _: &AppState,
        c: &Context,
        r: &mut CodeAgentRun,
    ) -> Result<Outcome, &'static str> {
        assert_eq!(
            c.snapshot.ai.as_ref().unwrap().api_key,
            "host-only-synthetic-key"
        );
        assert_eq!(
            r.connection_id,
            c.snapshot
                .workspace
                .spec
                .agent
                .as_ref()
                .unwrap()
                .connection_id
        );
        self.0.fetch_add(1, Ordering::SeqCst);
        r.provider_requests = 1;
        r.output = "File created in the workspace".into();
        Ok(Outcome {
            code: 0,
            failed: false,
        })
    }
}
fn driver() -> &'static Done {
    Box::leak(Box::new(Done(AtomicUsize::new(0))))
}
async fn start(
    f: &Fixture,
    id: Uuid,
    b: CodeAgentRunRequest,
    d: &'static impl Driver,
) -> Result<CodeAgentRun, AppError> {
    requests::admit(f.state.clone(), f.id, id, b, RequestId(Uuid::new_v4()), d)
        .await
        .map(|(_, Json(r))| r)
}
async fn phase(f: &Fixture, id: Uuid, expected: Phase) -> CodeAgentRun {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let r = f.state.store.code_agent_run(f.id, id).unwrap().unwrap().0;
            if r.phase == expected {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("phase transition")
}
async fn http(f: &Fixture, method: &str, suffix: &str) -> (StatusCode, Value) {
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!("/api/code/workspaces/{}/agent-runs{suffix}", f.id))
                .header("host", "127.0.0.1:5523")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn code_run_same_id_is_single_use_even_after_success_and_different_payload_conflicts() {
    let f = fixture().await;
    let d = driver();
    let id = Uuid::new_v4();
    let b = body(&f);
    assert_eq!(
        start(&f, id, b.clone(), d).await.unwrap().phase,
        Phase::Queued
    );
    assert_eq!(start(&f, id, b.clone(), d).await.unwrap().id, id);
    let mut different = b.clone();
    different.prompt.push('!');
    assert!(start(&f, id, different, d).await.is_err());
    assert!(start(&f, Uuid::new_v4(), b.clone(), d).await.is_err());
    f.state.code_jobs.add_permits(1);
    phase(&f, id, Phase::Succeeded).await;
    assert_eq!(start(&f, id, b, d).await.unwrap().phase, Phase::Succeeded);
    assert_eq!(d.0.load(Ordering::SeqCst), 1);
    assert_eq!(http(&f, "GET", "").await.1.as_array().unwrap().len(), 1);
    let stored = std::fs::read(f._dir.path().join("state.db")).unwrap();
    assert!(!String::from_utf8_lossy(&stored).contains("Create the requested file"));
}
#[tokio::test]
async fn code_run_cancel_before_admission_fences_a_delayed_post() {
    let f = fixture().await;
    let id = Uuid::new_v4();
    let (status, value) = http(&f, "POST", &format!("/{id}/cancel")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["id"], id.to_string());
    assert!(value["run"].is_null());
    assert!(start(&f, id, body(&f), driver()).await.is_err());
    assert!(f.state.store.code_agent_runs(f.id).unwrap().is_empty());
}
#[tokio::test]
async fn code_run_cancel_stops_a_queued_worker_and_never_overwrites_cancellation() {
    let f = fixture().await;
    let d = driver();
    let id = Uuid::new_v4();
    let r = start(&f, id, body(&f), d).await.unwrap();
    http(&f, "POST", &format!("/{id}/cancel")).await;
    phase(&f, id, Phase::Cancelled).await;
    assert!(!f.state.store.save_code_agent_run(&r).unwrap());
    f.state.code_jobs.add_permits(1);
    tokio::task::yield_now().await;
    assert_eq!(d.0.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn code_run_stale_revision_and_vm_identity_are_rejected_without_reservation() {
    let f = fixture().await;
    for field in 0..4 {
        let mut b = body(&f);
        match field {
            0 => b.expected_revision = 2,
            1 => b.vm_id = Uuid::new_v4(),
            2 => b.runtime_id = Uuid::new_v4(),
            _ => b.operation_id = Uuid::new_v4(),
        };
        assert!(start(&f, Uuid::new_v4(), b, driver()).await.is_err());
    }
    assert!(f.state.store.code_agent_runs(f.id).unwrap().is_empty());
}
#[tokio::test]
async fn code_run_auth_rotation_disconnect_and_runtime_change_interrupt_before_any_inference() {
    for change in 0..3 {
        let f = fixture().await;
        let d = driver();
        let id = Uuid::new_v4();
        start(&f, id, body(&f), d).await.unwrap();
        let snapshot = f
            .state
            .store
            .code_verification_snapshot(f.id)
            .unwrap()
            .unwrap();
        let mut saved = snapshot.ai.unwrap();
        match change {
            0 => {
                saved.api_key = "rotated-synthetic-key".into();
                f.state.store.save_ai_connection(&saved).unwrap();
            }
            1 => f
                .state
                .store
                .delete_ai_connection(saved.connection.id)
                .unwrap(),
            _ => {
                let mut r = f.record.clone();
                r.runtime.runtime_id = Some(Uuid::new_v4());
                assert!(f.state.store.save_code_runtime(&r).unwrap());
            }
        }
        phase(&f, id, Phase::Interrupted).await;
        assert_eq!(d.0.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn code_run_github_disconnect_does_not_replace_or_interrupt_assigned_ai_auth() {
    let f = fixture().await;
    let d = driver();
    let id = Uuid::new_v4();
    start(&f, id, body(&f), d).await.unwrap();
    f.state
        .store
        .delete_pipe_auth(crate::pipe_auth::Provider::Github)
        .unwrap();
    f.state.code_jobs.add_permits(1);
    phase(&f, id, Phase::Succeeded).await;
    assert_eq!(d.0.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn code_run_restart_repairs_observation_without_replaying_a_paid_request() {
    let f = fixture().await;
    let d = driver();
    let first = Uuid::new_v4();
    let mut r = start(&f, first, body(&f), d).await.unwrap();
    http(&f, "POST", &format!("/{first}/cancel")).await;
    phase(&f, first, Phase::Cancelled).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    r.id = Uuid::new_v4();
    r.phase = Phase::Running;
    let snapshot = f
        .state
        .store
        .code_verification_snapshot(f.id)
        .unwrap()
        .unwrap();
    assert!(
        f.state
            .store
            .admit_code_agent_run(&snapshot, &f.record, &r, &hash(&body(&f)))
            .unwrap()
    );
    let (status, value) = http(&f, "GET", &format!("/{}", r.id)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["phase"], "interrupted");
    assert_eq!(
        start(&f, r.id, body(&f), d).await.unwrap().phase,
        Phase::Interrupted
    );
    assert_eq!(d.0.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn code_run_endpoints_require_the_controller_operator() {
    let f = fixture().await;
    let app = routes()
        .with_state(f.state.clone())
        .layer(Extension(RequestId(Uuid::new_v4())));
    for method in ["GET", "POST"] {
        let path = if method == "GET" {
            format!("/api/code/workspaces/{}/agent-runs", f.id)
        } else {
            format!(
                "/api/code/workspaces/{}/agent-runs/{}/cancel",
                f.id,
                Uuid::new_v4()
            )
        };
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ));
    }
    assert!(f.state.store.code_agent_runs(f.id).unwrap().is_empty());
}

#[tokio::test]
async fn code_run_supports_all_four_verified_assignments_in_one_retained_vm() {
    for profile in code_agent_auth::catalog()
        .into_iter()
        .filter(|p| p.supported)
    {
        let f = fixture().await;
        let snapshot = f
            .state
            .store
            .code_verification_snapshot(f.id)
            .unwrap()
            .unwrap();
        let mut saved = snapshot.ai.unwrap();
        saved.connection.provider = profile.provider;
        saved.connection.endpoint = match profile.provider {
            AiProvider::Openai => crate::pipe_ai::ENDPOINT,
            AiProvider::Anthropic => crate::pipe_ai::anthropic::CREDENTIAL_ENDPOINT,
        }
        .into();
        f.state.store.save_ai_connection(&saved).unwrap();
        let mut spec = snapshot.workspace.spec;
        spec.agent.as_mut().unwrap().agent = profile.agent;
        f.state
            .store
            .write_code_workspace(
                f.id,
                CodeWorkspaceWrite {
                    expected_revision: 1,
                    spec,
                },
                2,
            )
            .unwrap();
        let mut b = body(&f);
        b.expected_revision = f
            .state
            .store
            .code_workspace(f.id)
            .unwrap()
            .unwrap()
            .revision;
        b.model.clear();
        let id = Uuid::new_v4();
        let r = start(&f, id, b, driver()).await.unwrap();
        assert_eq!(r.agent, profile.agent);
        assert_eq!(r.adapter_id, profile.id);
        assert_eq!(r.vm_id, f.record.runtime.vm_id);
        http(&f, "POST", &format!("/{id}/cancel")).await;
        phase(&f, id, Phase::Cancelled).await;
    }
}
#[tokio::test]
async fn code_run_unverified_chatgpt_and_changed_checkout_are_blocked() {
    for chatgpt in [true, false] {
        let f = fixture().await;
        let snapshot = f
            .state
            .store
            .code_verification_snapshot(f.id)
            .unwrap()
            .unwrap();
        let mut b = body(&f);
        if chatgpt {
            let mut saved = snapshot.ai.unwrap();
            saved.connection.authentication = AiAuthentication::Chatgpt;
            f.state.store.save_ai_connection(&saved).unwrap();
        } else {
            let mut spec = snapshot.workspace.spec;
            spec.github.branch = "other-branch".into();
            f.state
                .store
                .write_code_workspace(
                    f.id,
                    CodeWorkspaceWrite {
                        expected_revision: 1,
                        spec,
                    },
                    2,
                )
                .unwrap();
            b.expected_revision = 2;
        }
        assert!(start(&f, Uuid::new_v4(), b, driver()).await.is_err());
        assert!(f.state.store.code_agent_runs(f.id).unwrap().is_empty());
    }
}
struct Rotating;
impl Driver for Rotating {
    async fn execute(
        &self,
        state: &AppState,
        c: &Context,
        r: &mut CodeAgentRun,
    ) -> Result<Outcome, &'static str> {
        let mut saved = c.snapshot.ai.clone().unwrap();
        saved.api_key = "rotated-at-completion".into();
        state.store.save_ai_connection(&saved).unwrap();
        r.provider_requests = 1;
        Ok(Outcome {
            code: 0,
            failed: false,
        })
    }
}
#[tokio::test]
async fn code_run_final_receipt_cannot_claim_success_after_auth_changed() {
    let f = fixture().await;
    let id = Uuid::new_v4();
    start(&f, id, body(&f), &Rotating).await.unwrap();
    f.state.code_jobs.add_permits(1);
    phase(&f, id, Phase::Interrupted).await;
}
#[tokio::test]
async fn code_run_rejects_host_command_fields_and_credentials_in_model_names() {
    let f = fixture().await;
    let mut b = body(&f);
    b.model = "host-only-synthetic-key".into();
    assert!(start(&f, Uuid::new_v4(), b, driver()).await.is_err());
    let mut value = serde_json::to_value(body(&f)).unwrap();
    value["command"] = json!("untrusted host command");
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/code/workspaces/{}/agent-runs/{}",
                    f.id,
                    Uuid::new_v4()
                ))
                .header("content-type", "application/json")
                .body(Body::from(value.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        response.status(),
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
    ));
    assert!(f.state.store.code_agent_runs(f.id).unwrap().is_empty());
}

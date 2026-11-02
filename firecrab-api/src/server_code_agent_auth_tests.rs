use super::*;
use crate::{
    code_agent_auth::{self, Probe},
    handlers::code_workspaces::agent_auth,
    persistence::code_runtime::{Admission, Record},
};

fn ready(f: &Fixture) -> Record {
    let pin = SandboxTemplatePin {
        name: "dev".into(),
        version: "v1".into(),
        kernel_sha256: "a".repeat(64),
        rootfs_sha256: "b".repeat(64),
        boot_args_sha256: "c".repeat(64),
        initrd_sha256: None,
    };
    let Admission::Accepted(mut r) = f
        .state
        .store
        .admit_code_runtime(
            f.id,
            f.state
                .store
                .code_workspace(f.id)
                .unwrap()
                .unwrap()
                .revision,
            Some(CodeRuntimePlacement {
                template: pin,
                micro_network_id: Uuid::new_v4(),
                storage_root: "default".into(),
            }),
            CodeRuntimeAction::Start,
        )
        .unwrap()
    else {
        panic!("admission")
    };
    r.vm_created = true;
    r.runtime.phase = CodeRuntimePhase::Ready;
    r.runtime.runtime_id = Some(Uuid::new_v4());
    r.runtime.initial_commit = Some(SHA.into());
    assert!(f.state.store.save_code_runtime(&r).unwrap());
    *r
}
struct Fake {
    change: u8,
}
impl Probe for Fake {
    async fn check(
        &self,
        state: &AppState,
        r: &Record,
        _: &CodeAgentAuthAdapter,
        saved: &SavedConnection,
    ) -> Result<(), CodeAgentAuthFailure> {
        assert_eq!(saved.api_key, "code-private-api-key");
        match self.change {
            1 => state
                .store
                .delete_ai_connection(saved.connection.id)
                .unwrap(),
            2 => {
                let mut changed = saved.clone();
                changed.api_key = "code-private-rotated".into();
                state.store.save_ai_connection(&changed).unwrap();
            }
            3 => {
                let mut changed = r.clone();
                changed.runtime.runtime_id = Some(Uuid::new_v4());
                assert!(state.store.save_code_runtime(&changed).unwrap());
            }
            _ => {}
        }
        Ok(())
    }
}
#[tokio::test]
async fn code_agent_auth_combines_provider_proof_with_revision_and_runtime_bound_cli_proof() {
    for profile in code_agent_auth::catalog()
        .into_iter()
        .filter(|p| p.supported)
    {
        let f = fixture().await;
        f.state
            .store
            .save_ai_connection(&account(f.account, profile.provider))
            .unwrap();
        let mut spec = f.state.store.code_workspace(f.id).unwrap().unwrap().spec;
        spec.agent = Some(CodeAgentBinding {
            agent: profile.agent,
            connection_id: f.account,
        });
        assert!(matches!(
            f.state
                .store
                .write_code_workspace(
                    f.id,
                    CodeWorkspaceWrite {
                        expected_revision: 1,
                        spec
                    },
                    2
                )
                .unwrap(),
            WriteResult::Written(_)
        ));
        let record = ready(&f);
        let snapshot = f
            .state
            .store
            .code_verification_snapshot(f.id)
            .unwrap()
            .unwrap();
        let result = agent_auth::check(
            &f.state,
            snapshot,
            record.clone(),
            profile.clone(),
            &Fake { change: 0 },
            RequestId(Uuid::new_v4()),
        )
        .await
        .unwrap();
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["status"], "configuration_verified");
        assert_eq!(value["adapterId"], profile.id);
        assert_eq!(
            value["runtimeId"],
            record.runtime.runtime_id.unwrap().to_string()
        );
        assert!(!value.to_string().contains("private"));
        let expected = if profile.provider == AiProvider::Anthropic {
            "/anthropic/v1/models"
        } else {
            "/v1/models"
        };
        assert!(
            f.remote
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(path, _)| path == expected)
        );
    }
}
#[tokio::test]
async fn code_agent_auth_rejects_disconnection_rotation_and_runtime_changes_during_probe() {
    for change in [1, 2, 3] {
        let f = fixture().await;
        let record = ready(&f);
        let snapshot = f
            .state
            .store
            .code_verification_snapshot(f.id)
            .unwrap()
            .unwrap();
        let p = code_agent_auth::resolve(
            snapshot.workspace.spec.agent.as_ref().unwrap(),
            snapshot.ai.as_ref().unwrap(),
        )
        .unwrap();
        let error = agent_auth::check(
            &f.state,
            snapshot,
            record,
            p,
            &Fake { change },
            RequestId(Uuid::new_v4()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), "code_workspace_bindings_changed");
    }
}

#[tokio::test]
async fn code_agent_auth_rejected_credentials_never_reach_the_cli() {
    let f = fixture().await;
    let record = ready(&f);
    let snapshot = f
        .state
        .store
        .code_verification_snapshot(f.id)
        .unwrap()
        .unwrap();
    let p = code_agent_auth::resolve(
        snapshot.workspace.spec.agent.as_ref().unwrap(),
        snapshot.ai.as_ref().unwrap(),
    )
    .unwrap();
    f.remote.replies.lock().unwrap().insert(
        "/v1/models".into(),
        (401, json!({"message":"code-private-provider-body"})),
    );
    let result = agent_auth::check(
        &f.state,
        snapshot,
        record,
        p,
        &Fake { change: 1 },
        RequestId(Uuid::new_v4()),
    )
    .await
    .unwrap();
    let value = serde_json::to_value(result).unwrap();
    assert_eq!(value["reason"], "authentication_rejected");
    assert!(!value.to_string().contains("private"));
    assert!(f.state.store.ai_connection(f.account).unwrap().is_some());
}

#[tokio::test]
async fn code_agent_auth_catalog_requires_operator_access_and_exposes_no_commands() {
    let f = fixture().await;
    for authorized in [false, true] {
        let response = router(&f.state, authorized)
            .oneshot(
                Request::builder()
                    .uri("/api/code/agent-auth/adapters")
                    .header("host", "127.0.0.1:5523")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if authorized {
                StatusCode::OK
            } else {
                StatusCode::UNAUTHORIZED
            }
        );
        if authorized {
            assert_eq!(response.headers()["cache-control"], "no-store");
            let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value.as_array().unwrap().len(), 5);
            assert!(
                value
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|p| p.get("binary").is_none())
            );
        }
    }
}

#[tokio::test]
async fn code_agent_auth_http_cancellation_retains_lifecycle_and_credential_gates() {
    let f = fixture().await;
    ready(&f);
    *f.remote.pause.lock().unwrap() = Some("/v1/models".into());
    let app = f.app.clone();
    let path = format!("/api/code/workspaces/{}/agent-auth/verify", f.id);
    let request = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("host", "127.0.0.1:5523")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"expectedRevision":1}"#))
                .unwrap(),
        )
        .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        f.remote.entered.notified(),
    )
    .await
    .unwrap();
    request.abort();
    let _ = request.await;
    assert!(f.state.pipe_ai_mutations.try_lock().is_err());
    assert!(crate::code_runtime::lock(&f.state, f.id).is_err());
    f.remote.release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if f.state.pipe_ai_mutations.try_lock().is_ok() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(crate::code_runtime::lock(&f.state, f.id).is_ok());
}
async fn request(f: &Fixture, path: &str, body: Value) -> (StatusCode, Value) {
    let r = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("host", "127.0.0.1:5523")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = r.status();
    assert_eq!(r.headers()["cache-control"], "no-store");
    let bytes = to_bytes(r.into_body(), 65536).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
#[tokio::test]
async fn code_agent_auth_routes_fail_closed_without_a_runtime_and_reject_caller_commands() {
    let f = fixture().await;
    let path = format!("/api/code/workspaces/{}/agent-auth/verify", f.id);
    let (status, value) = request(&f, &path, json!({"expectedRevision":1})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["result"]["reason"], "runtime_unavailable");
    assert!(f.remote.requests.lock().unwrap().is_empty());
    assert_eq!(
        request(&f, &path, json!({"expectedRevision":0})).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(&f, &path, json!({"expectedRevision":2})).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &f,
            &path,
            json!({"expectedRevision":1,"command":"untrusted"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    oauth(&f, false);
    assert_eq!(
        request(&f, &path, json!({"expectedRevision":1})).await.1["result"]["reason"],
        "adapter_unavailable"
    );
    assert!(f.remote.requests.lock().unwrap().is_empty());
}

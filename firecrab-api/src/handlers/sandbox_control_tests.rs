//! HTTP contracts use private fixture databases and an absent helper, never host resources.
use crate::{ai_sandbox::access::LocalOperator, state::AppState};
use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Method, Request, StatusCode},
};
use firecrab_api_types::*;
use serde_json::{Value, json};
use std::time::Duration;
use tower::ServiceExt;
use uuid::Uuid;

async fn fixture() -> (tempfile::TempDir, AppState, Uuid, SandboxEnvironment) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("kernel"), b"fixture kernel").unwrap();
    std::fs::write(dir.path().join("rootfs"), b"fixture rootfs").unwrap();
    let registry = crate::templates::TemplateRegistry::from_specs(
        dir.path(),
        [crate::templates::TemplateSpec {
            alias: "fixture".into(),
            version: "v1".into(),
            kernel: "kernel".into(),
            rootfs: "rootfs".into(),
            initrd: None,
            boot_args: "console=ttyS0".into(),
        }],
    )
    .unwrap();
    let image = registry.resolve_alias("fixture").unwrap();
    let pin = SandboxTemplatePin {
        name: "fixture".into(),
        version: "v1".into(),
        kernel_sha256: image.kernel.sha256().into(),
        rootfs_sha256: image.rootfs.sha256().into(),
        initrd_sha256: None,
        boot_args_sha256: image.boot_args_sha256(),
    };
    let mut state = AppState::with_db_file(registry, dir.path().join("state.db"))
        .await
        .unwrap();
    state.network = crate::network::NetworkClient::with_socket_path(dir.path().join("absent.sock"));
    let project = Uuid::new_v4();
    let spec: SandboxProjectSpec = serde_json::from_value(json!({
        "name":"Control contract", "sourcePath":null, "vmId":null,
        "selection":null,"instructions":""
    }))
    .unwrap();
    state
        .store
        .save_sandbox_project(&SandboxProject {
            id: project,
            owner: SandboxPrincipal::Local,
            spec,
            created_at: 1,
        })
        .unwrap();
    let manifest = SandboxEnvironmentSpec {
        version: 1,
        subnet_cidr: "172.30.250.0/24".into(),
        roles: vec![SandboxEnvironmentRole {
            name: "debug".into(),
            template: pin,
            cpu: 1,
            memory_mib: 256,
            disk_gb: 1,
            depends_on: vec![],
            services: vec![],
            execution_mode: SandboxExecutionMode::AgentSandbox {
                runtime: SandboxContainerRuntime::Docker,
                image: format!("example.invalid/fixture@sha256:{}", "a".repeat(64)),
            },
        }],
        allow: vec![],
        retention: SandboxEnvironmentRetention { ttl_seconds: None },
    };
    let environment = crate::ai_sandbox::environment::reservation(
        Uuid::new_v4(),
        project,
        Default::default(),
        manifest,
    )
    .unwrap();
    state
        .store
        .reserve_sandbox_environment(&environment)
        .unwrap();
    (dir, state, project, environment)
}

async fn request(
    state: &AppState,
    method: Method,
    path: &str,
    body: Value,
    host: Option<Uuid>,
) -> (StatusCode, Value) {
    let router = crate::server::build_router(
        state.clone(),
        &crate::server::HttpConfig {
            bind_addr: "127.0.0.1:5523".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 8,
            request_timeout: Duration::from_secs(3),
            static_root: None,
        },
    );
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:5523")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    if let Some(host_id) = host {
        request
            .extensions_mut()
            .insert(ConnectInfo(crate::tls::AuthenticatedHost { host_id }));
    } else {
        request.extensions_mut().insert(LocalOperator);
    }
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 128 * 1024)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

#[tokio::test]
async fn environment_http_preserves_identity_and_retained_workspace_deletion_authority() {
    let (_dir, state, project, environment) = fixture().await;
    let root = format!("/api/ai-sandbox/projects/{project}");
    let path = format!("{root}/environments/{}", environment.id);
    let (status, images) = request(
        &state,
        Method::GET,
        &format!("{root}/environment-images"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        images[0],
        serde_json::to_value(&environment.spec.roles[0].template).unwrap()
    );
    let (status, listed) = request(
        &state,
        Method::GET,
        &format!("{root}/environments"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed.as_array().unwrap().len(), 1);
    let manifest = serde_json::to_value(&environment.spec).unwrap();
    for method in [Method::GET, Method::PUT] {
        let (status, saved) = request(&state, method, &path, manifest.clone(), None).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(saved["generation"], json!(environment.generation));
        assert_eq!(saved["roles"][0]["vmId"], json!(environment.roles[0].vm_id));
    }
    let mut changed = manifest.clone();
    changed["roles"][0]["cpu"] = json!(2);
    let (status, error) = request(&state, Method::PUT, &path, changed, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "sandbox_environment_immutable");

    let stopped = request(
        &state,
        Method::POST,
        &format!("{path}/stop"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(stopped.0, StatusCode::OK, "{}", stopped.1);
    assert_eq!(stopped.1["state"], "stopped");
    let reconciled = request(
        &state,
        Method::POST,
        &format!("{path}/reconcile"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(reconciled.1["state"], "stopped");
    let resumed = request(
        &state,
        Method::POST,
        &format!("{path}/resume"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(resumed.0, StatusCode::OK, "{}", resumed.1);
    assert_eq!(resumed.1["state"], "failed");
    assert!(resumed.1["error"].as_str().unwrap().contains("private DNS"));
    assert_eq!(resumed.1["generation"], json!(environment.generation));
    let exported = request(
        &state,
        Method::POST,
        &format!("{path}/export"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(exported.0, StatusCode::OK, "{}", exported.1);
    assert_eq!(exported.1["taskIds"], json!([]));
    assert_eq!(
        exported.1["environment"]["generation"],
        json!(environment.generation)
    );
    let deleted = request(
        &state,
        Method::DELETE,
        &path,
        json!({"authorizeWorkspaceDeletion":false}),
        None,
    )
    .await;
    assert_eq!(deleted.0, StatusCode::OK, "{}", deleted.1);
    assert_eq!(deleted.1["state"], "deleted");
    let repeated = request(
        &state,
        Method::DELETE,
        &path,
        json!({"authorizeWorkspaceDeletion":false}),
        None,
    )
    .await;
    assert_eq!(repeated.1, deleted.1);
    assert_eq!(
        request(
            &state,
            Method::POST,
            &format!("{path}/resume"),
            json!({}),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );

    let mut retained = environment.spec;
    retained.roles[0].execution_mode = SandboxExecutionMode::PersistentWorkspace;
    let retained = crate::ai_sandbox::environment::reservation(
        Uuid::new_v4(),
        project,
        Default::default(),
        retained,
    )
    .unwrap();
    state.store.reserve_sandbox_environment(&retained).unwrap();
    let path = format!("{root}/environments/{}", retained.id);
    assert_eq!(
        request(
            &state,
            Method::DELETE,
            &path,
            json!({"authorizeWorkspaceDeletion":false}),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        state
            .store
            .sandbox_environment(retained.id)
            .unwrap()
            .unwrap(),
        retained
    );
    let allowed = request(
        &state,
        Method::DELETE,
        &path,
        json!({"authorizeWorkspaceDeletion":true}),
        None,
    )
    .await;
    assert_eq!(allowed.0, StatusCode::OK, "{}", allowed.1);
    assert_eq!(allowed.1["state"], "deleted");
    assert!(state.vms.lock().unwrap().is_empty());
}

#[tokio::test]
async fn environment_http_scopes_reads_and_rejects_delegate_lifecycle_writes() {
    let (_dir, state, project, environment) = fixture().await;
    let root = format!("/api/ai-sandbox/projects/{project}");
    let path = format!("{root}/environments/{}", environment.id);
    let host = Uuid::new_v4();
    state
        .store
        .save_sandbox_access(
            host,
            &SandboxAccessPolicy {
                grants: vec![SandboxResourceGrant {
                    resource: SandboxResource::Project { id: project },
                    permissions: vec![SandboxPermission::Read],
                }],
            },
        )
        .unwrap();
    for suffix in ["environments", "environment-images"] {
        assert_eq!(
            request(
                &state,
                Method::GET,
                &format!("{root}/{suffix}"),
                json!({}),
                Some(host)
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    assert_eq!(
        request(&state, Method::GET, &path, json!({}), Some(host))
            .await
            .0,
        StatusCode::OK
    );
    for suffix in ["reconcile", "stop", "resume", "export"] {
        assert_eq!(
            request(
                &state,
                Method::POST,
                &format!("{path}/{suffix}"),
                json!({}),
                Some(host)
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        request(
            &state,
            Method::PUT,
            &path,
            serde_json::to_value(&environment.spec).unwrap(),
            Some(host)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &state,
            Method::DELETE,
            &path,
            json!({"authorizeWorkspaceDeletion":true}),
            Some(host)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        state
            .store
            .sandbox_environment(environment.id)
            .unwrap()
            .unwrap(),
        environment
    );
    for path in [
        format!("/api/ai-sandbox/projects/{}/environments", Uuid::new_v4()),
        format!("{root}/environments/{}", Uuid::new_v4()),
        "/api/ai-sandbox/projects/not-a-uuid/environments".into(),
    ] {
        let status = request(&state, Method::GET, &path, json!({}), None).await.0;
        assert!(matches!(
            status,
            StatusCode::NOT_FOUND | StatusCode::BAD_REQUEST
        ));
    }
    state
        .store
        .save_sandbox_access(host, &SandboxAccessPolicy::default())
        .unwrap();
    assert_eq!(
        request(&state, Method::GET, &path, json!({}), Some(host))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn accepted_environment_put_survives_probe_failure_without_allocating_host_resources() {
    let (_dir, state, project, environment) = fixture().await;
    let id = Uuid::new_v4();
    let path = format!("/api/ai-sandbox/projects/{project}/environments/{id}");
    let accepted = request(
        &state,
        Method::PUT,
        &path,
        serde_json::to_value(&environment.spec).unwrap(),
        None,
    )
    .await;
    assert_eq!(accepted.0, StatusCode::ACCEPTED, "{}", accepted.1);
    let settled = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let saved = state.store.sandbox_environment(id).unwrap().unwrap();
            if saved.state == SandboxEnvironmentState::Failed {
                break saved;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(json!(settled.generation), accepted.1["generation"]);
    let repeated = request(
        &state,
        Method::PUT,
        &path,
        serde_json::to_value(&environment.spec).unwrap(),
        None,
    )
    .await;
    assert_eq!(repeated.0, StatusCode::OK);
    assert_eq!(repeated.1["generation"], accepted.1["generation"]);
    assert_eq!(
        repeated.1["roles"],
        serde_json::to_value(&settled.roles).unwrap()
    );
    assert!(
        state
            .store
            .micro_network(settled.network_id)
            .unwrap()
            .is_none()
    );
    assert!(state.vms.lock().unwrap().is_empty());
    let mut bad = serde_json::to_value(&environment.spec).unwrap();
    bad["roles"][0]["template"]["rootfsSha256"] = json!("mutable");
    let invalid = format!(
        "/api/ai-sandbox/projects/{project}/environments/{}",
        Uuid::new_v4()
    );
    assert_eq!(
        request(&state, Method::PUT, &invalid, bad, None).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(state.store.sandbox_environments(project).unwrap().len(), 2);
}

fn saved_task(state: &AppState, project: Uuid) -> crate::ai_sandbox::SavedTask {
    let project_spec = state.store.sandbox_project(project).unwrap().unwrap().spec;
    serde_json::from_value(json!({
        "caller":{"principal":"local","operator":true},"project":project_spec,
        "workspace":null,"guest":null,"guest_pending":false,"guest_staged":false,
        "task":{"id":Uuid::new_v4(),"projectId":project,"attemptId":Uuid::new_v4(),"runtime":null,
        "request":{"kind":"chat","input":"Inspect retained output","selection":null,"retryOf":null},
        "selection":{"connectionId":Uuid::new_v4(),"model":"fixture"},"state":"result",
        "createdAt":1,"updatedAt":2,"result":"Done","errorCode":null,"error":null,"events":[],"changes":[]}
    })).unwrap()
}

#[tokio::test]
async fn environment_http_active_attempt_fences_stop_and_cleanup_until_cancelled() {
    let (_dir, state, project, environment) = fixture().await;
    let path = format!(
        "/api/ai-sandbox/projects/{project}/environments/{}",
        environment.id
    );
    let mut task = saved_task(&state, project);
    task.task.state = SandboxTaskState::InProgress;
    state.store.save_sandbox_task(&task).unwrap();
    let stop = request(
        &state,
        Method::POST,
        &format!("{path}/stop"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(stop.0, StatusCode::BAD_REQUEST);
    let delete = request(
        &state,
        Method::DELETE,
        &path,
        json!({"authorizeWorkspaceDeletion":false}),
        None,
    )
    .await;
    assert_eq!(delete.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        state
            .store
            .sandbox_environment(environment.id)
            .unwrap()
            .unwrap(),
        environment
    );
    task.task.state = SandboxTaskState::Cancelled;
    state.store.save_sandbox_task(&task).unwrap();
    let stop = request(
        &state,
        Method::POST,
        &format!("{path}/stop"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(stop.0, StatusCode::OK, "{}", stop.1);
    let delete = request(
        &state,
        Method::DELETE,
        &path,
        json!({"authorizeWorkspaceDeletion":false}),
        None,
    )
    .await;
    assert_eq!(delete.0, StatusCode::OK, "{}", delete.1);
    assert_eq!(delete.1["state"], "deleted");
}

#[tokio::test]
async fn guest_http_requires_project_and_task_resource_grants_and_current_admission() {
    let (_dir, state, project, _environment) = fixture().await;
    let mut task = saved_task(&state, project);
    state.store.save_sandbox_task(&task).unwrap();
    let path = format!(
        "/api/ai-sandbox/projects/{project}/tasks/{}/guest",
        task.task.id
    );
    let host = Uuid::new_v4();
    assert_eq!(
        request(
            &state,
            Method::GET,
            &format!("{path}/health"),
            json!({}),
            Some(host)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    state
        .store
        .save_sandbox_access(
            host,
            &SandboxAccessPolicy {
                grants: vec![SandboxResourceGrant {
                    resource: SandboxResource::Project { id: project },
                    permissions: vec![SandboxPermission::Read],
                }],
            },
        )
        .unwrap();
    let missing = format!(
        "/api/ai-sandbox/projects/{project}/tasks/{}/guest/health",
        Uuid::new_v4()
    );
    assert_eq!(
        request(&state, Method::GET, &missing, json!({}), Some(host))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let unavailable = request(
        &state,
        Method::GET,
        &format!("{path}/health"),
        json!({}),
        Some(host),
    )
    .await;
    assert_eq!(unavailable.0, StatusCode::CONFLICT, "{}", unavailable.1);
    assert_eq!(unavailable.1["error"]["code"], "sandbox_guest_unavailable");
    task.project.source_path = Some("/not-an-admitted-source".into());
    // A new immutable attempt fixture captures a different source than its current project.
    task.task.id = Uuid::new_v4();
    task.task.attempt_id = Uuid::new_v4();
    state.store.save_sandbox_task(&task).unwrap();
    let changed = format!(
        "/api/ai-sandbox/projects/{project}/tasks/{}/guest/health",
        task.task.id
    );
    let denied = request(&state, Method::GET, &changed, json!({}), Some(host)).await;
    assert_eq!(denied.0, StatusCode::FORBIDDEN);
    let stale = request(&state, Method::GET, &changed, json!({}), None).await;
    assert_eq!(stale.0, StatusCode::CONFLICT);
    assert_eq!(stale.1["error"]["code"], "sandbox_guest_generation_changed");
}

#[tokio::test]
async fn guest_http_refuses_missing_channels_and_invalid_process_identity_without_side_effects() {
    let (dir, state, project, _environment) = fixture().await;
    let mut task = saved_task(&state, project);
    let runtime = SandboxRuntimeRef {
        vm_id: Uuid::new_v4(),
        runtime_id: Uuid::new_v4(),
    };
    let mut record = super::vms::test_support::record("guest-http-fixture", runtime.vm_id);
    record.state = VmState::Running;
    record.last_runtime_id = Some(runtime.runtime_id);
    super::vms::test_support::seed_vm(&state, &record);
    task.project.vm_id = Some(runtime.vm_id);
    task.task.request.kind = SandboxTaskKind::Code;
    let mut project_record = state.store.sandbox_project(project).unwrap().unwrap();
    project_record.spec = task.project.clone();
    state.store.save_sandbox_project(&project_record).unwrap();
    task.guest = Some(runtime);
    task.task.runtime = Some(runtime);
    state.store.save_sandbox_task(&task).unwrap();
    let root = format!(
        "/api/ai-sandbox/projects/{project}/tasks/{}/guest",
        task.task.id
    );
    assert_eq!(
        request(
            &state,
            Method::GET,
            &format!("{root}/health"),
            json!({}),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    task.workspace = Some(dir.path().to_path_buf());
    state.store.save_sandbox_task(&task).unwrap();
    let health = request(
        &state,
        Method::GET,
        &format!("{root}/health"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(health.0, StatusCode::NOT_IMPLEMENTED, "{}", health.1);
    assert_eq!(
        health.1["error"]["code"],
        "sandbox_guest_protocol_unsupported"
    );
    let processes = request(
        &state,
        Method::GET,
        &format!("{root}/processes"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(processes.0, StatusCode::CONFLICT);
    assert_eq!(
        processes.1["error"]["code"],
        "sandbox_guest_inspection_failed"
    );
    let signal = json!({"process":{"pid":1,"startTicks":1,"bootId":Uuid::new_v4(),"state":"S"},"signal":"kill"});
    let refused = request(
        &state,
        Method::POST,
        &format!("{root}/signal"),
        signal,
        None,
    )
    .await;
    assert_eq!(refused.0, StatusCode::CONFLICT, "{}", refused.1);
    assert_eq!(refused.1["error"]["code"], "sandbox_guest_signal_refused");
    let saved = state.store.sandbox_task(task.task.id).unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(saved.task).unwrap(),
        serde_json::to_value(task.task).unwrap()
    );
    assert_eq!(state.vms.lock().unwrap().get(&record.id), Some(&record));
}

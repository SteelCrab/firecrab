use super::*;
fn request() -> SandboxTaskInput {
    serde_json::from_value(json!({"kind":"code","input":"Update the selected files", "projectDirectory":"api", "instructionSnapshotId":Uuid::new_v4()})).unwrap()
}
#[test]
fn profile_requires_an_explicit_safe_directory_and_saved_context_without_host_inheritance() {
    let project: SandboxProjectSpec =
        serde_json::from_value(json!({"name":"retained","vmId":Uuid::new_v4()})).unwrap();
    let input = request();
    validate_request(&project, &input).unwrap();
    for dir in [
        "/tmp",
        "../web",
        "api/../web",
        ".ssh",
        ".git",
        ".firecrab-workspace",
        "api/.env",
    ] {
        let mut bad = input.clone();
        bad.project_directory = Some(dir.into());
        assert!(validate_request(&project, &bad).is_err(), "{dir}");
    }
    let mut bad = input.clone();
    bad.kind = SandboxTaskKind::Chat;
    assert!(validate_request(&project, &bad).is_err());
    let mut bad = input.clone();
    bad.instruction_snapshot_id = None;
    assert!(validate_request(&project, &bad).is_err());
    let mut bad = input.clone();
    bad.coordination.dependencies.push(Uuid::new_v4());
    assert!(validate_request(&project, &bad).is_err());
    let mut bad = project.clone();
    bad.context.references.push("README.md".into());
    assert!(validate_request(&bad, &input).is_err());
    let mut root = input;
    root.project_directory = Some("".into());
    validate_request(&project, &root).unwrap();
}
#[test]
fn native_paths_remain_under_the_selected_directory_and_keep_credential_exclusions() {
    assert_eq!(
        native::path("api", "auth.rs", false).unwrap(),
        "api/auth.rs"
    );
    assert_eq!(native::path("api", "", true).unwrap(), "api");
    assert_eq!(native::path("", "", true).unwrap(), "");
    for relative in [
        "../web/secret",
        "/etc/passwd",
        ".ssh/id_rsa",
        ".env.local",
        ".git/config",
        ".firecrab-workspace/task",
        "api\\auth.rs",
    ] {
        assert!(native::path("api", relative, false).is_err(), "{relative}");
    }
}
#[test]
fn capability_set_and_both_provider_prompts_describe_only_actual_file_operations() {
    let definitions = tools();
    assert_eq!(
        definitions
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["list_files", "read_file", "write_file"]
    );
    let write = definitions
        .iter()
        .find(|tool| tool["name"] == "write_file")
        .unwrap();
    assert!(
        write["parameters"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "expectedVersion")
    );
    assert!(
        write["parameters"]["properties"]
            .get("operationId")
            .is_none()
    );
    assert!(model::boundary(true).contains("Writes apply immediately"));
    assert!(!model::boundary(true).contains("only an isolated copy"));
    assert!(model::boundary(false).contains("only an isolated copy"));
}
#[tokio::test]
async fn model_supplied_operation_ids_are_replaced_before_the_event_checkpoint() {
    let (_dir, _state, mut saved) = super::super::recovery_tests::fixture().await;
    saved.task.request.project_directory = Some("api".into());
    saved.guest_pending = false;
    let supplied = Uuid::new_v4();
    let mut call = model::Call {
        id: "call-1".into(),
        name: "write_file".into(),
        arguments: json!({"path":"auth.rs","content":"saved","expectedVersion":0,"operationId":supplied}),
    };
    prepare(&mut saved, &mut call);
    assert!(saved.guest_pending);
    let first = call.arguments["operationId"].as_str().unwrap().to_owned();
    assert_ne!(first, supplied.to_string());
    saved.guest_pending = false;
    prepare(&mut saved, &mut call);
    assert_ne!(call.arguments["operationId"], first);
    let persisted: WriteArgs = serde_json::from_str(&call.arguments.to_string()).unwrap();
    assert_eq!(persisted.path, "auth.rs");
    assert!(!persisted.operation_id.is_nil());
}
#[tokio::test]
async fn guest_execution_rejects_the_file_profile_before_any_transport_or_host_source_access() {
    let (_dir, state, mut saved) = super::super::recovery_tests::fixture().await;
    saved.task.request.project_directory = Some("".into());
    let error = guest::execute(&state, &mut saved, "touch should-never-run", 1)
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    let value = serde_json::to_value(&saved.task).unwrap();
    assert!(value.get("folderMutations").is_none());
}
#[test]
fn file_results_keep_relative_paths_versions_and_raw_field_credential_redaction() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let secret = "fake-quoted-\"credential\\value";
    let account:SavedConnection=serde_json::from_value(json!({"api_key":secret,"connection":{"id":Uuid::new_v4(),"name":"fixture","provider":"openai","endpoint":"https://example.invalid","models":[],"streaming":false,"toolCalls":true}})).unwrap();
    let file:ProjectFile=serde_json::from_value(json!({"version":{"id":7,"path":"api/auth.txt","sha256":"a".repeat(64),"byteLength":secret.len(),"createdAt":1},"contentBase64":STANDARD.encode(secret)})).unwrap();
    let result = format_file(file, "auth.txt", &[account]).unwrap();
    assert_eq!(result["path"], "auth.txt");
    assert_eq!(result["projectPath"], "api/auth.txt");
    assert_eq!(result["version"], 7);
    assert_eq!(result["content"], "[REDACTED]");
}
#[test]
fn oversized_model_replies_fail_without_returning_a_truncated_file_or_lost_version() {
    assert!(reply(json!({"content":"x".repeat(16000),"version":7})).is_err());
    assert!(
        reply(json!({"content":"small","version":7}))
            .unwrap()
            .0
            .contains("version")
    );
}

#[tokio::test]
async fn invalid_writes_never_receive_an_admitted_receipt_or_pending_effect_fence() {
    let (_dir, _state, mut saved) = super::super::recovery_tests::fixture().await;
    saved.task.request.project_directory = Some("api".into());
    saved.guest_pending = false;
    for (path, expected) in [("../escape", 0), (".env", 0), ("auth.rs", u64::MAX)] {
        let mut call = model::Call {
            id: "invalid".into(),
            name: "write_file".into(),
            arguments: json!({"path":path,"content":"saved","expectedVersion":expected,"operationId":Uuid::new_v4()}),
        };
        prepare(&mut saved, &mut call);
        assert!(call.arguments.get("operationId").is_none());
        assert!(!saved.guest_pending);
    }
}

#[test]
fn credential_arguments_and_keys_are_rejected_before_serialization_or_write_admission() {
    let secret = "fixture-quoted-\"secret\\value";
    let account:SavedConnection=serde_json::from_value(json!({"api_key":secret,"connection":{"id":Uuid::new_v4(),"name":"fixture","provider":"openai","endpoint":"https://example.invalid","models":[],"streaming":false,"toolCalls":true}})).unwrap();
    for arguments in [
        json!({"path":"auth.rs","content":secret,"expectedVersion":0}),
        json!({(secret):"value"}),
        json!({"nested":[{"value":secret}]}),
    ] {
        let mut call = model::Call {
            id: "credential".into(),
            name: "write_file".into(),
            arguments,
        };
        reject_credentials(&mut call, std::slice::from_ref(&account));
        assert!(call.arguments.get("rejected").is_some());
        assert!(serde_json::from_value::<WriteArgs>(call.arguments.clone()).is_err());
        assert!(!call.arguments.to_string().contains("fixture-quoted"));
    }
}
#[tokio::test]
async fn an_unconfirmed_write_cannot_admit_another_effect_or_use_isolated_guest_cleanup() {
    let (_dir, state, mut saved) = super::super::recovery_tests::fixture().await;
    saved.task.request.project_directory = Some("api".into());
    assert!(saved.guest_pending);
    let mut call = model::Call {
        id: "next".into(),
        name: "write_file".into(),
        arguments: json!({"path":"auth.rs","content":"saved","expectedVersion":0}),
    };
    prepare(&mut saved, &mut call);
    assert!(saved.guest_pending);
    assert!(call.arguments.get("operationId").is_none());
    assert!(
        super::super::reconcile_guest(&state, &mut saved)
            .await
            .is_err()
    );
    assert!(saved.guest_pending);
}
#[tokio::test]
async fn folder_session_endpoint_uses_transport_identity_and_needs_no_host_source_grant() {
    use crate::{
        handlers::ai_sandbox::{TaskQuery, tasks},
        server::RequestId,
    };
    use axum::{
        Extension,
        extract::{Path, Query, State},
    };
    let (_dir, state, mut saved) = super::super::recovery_tests::fixture().await;
    saved.task.id = Uuid::new_v4();
    saved.task.attempt_id = Uuid::new_v4();
    saved.guest_pending = false;
    saved.guest_staged = false;
    saved.guest_command = None;
    let host = Uuid::new_v4();
    let caller: access::Caller =
        serde_json::from_value(json!({"principal":SandboxPrincipal::Host(host),"operator":false}))
            .unwrap();
    saved.caller = caller;
    saved.project.source_path = Some("/not-authorized/host/source".into());
    saved.task.request.project_directory = Some("api".into());
    let project = saved.task.project_id;
    state
        .store
        .save_sandbox_project(&SandboxProject {
            id: project,
            owner: caller.principal,
            spec: saved.project.clone(),
            created_at: 0,
        })
        .unwrap();
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
    state.store.save_sandbox_task(&saved).unwrap();
    let original = saved.task.id;
    saved.task.id = Uuid::new_v4();
    saved.task.attempt_id = Uuid::new_v4();
    saved.caller = Default::default();
    saved.task.request.input = "another caller's task".into();
    state.store.save_sandbox_task(&saved).unwrap();
    let response = tasks(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        caller,
        Path(project),
        Query(TaskQuery {
            project_directory: Some("api".into()),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(response.0.len(), 1);
    assert_eq!(response.0[0].id, original);
    let invalid = tasks(
        State(state),
        Extension(RequestId(Uuid::new_v4())),
        caller,
        Path(project),
        Query(TaskQuery {
            assigned_agent: Some("Backend".into()),
            ..Default::default()
        }),
    )
    .await;
    assert!(invalid.is_err());
    assert!(
        serde_json::from_value::<TaskQuery>(json!({"projectDirectory":"api","caller":"local"}))
            .is_err()
    );
}
#[tokio::test]
async fn folder_model_connections_expose_only_executable_metadata_without_credentials() {
    use crate::{handlers::ai_sandbox::connections, server::RequestId};
    use axum::{Extension, extract::State};
    let (_dir, state, _saved) = super::super::recovery_tests::fixture().await;
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    for id in [first, second] {
        let account:SavedConnection=serde_json::from_value(json!({"api_key":"fake-private-account-key","connection":{"id":id,"name":"fixture","provider":"openai","endpoint":"https://example.invalid","models":[],"streaming":false,"toolCalls":true}})).unwrap();
        state.store.save_ai_connection(&account).unwrap();
    }
    let host = Uuid::new_v4();
    let caller: access::Caller =
        serde_json::from_value(json!({"principal":SandboxPrincipal::Host(host),"operator":false}))
            .unwrap();
    let mut policy = SandboxAccessPolicy {
        grants: vec![
            SandboxResourceGrant {
                resource: SandboxResource::Connection { id: first },
                permissions: vec![SandboxPermission::Read, SandboxPermission::Execute],
            },
            SandboxResourceGrant {
                resource: SandboxResource::Connection { id: second },
                permissions: vec![SandboxPermission::Read],
            },
        ],
    };
    state.store.save_sandbox_access(host, &policy).unwrap();
    let response = connections(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        caller,
    )
    .await
    .unwrap();
    assert_eq!(response.0.len(), 1);
    assert_eq!(response.0[0].id, first);
    assert!(
        !serde_json::to_string(&response.0)
            .unwrap()
            .contains("fake-private-account-key")
    );
    policy.grants[0].permissions.pop();
    state.store.save_sandbox_access(host, &policy).unwrap();
    assert!(
        connections(State(state), Extension(RequestId(Uuid::new_v4())), caller)
            .await
            .unwrap()
            .0
            .is_empty()
    );
}

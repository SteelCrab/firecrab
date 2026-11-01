use super::*;
use serde_json::json;

const ID: &str = "11111111-1111-4111-8111-111111111111";
fn workspace() -> Value {
    json!({"id":ID,"revision":1,"createdAtMs":1,"updatedAtMs":1,
        "spec":{"name":"web-app","github":{"account":"octocat","owner":"team","repository":"web-app","branch":"feature/login"},
        "runtime":"node_js","resources":{"cpu":2,"ram":4096,"diskGb":16},"agent":{"agent":"codex","connectionId":ID}},
        "connections":{"github":"present_unverified","ai":"missing"}})
}
fn runtime() -> Value {
    json!({"workspaceId":ID,"vmId":ID,"configurationRevision":1,"operationId":ID,"action":"start","phase":"ready",
        "projectPath":"/home/fc-code/project","runtimeId":ID,"initialCommit":"a".repeat(40),"vmState":"running","error":null,
        "placement":{"template":{"name":"ubuntu-26.04","version":"2026.10","kernelSha256":"1".repeat(64),"rootfsSha256":"2".repeat(64),"initrdSha256":null,"bootArgsSha256":"3".repeat(64)},"microNetworkId":ID,"storageRoot":"default"}})
}
#[test]
fn browser_projections_read_the_current_public_api_contract() {
    let workspace: firecrab_api_types::CodeWorkspace = serde_json::from_value(workspace()).unwrap();
    let records = decode_workspaces(json!([workspace])).ok().unwrap();
    assert_eq!(records[0].spec.resources.disk_gb, 16);
    assert_eq!(records[0].connections.ai, "missing");
    let runtime: firecrab_api_types::CodeWorkspaceRuntime =
        serde_json::from_value(runtime()).unwrap();
    let result = decode_runtime(ID, json!(runtime)).ok().unwrap();
    assert_eq!(result.phase, "ready");
    assert_eq!(result.placement.template.version, "2026.10");
}
#[test]
fn runtime_identity_and_live_observations_cannot_cross_workspaces() {
    let mut value = runtime();
    value["workspaceId"] = json!("22222222-2222-4222-8222-222222222222");
    assert!(decode_runtime(ID, value).is_err());
    for (key, wrong) in [
        ("vmState", json!("stopped")),
        ("runtimeId", Value::Null),
        ("error", json!("sync_failed")),
    ] {
        let mut value = runtime();
        value[key] = wrong;
        assert_eq!(
            decode_runtime(ID, value).ok().unwrap().phase,
            "reconcile_required"
        );
    }
    assert!(decode_workspaces(json!([workspace(), workspace()])).is_err());
    assert!(decode_workspaces(json!({"error":"not a list"})).is_err());
}
#[test]
fn configuration_and_account_presence_never_claim_agent_authentication() {
    let mut app = App {
        route: "code".into(),
        loaded: true,
        ..App::default()
    };
    app.code.workspaces = decode_workspaces(json!([workspace()])).ok().unwrap();
    app.code.runtime = RuntimeState::Current(decode_runtime(ID, runtime()).ok().unwrap());
    let html = content(&app);
    assert!(html.contains("Account missing or disconnected"));
    assert!(html.contains("Agent authentication is not verified here."));
    assert!(!html.contains("Open editor"));
    app.code.runtime = RuntimeState::Failed(Problem::Unavailable);
    assert!(!content(&app).contains("MicroVM ready"));
}
#[test]
fn untrusted_names_and_unknown_states_are_safe_in_both_languages() {
    let mut record = workspace();
    record["spec"]["name"] = json!("<img src=x onerror=alert(1)>");
    record["connections"]["ai"] = json!("invented_ready");
    let mut app = App {
        route: "code".into(),
        loaded: true,
        ..App::default()
    };
    app.code.workspaces = decode_workspaces(json!([record])).ok().unwrap();
    for locale in ["en", "ko"] {
        app.locale = locale.into();
        let html = content(&app);
        assert!(!html.contains("<img src=x"));
        assert!(html.contains("&lt;img src=x"));
        assert!(!html.contains("invented_ready"));
    }
    assert!(!is_route("code/../../vms"));
    assert!(!is_route("code/00000000-0000-0000-0000-000000000000"));
    assert!(is_route(&format!("code/{ID}")));
}

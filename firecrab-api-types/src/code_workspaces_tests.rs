use super::*;
use serde_json::json;

fn spec() -> CodeWorkspaceSpec {
    serde_json::from_value(json!({
        "name":"web-app", "github":{"account":"octocat","owner":"team","repository":"web-app","branch":"feature/login"},
        "runtime":"node_js", "resources":{"cpu":2,"ram":4096,"diskGb":16},
        "agent":{"agent":"codex","connectionId":"11111111-1111-4111-8111-111111111111"}
    })).unwrap()
}

#[test]
fn code_workspace_branch_validation_rejects_options_revisions_and_unsafe_refs() {
    let mut workspace = spec();
    for value in [
        "main",
        "feature/login",
        "release/v1.0",
        "한글/기능",
        "refs/heads/main",
    ] {
        workspace.github.branch = value.into();
        assert!(workspace.validate().is_empty(), "{value}");
    }
    for value in [
        "",
        "-n",
        "HEAD",
        "/main",
        "main/",
        "a//b",
        ".main",
        "a/.b",
        "a.lock/b",
        "a/b.lock",
        "a..b",
        "a@{b",
        "@",
        "main.",
        "main:other",
        "a b",
        "a\nb",
        "a\\b",
        "a^",
        "a~",
        "a?",
        "a*",
        "a[",
    ] {
        workspace.github.branch = value.into();
        assert!(
            workspace.validate().contains_key("spec.github.branch"),
            "{value:?}"
        );
    }
    workspace.github.branch = "a".repeat(256);
    assert!(workspace.validate().contains_key("spec.github.branch"));
}

#[test]
fn code_workspace_validates_names_repository_identity_resources_and_non_nil_refs() {
    let mut workspace = spec();
    workspace.name = "../workspace".into();
    workspace.github.account = "https://github.com/octocat".into();
    workspace.github.owner = "user:secret@github.com".into();
    workspace.github.repository = "owner/repo.git".into();
    workspace.resources = CodeResources {
        cpu: 0,
        ram: 3000,
        disk_gb: 0,
    };
    workspace.agent.as_mut().unwrap().connection_id = Uuid::nil();
    assert_eq!(workspace.validate().len(), 8);
    let mut workspace = spec();
    workspace.resources = CodeResources {
        cpu: 32,
        ram: 32768,
        disk_gb: 500,
    };
    workspace.agent = None;
    assert!(workspace.validate().is_empty());
}

#[test]
fn code_workspace_rejects_unknown_credentials_and_client_supplied_runtime_identity() {
    let base = serde_json::to_value(spec()).unwrap();
    for path in ["token", "apiKey", "vmId", "workspacePath"] {
        let mut value = base.clone();
        value[path] = json!("private-input");
        assert!(serde_json::from_value::<CodeWorkspaceSpec>(value).is_err());
    }
    for path in ["github", "agent", "resources"] {
        let mut value = base.clone();
        value[path]["token"] = json!("private-input");
        assert!(serde_json::from_value::<CodeWorkspaceSpec>(value).is_err());
    }
    let mut value = json!({"expectedRevision":0,"spec":base});
    value["status"] = json!("authenticated");
    assert!(serde_json::from_value::<CodeWorkspaceWrite>(value).is_err());
}

#[test]
fn code_workspace_provider_eligibility_does_not_imply_agent_authentication() {
    use AiAuthentication::{ApiKey, Chatgpt};
    use AiProvider::{Anthropic, Openai};
    use CodeAgent::{ClaudeCode, Codex, OpenCode};
    for (provider, authentication, eligible) in [
        (Openai, ApiKey, vec![Codex, OpenCode]),
        (Openai, Chatgpt, vec![Codex]),
        (Anthropic, ApiKey, vec![ClaudeCode, OpenCode]),
        (Anthropic, Chatgpt, vec![]),
    ] {
        let connection = AiConnection {
            id: Uuid::nil(),
            name: "fixture".into(),
            provider,
            authentication,
            endpoint: String::new(),
            models: vec![],
            streaming: false,
            tool_calls: false,
        };
        for agent in [Codex, ClaudeCode, OpenCode] {
            assert_eq!(
                agent.provider_compatible(&connection),
                eligible.contains(&agent)
            );
        }
    }
    assert_eq!(
        serde_json::to_value(CodeConnectionState::PresentUnverified).unwrap(),
        "present_unverified"
    );
}

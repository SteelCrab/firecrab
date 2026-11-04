use super::super::tests::{guest_fixture, shell_replies};
use super::*;

const LIST: &str = "'ls' '-g' '--json'";
const ADD: &str = "'add' '";

fn calls(root: &Path) -> Vec<String> {
    fs::read_to_string(root.join("remote-calls.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn installed_json() -> String {
    json!([{
        "name": "webapp-testing",
        "path": "/root/.agents/skills/webapp-testing",
        "scope": "global",
        "agents": ["Claude Code", "Codex", "Gemini CLI"],
        "source": "anthropics/skills",
        "sourceUrl": "https://github.com/anthropics/skills.git",
        "sourceType": "github",
        "status": "tracked"
    }])
    .to_string()
}

fn install(source: &str, skill: &str) -> Change {
    Change::Install {
        source: source.into(),
        skill: skill.into(),
    }
}

#[test]
fn installed_reports_missing_node_without_installing_it() {
    let (root, runtime) = guest_fixture(true);
    shell_replies(
        root.path(),
        json!({LIST: {"out": "FIRECRAB_NODE_MISSING\n"}}),
    );
    let value = runtime.installed_skills().unwrap();
    assert_eq!(value, json!({"node": "missing", "skills": []}));
    let script = calls(root.path()).pop().unwrap();
    assert!(!script.contains("nodejs.org"), "{script}");
}

#[test]
fn installed_lists_global_skills_from_the_cli() {
    let (root, runtime) = guest_fixture(true);
    shell_replies(root.path(), json!({LIST: {"out": installed_json()}}));
    let value = runtime.installed_skills().unwrap();
    assert_eq!(value["node"], "ready");
    assert_eq!(
        value["skills"],
        json!([{
            "name": "webapp-testing",
            "source": "anthropics/skills",
            "agents": ["Claude Code", "Codex", "Gemini CLI"],
            "status": "tracked"
        }])
    );
    let script = calls(root.path()).pop().unwrap();
    assert!(
        script.contains(&format!("npx -y {CLI} 'ls' '-g' '--json'")),
        "{script}"
    );
    assert!(script.contains("cd /root"), "{script}");
}

#[test]
fn install_runs_the_pinned_cli_for_every_supported_agent() {
    let (root, runtime) = guest_fixture(true);
    shell_replies(
        root.path(),
        json!({
            ADD: {"out": json!([{
                "name": "webapp-testing", "status": "installed", "source": "anthropics/skills",
                "agents": ["Claude Code", "Codex"],
                "security": {"gen": "low", "socket": "0 alerts", "snyk": "low", "details": "https://skills.sh/anthropics/skills"}
            }]).to_string()},
            LIST: {"out": installed_json()},
        }),
    );
    let value = runtime
        .change_skills(install("anthropics/skills", "webapp-testing"))
        .unwrap();
    assert_eq!(value["result"][0]["status"], "installed");
    assert_eq!(value["result"][0]["security"]["snyk"], "low");
    assert_eq!(value["installed"]["skills"][0]["name"], "webapp-testing");
    let script = calls(root.path())
        .into_iter()
        .find(|c| c.contains(ADD))
        .unwrap();
    assert!(
        script.contains(&format!(
            "npx -y {CLI} 'add' 'anthropics/skills' '--skill' 'webapp-testing' '-g' '-a' 'claude-code' 'codex' 'gemini-cli' 'opencode' 'github-copilot' 'cursor' 'grok' '-y' '--json'"
        )),
        "{script}"
    );
    assert!(
        script.contains("https://nodejs.org/dist/latest-v24.x"),
        "{script}"
    );
    assert!(script.contains("sha256sum -c"), "{script}");
}

#[test]
fn install_rejects_unsafe_input_without_a_remote_call() {
    let (root, runtime) = guest_fixture(true);
    for change in [
        install("owner/repo; rm -rf /", "x"),
        install("https://example.com/x", "x"),
        install("owner/repo", "-rf"),
        install("owner/repo", "a$(id)"),
        Change::Remove {
            skill: "--all".into(),
        },
    ] {
        assert!(runtime.change_skills(change).is_err());
    }
    assert!(calls(root.path()).is_empty());
}

#[test]
fn install_failure_reports_the_cli_error() {
    let (root, runtime) = guest_fixture(true);
    shell_replies(
        root.path(),
        json!({ADD: {"out": json!([{"status": "failed", "error": "No valid skills found"}]).to_string()}}),
    );
    let error = runtime
        .change_skills(install("owner/repo", "missing"))
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("No valid skills found"),
        "{error}"
    );
}

#[test]
fn install_reports_a_vm_failure() {
    let (root, runtime) = guest_fixture(true);
    shell_replies(
        root.path(),
        json!({ADD: {"err": "Node.js 22.20 or later could not be installed in this MicroVM.", "status": 1}}),
    );
    let error = runtime
        .change_skills(install("owner/repo", "skill"))
        .err()
        .unwrap();
    assert!(error.to_string().contains("Node.js 22.20"), "{error}");
}

#[test]
fn remove_and_update_use_the_global_scope() {
    let (root, runtime) = guest_fixture(true);
    shell_replies(root.path(), json!({LIST: {"out": "[]"}}));
    let removed = runtime
        .change_skills(Change::Remove {
            skill: "webapp-testing".into(),
        })
        .unwrap();
    assert_eq!(removed["installed"]["skills"], json!([]));
    runtime.change_skills(Change::Update).unwrap();
    let scripts = calls(root.path());
    assert!(
        scripts
            .iter()
            .any(|c| c.contains(&format!("npx -y {CLI} 'remove' '-g' '-y' 'webapp-testing'")))
    );
    assert!(
        scripts
            .iter()
            .any(|c| c.contains(&format!("npx -y {CLI} 'update' '-g' '-y'")))
    );
}

#[test]
fn change_requests_are_tagged_and_strict() {
    let parsed: Change = serde_json::from_value(
        json!({"action": "install", "source": "anthropics/skills", "skill": "pdf"}),
    )
    .unwrap();
    assert!(matches!(parsed, Change::Install { .. }));
    assert!(serde_json::from_value::<Change>(json!({"action": "update"})).is_ok());
    assert!(
        serde_json::from_value::<Change>(json!({"action": "remove", "skill": "x", "extra": 1}))
            .is_err()
    );
    assert!(serde_json::from_value::<Change>(json!({"action": "run"})).is_err());
}

use super::super::tests::{shell_replies, workspace_fixture};
use super::*;

fn request(action: &str) -> Request {
    Request {
        action: action.into(),
        id: Uuid::new_v4(),
        config: Config {
            test_command: "cargo test --locked".into(),
            build_command: "cargo build --locked".into(),
            start_command: String::new(),
            port: 3000,
        },
    }
}

#[test]
fn status_restores_the_guest_run_and_saved_configuration_after_reconnect() {
    let (root, runtime, agent) = workspace_fixture();
    let run = Uuid::new_v4();
    let config = request("run").config;
    let output = format!(
        "FILE:Cargo.toml:{}\nJOB:{run}\nSTATE:passed\nPHASE:ready\nexit:0\nAPPRUNNING:true\napp:ready\nBRANCH:feature/check\nHEAD:abcdef\nREMOTE:https://github.com/octocat/project.git\nWORKFLOW:present\nLOG:{}\nCONFIG:{}\n",
        STANDARD.encode("[workspace]"),
        STANDARD.encode("tests passed\n"),
        STANDARD.encode(serde_json::to_vec(&config).unwrap()),
    );
    shell_replies(root.path(), json!({"FILE:%s:": {"out": output}}));
    let value = runtime.automation_status(agent.id).unwrap();
    assert_eq!(value["run"]["id"], run.to_string());
    assert_eq!(value["run"]["state"], "passed");
    assert_eq!(value["run"]["appReady"], true);
    assert_eq!(value["run"]["config"]["testCommand"], config.test_command);
    assert!(value.to_string().contains("tests passed"));
    assert!(!root.path().join("local-herdr-executed").exists());
}

#[test]
fn verification_uses_the_vm_workspace_and_persists_settings() {
    let (root, runtime, agent) = workspace_fixture();
    for action in ["run", "configure_ci", "stop_preview", "stop_app"] {
        runtime.automate(agent.id, request(action)).unwrap();
    }
    let config: Config = resource::read(&runtime.automation_config_path(agent.id).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(config.test_command, "cargo test --locked");
    let calls = fs::read_to_string(root.path().join("remote-calls.jsonl")).unwrap();
    let calls = calls
        .lines()
        .map(|s| serde_json::from_str::<String>(s).unwrap())
        .collect::<Vec<_>>();
    assert!(calls.iter().any(
        |s| s.contains("/root/.local/share/firecrab/automation/project") && s.contains("worker.sh")
    ));
    assert!(
        calls
            .iter()
            .any(|s| s.contains("cd '/workspace/firecrab/project'") && s.contains("set -C"))
    );
    assert!(
        calls
            .iter()
            .any(|s| s.contains("app.start") && s.contains("kill -TERM"))
    );
}

#[test]
fn active_work_and_invalid_commands_cannot_start_verification() {
    let (root, runtime, mut agent) = workspace_fixture();
    agent.pending_task = Some(PendingTask {
        text: "waiting for approval".into(),
        uncertain: false,
    });
    runtime.save(std::slice::from_ref(&agent)).unwrap();
    assert!(
        runtime
            .automate(agent.id, request("run"))
            .unwrap_err()
            .to_string()
            .contains("Wait for all agents")
    );
    for command in [String::new(), "x".repeat(4097), "test\0command".into()] {
        let mut input = request("run");
        input.config.test_command = command;
        assert!(runtime.automate(agent.id, input).is_err());
    }
    let mut input = request("run");
    input.config.start_command = "npm start".into();
    input.config.port = 22;
    assert!(runtime.automate(agent.id, input).is_err());
    assert!(!root.path().join("remote-calls.jsonl").exists());
}

#[test]
fn running_apps_and_preview_gates_prevent_overlapping_or_unverified_publication() {
    let (root, runtime, agent) = workspace_fixture();
    shell_replies(
        root.path(),
        json!({"FILE:%s:": {"out": format!("JOB:{}\nSTATE:passed\nAPPRUNNING:true\napp:ready\n", Uuid::new_v4())}}),
    );
    assert!(
        runtime
            .automate(agent.id, request("run"))
            .unwrap_err()
            .to_string()
            .contains("Stop this task's existing")
    );
    shell_replies(root.path(), json!({"FILE:%s:": {"out": "STATE:failed\n"}}));
    for action in ["start_preview", "check_preview"] {
        assert!(runtime.automate(agent.id, request(action)).is_err());
    }
    shell_replies(
        root.path(),
        json!({"then printf running": {"out": "running"}}),
    );
    assert!(runtime.automation_busy(agent.id).unwrap());
    assert!(
        runtime
            .automate(agent.id, request("configure_ci"))
            .unwrap_err()
            .to_string()
            .contains("Wait for verification")
    );
    assert!(runtime.automate(agent.id, request("unknown")).is_err());
}

#[test]
fn workflow_runs_require_vm_github_auth_and_valid_remote_json() {
    let (root, mut runtime, agent) = workspace_fixture();
    runtime
        .guest
        .as_mut()
        .unwrap()
        .authenticated
        .remove("github");
    assert!(
        runtime
            .automate(agent.id, request("github"))
            .unwrap_err()
            .to_string()
            .contains("Connect GitHub")
    );
    runtime
        .guest
        .as_mut()
        .unwrap()
        .authenticated
        .insert("github".into());
    shell_replies(
        root.path(),
        json!({"gh run list": {"out": "[{\"databaseId\":12,\"status\":\"completed\"}]"}}),
    );
    assert_eq!(
        runtime.automate(agent.id, request("github")).unwrap()["runs"][0]["databaseId"],
        12
    );
    shell_replies(root.path(), json!({"gh run list": {"out": "not JSON"}}));
    assert!(runtime.automate(agent.id, request("github")).is_err());
}

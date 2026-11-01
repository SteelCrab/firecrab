use super::super::tests::{shell_replies, workspace_fixture};
use super::*;

#[test]
fn repositories_use_only_the_vm_account_and_return_no_credentials() {
    let (root, mut runtime, _) = workspace_fixture();
    runtime
        .guest
        .as_mut()
        .unwrap()
        .authenticated
        .remove("github");
    assert!(runtime.github_repositories().is_err());
    assert!(!root.path().join("remote-calls.jsonl").exists());
    runtime
        .guest
        .as_mut()
        .unwrap()
        .authenticated
        .insert("github".into());
    shell_replies(
        root.path(),
        json!({"user/repos?": {"out": r#"[{"name":"owner/private-repo","private":true,"archived":false,"token":"fixture-only-credential"}]"#}}),
    );
    let result = runtime.github_repositories().unwrap();
    assert_eq!(result["repositories"][0]["name"], "owner/private-repo");
    assert_eq!(result["repositories"][0]["private"], true);
    assert!(!result.to_string().contains("fixture-only-credential"));
    for output in [
        "not JSON",
        r#"[{"name":"../host","private":false,"archived":false}]"#,
    ] {
        shell_replies(root.path(), json!({"user/repos?":{"out":output}}));
        assert!(runtime.github_repositories().is_err());
    }
}

#[test]
fn vm_status_keeps_authentication_and_sessions_in_the_selected_vm_scope() {
    let (root, mut runtime, agent) = workspace_fixture();
    let status = runtime.status().unwrap();
    assert_eq!(
        status["vmId"],
        runtime.guest.as_ref().unwrap().id.to_string()
    );
    assert_eq!(status["github"]["state"], "connected");
    assert_eq!(status["agents"][0]["id"], agent.id.to_string());
    assert_eq!(status["agents"][0]["state"], "idle");
    assert!(
        status["drivers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| ["codex", "claude"].contains(&d["id"].as_str().unwrap()))
    );
    runtime
        .guest
        .as_mut()
        .unwrap()
        .authenticated
        .remove("github");
    runtime.guest.as_mut().unwrap().available.remove("gh");
    assert_eq!(runtime.github_status().unwrap()["state"], "unavailable");
    assert!(
        runtime
            .connect_github()
            .unwrap_err()
            .to_string()
            .contains("Prepare GitHub tools")
    );
    runtime
        .guest
        .as_mut()
        .unwrap()
        .available
        .insert("gh".into());
    fs::write(
        root.path().join("github-status"),
        "PENDING\nCODE:AB12-CD34\nTOKEN:fixture-only-credential\n",
    )
    .unwrap();
    let pending = runtime.connect_github().unwrap();
    assert_eq!(pending["userCode"], "AB12-CD34");
    assert!(!pending.to_string().contains("fixture-only-credential"));
    let calls = fs::read_to_string(root.path().join("remote-calls.jsonl")).unwrap();
    assert!(!calls.contains("nohup"));
}

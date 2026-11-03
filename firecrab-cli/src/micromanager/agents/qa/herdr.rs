//! Opt-in QA against explicitly owned resources. Not an automated coverage input.
#[cfg(unix)]
use super::*;
#[cfg(unix)]
#[test]
#[ignore = "Explicit private Herdr QA server; never launches a coding agent"]
fn real_herdr_terminal_transport() {
    let session =
        std::env::var("FIRECRAB_TEST_HERDR_SESSION").expect("Explicit QA session required");
    assert!(session.starts_with("firecrab-qa-"));
    let root = tempfile::tempdir().unwrap();
    let mut runtime = Runtime::new(root.path());
    runtime.session = session;
    let cwd = root.path().to_str().unwrap();
    let created = runtime
        .run(&[
            "workspace",
            "create",
            "--cwd",
            cwd,
            "--label",
            "Firecrab-transport-QA",
            "--no-focus",
        ])
        .unwrap();
    let pane = &created["root_pane"];
    let id = Uuid::new_v4();
    let agent = Agent {
        id,
        name: "QA terminal".into(),
        role: "Transport check".into(),
        kind: "terminal".into(),
        cwd: cwd.into(),
        model: String::new(),
        effort: String::new(),
        workspace_id: pane["workspace_id"].as_str().unwrap().into(),
        pane_id: pane["pane_id"].as_str().unwrap().into(),
        terminal_id: pane["terminal_id"].as_str().unwrap().into(),
        closed: false,
        launch_error: None,
        last_output: String::new(),
        request_digest: String::new(),
        pending_task: None,
        conversation_id: None,
        recovery: None,
        chosen: None,
    };
    runtime.save(&[agent]).unwrap();
    runtime
        .input(
            id,
            Input {
                text: Some("printf 'rust-terminal-bridge-ok\\n'".into()),
                key: None,
            },
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let output = runtime.inspect(id).unwrap();
        if output["text"]
            .as_str()
            .unwrap()
            .contains("rust-terminal-bridge-ok\n")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Terminal output did not contain executed command result: {output}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    runtime
        .input(
            id,
            Input {
                text: None,
                key: Some("ctrl+c".into()),
            },
        )
        .unwrap();
    runtime.close(id).unwrap();
    assert_eq!(runtime.inspect(id).unwrap()["agent"]["state"], "closed");
}

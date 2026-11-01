//! Opt-in QA against explicitly owned resources. Not an automated coverage input.
use super::*;
#[test]
#[ignore = "Explicit disposable qa-web-vscode VM; installs actual editor, never runs AI or public tunnels"]
fn real_disposable_editor_install_and_forward() {
    let managed = std::env::var("FIRECRAB_EDITOR_QA_HOME").expect("Private QA home");
    let vm: Uuid = std::env::var("FIRECRAB_EDITOR_QA_VM")
        .expect("Disposable QA VM")
        .parse()
        .unwrap();
    let api_port = super::super::super::dev::instance::api_port(Path::new(&managed)).unwrap();
    let metadata: Value =
        reqwest::blocking::get(format!("http://127.0.0.1:{api_port}/api/vms/{vm}"))
            .unwrap()
            .json()
            .unwrap();
    assert!(
        metadata["name"]
            .as_str()
            .unwrap()
            .starts_with("qa-web-vscode-")
    );
    let runtime = Runtime::for_vm(Path::new(&managed), vm, false).unwrap();
    let guest = runtime.guest.as_ref().unwrap();
    let records = runtime.records().unwrap();
    assert!(records.iter().all(|a| a.closed && a.role == "qa-editor"));
    let id = records.first().map_or_else(Uuid::new_v4, |a| a.id);
    let cwd = format!("/workspace/firecrab/{id}");
    if let Ok(action) = std::env::var("FIRECRAB_EDITOR_QA_ACTION") {
        assert!(!records.is_empty());
        match action.as_str() {
            "input_compatibility" => {
                guest.shell(&format!("mkdir -p {cwd}/.vscode; printf '%s' '{{\"editor.editContext\":false}}' > {cwd}/.vscode/settings.json"),Duration::from_secs(5)).unwrap();
            }
            "verify_browser_edit" => {
                let file = guest
                    .shell(
                        &format!("cat {cwd}/editor-proof.txt"),
                        Duration::from_secs(5),
                    )
                    .unwrap();
                assert_eq!(file.trim(), "BROWSER_SAVED_IN_AGENT_PROJECT");
                guest.shell(&format!("printf 'BROWSER_SAVED_IN_AGENT_PROJECT\\nAGENT_UPDATED_SAME_PROJECT\\n' > {cwd}/editor-proof.txt"),Duration::from_secs(5)).unwrap();
                println!(
                    "Browser save confirmed on the VM filesystem. Agent-side edit written to the identical project file."
                );
            }
            "stop" => {
                let saved: Forward =
                    serde_json::from_slice(&fs::read(runtime.editor_record(id)).unwrap()).unwrap();
                assert_eq!(runtime.stop_editor(id).unwrap()["state"], "installed");
                for _ in 0..20 {
                    if !listener_owned(saved.pid, saved.local_port) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                assert!(!listener_owned(saved.pid, saved.local_port));
                println!("Owned editor and SSH forward stopped. Project files retained.");
            }
            _ => panic!("Unknown disposable editor QA action"),
        }
        return;
    }
    if records.is_empty() {
        runtime
            .save(&[Agent {
                id,
                name: "Web VS Code QA".into(),
                role: "qa-editor".into(),
                kind: "codex".into(),
                cwd: cwd.clone(),
                model: String::new(),
                workspace_id: String::new(),
                pane_id: String::new(),
                terminal_id: String::new(),
                closed: true,
                launch_error: None,
                last_output: String::new(),
                request_digest: String::new(),
                pending_task: None,
            }])
            .unwrap();
    }
    guest
        .shell(
            &format!("mkdir -p {cwd}; printf 'AGENT_INITIAL_CONTENT\\n' > {cwd}/editor-proof.txt"),
            Duration::from_secs(10),
        )
        .unwrap();
    runtime.open_editor(id).unwrap();
    for _ in 0..240 {
        let status = runtime.editor_status(id).unwrap();
        if status["state"] == "failed" || status["state"] == "interrupted" {
            panic!("Editor setup failed: {status}");
        }
        if status["state"] == "installed" || status["state"] == "running" {
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    let connection = runtime.open_editor(id).unwrap();
    assert_eq!(connection["state"], "ready");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let response = client
        .get(connection["url"].as_str().unwrap())
        .send()
        .unwrap();
    let response = if response.status().is_redirection() {
        let cookie = response
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let original = reqwest::Url::parse(connection["url"].as_str().unwrap()).unwrap();
        let target = original
            .join(
                response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .unwrap()
                    .to_str()
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(target.origin(), original.origin());
        client
            .get(target)
            .header(reqwest::header::COOKIE, cookie)
            .send()
            .unwrap()
    } else {
        response
    };
    assert!(
        response.status().is_success(),
        "Editor did not serve the real workbench: {}",
        response.status()
    );
    let body = response.text().unwrap();
    assert!(body.contains("workbench") || body.contains("vscode"));
    let again = runtime.open_editor(id).unwrap();
    assert_eq!(
        connection["url"], again["url"],
        "Reopening must reuse the same editor connection"
    );
    let path = Path::new("/private/tmp/firecrab-web-vscode-qa-connection.json");
    resource::write(path, &connection).unwrap();
    let public = runtime.editor_status(id).unwrap();
    assert!(public.get("url").is_none());
    assert!(!public.to_string().contains("tkn="));
    println!(
        "Real Web VS Code workbench serves through a private pinned SSH forward. Connection details saved privately for browser file-edit QA. No AI calls or public endpoints."
    );
}

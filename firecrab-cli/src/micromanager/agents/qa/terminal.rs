//! Opt-in VM setup for interactive QA. No model CLI or paid inference is run.
use super::*;

#[test]
#[ignore = "Requires an explicitly owned disposable qa-agent-terminal VM"]
fn prepare_disposable_vm_terminals() {
    let home = PathBuf::from(std::env::var("FIRECRAB_AGENT_TERMINAL_QA_HOME").unwrap());
    let vm: Uuid = std::env::var("FIRECRAB_AGENT_TERMINAL_QA_VM")
        .unwrap()
        .parse()
        .unwrap();
    let port = super::super::super::dev::instance::api_port(&home).unwrap();
    let client = reqwest::blocking::Client::new();
    let machine: Value = client
        .get(format!("http://127.0.0.1:{port}/api/vms/{vm}"))
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .unwrap();
    assert!(
        machine["name"]
            .as_str()
            .unwrap()
            .starts_with("qa-agent-terminal-")
    );
    let runtime = Runtime::for_vm(&home, vm, false).unwrap();
    assert!(
        runtime.records().unwrap().is_empty(),
        "Do not change existing agent sessions"
    );
    let guest = runtime.guest.as_ref().unwrap();
    guest
        .shell(
            r#"
set -eu
export PATH=/root/.local/bin:/usr/local/bin:/usr/bin:/bin
if ! command -v herdr >/dev/null 2>&1; then
  curl --connect-timeout 15 --retry 2 -fsSL https://herdr.dev/install.sh -o /tmp/qa-herdr-install.sh
  sh /tmp/qa-herdr-install.sh
fi
herdr terminal session control --help >/dev/null
"#,
            Duration::from_secs(240),
        )
        .unwrap();
    let runtime = Runtime::for_vm(&home, vm, true).unwrap();
    runtime.ensure_server().unwrap();
    let mut records = Vec::new();
    for name in ["QA terminal A", "QA terminal B"] {
        let id = Uuid::new_v4();
        let cwd = format!("/workspace/firecrab/{id}");
        runtime
            .guest
            .as_ref()
            .unwrap()
            .shell(
                &format!("mkdir -p {}", guest::quote(&cwd)),
                Duration::from_secs(10),
            )
            .unwrap();
        let created = runtime
            .run(&[
                "workspace",
                "create",
                "--cwd",
                &cwd,
                "--label",
                name,
                "--no-focus",
            ])
            .unwrap();
        records.push(Agent {
            id,
            name: name.into(),
            role: "qa-terminal".into(),
            kind: "codex".into(),
            cwd,
            model: String::new(),
            workspace_id: created["workspace"]["workspace_id"]
                .as_str()
                .unwrap()
                .into(),
            pane_id: created["root_pane"]["pane_id"].as_str().unwrap().into(),
            terminal_id: created["root_pane"]["terminal_id"].as_str().unwrap().into(),
            closed: false,
            launch_error: None,
            last_output: String::new(),
            request_digest: String::new(),
            pending_task: None,
        });
    }
    runtime.save(&records).unwrap();
    let attach = Attach {
        controller: resource::controller_id(&home).unwrap(),
        cols: 100,
        rows: 24,
    };
    assert!(runtime.terminal_command(records[0].id, &attach).is_ok());
    let mut changed = records.clone();
    changed[0].terminal_id = "another-terminal".into();
    runtime.save(&changed).unwrap();
    assert!(
        runtime
            .terminal_command(records[0].id, &attach)
            .unwrap_err()
            .to_string()
            .contains("identity changed")
    );
    changed[0] = records[0].clone();
    changed[0].closed = true;
    runtime.save(&changed).unwrap();
    assert!(runtime.terminal_command(records[0].id, &attach).is_err());
    runtime.save(&records).unwrap();
    resource::write(
        Path::new("/private/tmp/firecrab-agent-terminal-qa-sessions.json"),
        &json!({"vmId":vm,"agents":records}),
    )
    .unwrap();
    println!("Prepared two owned QA shell PTYs; no coding CLI was invoked.");
}

#[test]
#[ignore = "Requires the owned qa-agent-terminal VM; release browser control of terminal A first"]
fn real_guest_input_keys_unicode_resize_and_detach() {
    use std::io::{BufRead, BufReader as SyncReader, Write};
    let home = PathBuf::from(std::env::var("FIRECRAB_AGENT_TERMINAL_QA_HOME").unwrap());
    let fixture: Value = serde_json::from_slice(
        &fs::read("/private/tmp/firecrab-agent-terminal-qa-sessions.json").unwrap(),
    )
    .unwrap();
    let vm: Uuid = fixture["vmId"].as_str().unwrap().parse().unwrap();
    let runtime = Runtime::for_vm(&home, vm, true).unwrap();
    let agent = runtime
        .records()
        .unwrap()
        .into_iter()
        .find(|a| a.name == "QA terminal A" && a.role == "qa-terminal")
        .unwrap();
    let guest = runtime.guest.as_ref().unwrap();
    let attach = Attach {
        controller: resource::controller_id(&home).unwrap(),
        cols: 80,
        rows: 24,
    };
    let mut child = runtime
        .terminal_command(agent.id, &attach)
        .unwrap()
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut first = true;
        for line in SyncReader::new(stdout).lines() {
            let line = line.unwrap();
            if first {
                tx.send(line).unwrap();
                first = false;
            }
        }
    });
    let first: Value =
        serde_json::from_str(&rx.recv_timeout(Duration::from_secs(10)).unwrap()).unwrap();
    assert_eq!(first["type"], "terminal.frame", "{first}");
    let mut stdin = child.stdin.take().unwrap();
    let send = |stdin: &mut std::process::ChildStdin, bytes: &[u8]| {
        writeln!(
            stdin,
            "{}",
            json!({"type":"terminal.input","bytes":STANDARD.encode(bytes)})
        )
        .unwrap();
        stdin.flush().unwrap();
    };
    let read = |file: &str| {
        guest
            .shell(
                &format!("cat {}/{} 2>/dev/null || :", guest::quote(&agent.cwd), file),
                Duration::from_secs(10),
            )
            .unwrap()
    };
    send(&mut stdin, b"\x03");
    // Wait for the shell's SIGINT handler before sending the next command.
    std::thread::sleep(Duration::from_millis(250));
    send(
        &mut stdin,
        b"export LC_CTYPE=C.UTF-8; PS1='QA A> '; clear\r",
    );
    std::thread::sleep(Duration::from_millis(250));
    guest
        .shell(
            &format!(
                "rm -f {0}/utf8-proof.txt {0}/interrupt-proof.txt {0}/resize-proof.txt",
                guest::quote(&agent.cwd)
            ),
            Duration::from_secs(10),
        )
        .unwrap();
    send(&mut stdin, "printf '%s' '한글' > utf8-proof.txt".as_bytes());
    assert_eq!(
        read("utf8-proof.txt"),
        "",
        "Typing must not submit a command"
    );
    send(&mut stdin, b"\r");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(read("utf8-proof.txt"), "한글");
    send(&mut stdin, b"sleep 60\r");
    std::thread::sleep(Duration::from_millis(250));
    send(&mut stdin, b"\x03");
    std::thread::sleep(Duration::from_millis(250));
    send(&mut stdin, b"printf yes > interrupt-proof.txt\r");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(read("interrupt-proof.txt"), "yes");
    writeln!(
        stdin,
        "{}",
        json!({"type":"terminal.resize","cols":120,"rows":30})
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(250));
    send(&mut stdin, b"stty size > resize-proof.txt\r");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(read("resize-proof.txt").trim(), "30 120");
    writeln!(stdin, "{{\"type\":\"terminal.release\"}}").unwrap();
    drop(stdin);
    for _ in 0..20 {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    reader.join().unwrap();
    assert!(
        runtime.pane(&agent).is_ok(),
        "Disconnect must leave the original PTY alive"
    );
    println!("Real guest PTY verified: no auto submit, Unicode, Ctrl+C, resize, detach.");
}

use super::super::tests::{shell_replies, workspace_fixture};
use super::*;

unsafe extern "C" {
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}

struct EditorFixture {
    root: tempfile::TempDir,
    runtime: Runtime,
    agent: Agent,
}
impl EditorFixture {
    fn new() -> Self {
        let (root, runtime, agent) = workspace_fixture();
        Self {
            root,
            runtime,
            agent,
        }
    }
    fn remote(&self, output: &str) {
        shell_replies(
            self.root.path(),
            json!({"printf 'BOOT:'": {"out": output},
            "/token": {"out": "a".repeat(64)}}),
        );
    }
}
impl Drop for EditorFixture {
    fn drop(&mut self) {
        // Reap only loopback SSH fixtures created by this test, including on assertion failure.
        let records =
            fs::read_to_string(self.root.path().join("forward-pids.jsonl")).unwrap_or_default();
        for line in records.lines() {
            let record: Value = serde_json::from_str(line).unwrap();
            let pid = record["pid"].as_u64().unwrap() as u32;
            if process_identity(pid).is_some_and(|s| s.contains(self.root.path().to_str().unwrap()))
            {
                let _ = Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .status();
            }
            for _ in 0..50 {
                if unsafe { waitpid(pid as i32, std::ptr::null_mut(), 1) } != 0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

#[test]
fn guest_install_and_running_states_survive_browser_reconnects() {
    let f = EditorFixture::new();
    for (output, expected) in [
        ("BOOT:a\n", "not_installed"),
        ("BOOT:a\nINSTALL:installing\nPHASE:download\n", "installing"),
        ("BOOT:a\nINSTALL:interrupted\n", "interrupted"),
        ("BOOT:a\nINSTALLED:yes\n", "installed"),
        ("BOOT:a\nINSTALLED:yes\nPID:14\nPORT:3000\n", "running"),
    ] {
        f.remote(output);
        let value = f.runtime.editor_status(f.agent.id).unwrap();
        assert_eq!(value["state"], expected);
        assert_eq!(value["projectPath"], project::WORKSPACE);
        assert_eq!(
            value["vmId"],
            f.runtime.guest.as_ref().unwrap().id.to_string()
        );
    }
    f.remote(&format!(
        "INSTALL:failed\nLOG:{}\n",
        STANDARD.encode("checksum mismatch\n")
    ));
    assert_eq!(
        f.runtime.editor_status(f.agent.id).unwrap()["error"],
        "checksum mismatch\n"
    );
    f.remote("INSTALL:installing\n");
    assert_eq!(
        f.runtime.open_editor(f.agent.id).unwrap()["state"],
        "installing"
    );
}

#[test]
fn editor_reuses_only_the_matching_owned_forward_and_exact_vm_project() {
    let f = EditorFixture::new();
    f.remote("BOOT:guest-boot-a\nINSTALLED:yes\nPID:123\nPORT:3000\n");
    let first = f.runtime.open_editor(f.agent.id).unwrap();
    let second = f.runtime.open_editor(f.agent.id).unwrap();
    assert_eq!(first, second);
    let url = reqwest::Url::parse(first["url"].as_str().unwrap()).unwrap();
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    assert_eq!(
        url.query_pairs()
            .find(|(key, _)| key == "folder")
            .unwrap()
            .1,
        project::WORKSPACE
    );
    let path = f.runtime.editor_record(f.agent.id);
    let saved: Forward = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(listener_owned(saved.pid, saved.local_port));
    f.remote("BOOT:guest-boot-b\nINSTALLED:yes\nPID:124\nPORT:3000\n");
    let renewed = f.runtime.open_editor(f.agent.id).unwrap();
    assert_eq!(renewed["projectPath"], project::WORKSPACE);
    assert_ne!(first["url"], renewed["url"]);
    f.remote("BOOT:guest-boot-b\nINSTALLED:yes\n");
    assert_eq!(
        f.runtime.stop_editor(f.agent.id).unwrap()["state"],
        "installed"
    );
    assert!(!path.exists());
}

#[test]
fn malformed_editor_processes_and_tokens_never_open_a_forward() {
    let f = EditorFixture::new();
    for output in [
        "INSTALLED:yes\nPID:bad\nPORT:3000\nBOOT:a\n",
        "INSTALLED:yes\nPID:12\nPORT:70000\nBOOT:a\n",
    ] {
        f.remote(output);
        assert!(f.runtime.open_editor(f.agent.id).is_err());
    }
    shell_replies(
        f.root.path(),
        json!({"printf 'BOOT:'": {"out": "INSTALLED:yes\nPID:12\nPORT:3000\nBOOT:a\n"},
        "/token": {"out": "invalid-secret"}}),
    );
    assert!(
        f.runtime
            .open_editor(f.agent.id)
            .unwrap_err()
            .to_string()
            .contains("Invalid editor connection identity")
    );
    assert!(!f.root.path().join("forward-pids.jsonl").exists());
    assert!(!f.runtime.editor_record(f.agent.id).exists());
}

#[test]
fn stale_editor_records_cannot_signal_an_unrelated_process() {
    let f = EditorFixture::new();
    let record = Forward {
        vm_id: f.runtime.guest.as_ref().unwrap().id,
        agent_id: f.agent.id,
        boot: "old-boot".into(),
        remote_pid: 1,
        remote_port: 3000,
        local_port: 1,
        pid: std::process::id(),
        identity: "stale-process-identity".into(),
        token: "a".repeat(64),
    };
    let path = f.runtime.editor_record(f.agent.id);
    resource::write(&path, &record).unwrap();
    f.remote("INSTALLED:yes\n");
    f.runtime.stop_editor(f.agent.id).unwrap();
    assert!(process_identity(std::process::id()).is_some());
    assert!(!path.exists());
}

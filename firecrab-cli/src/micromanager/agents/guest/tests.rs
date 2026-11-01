use super::super::tests::{shell_replies, workspace_fixture};
use super::*;
use std::io::Write;
use std::net::TcpListener;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

struct Api {
    port: u16,
    calls: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Api {
    fn start(responses: Value) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen, done) = (calls.clone(), stop.clone());
        let worker = std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                use std::io::BufRead;
                let mut reader = io::BufReader::new(&mut stream);
                let mut request = String::new();
                if reader.read_line(&mut request).unwrap_or(0) == 0 {
                    continue;
                }
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                        break;
                    }
                }
                drop(reader);
                let path = request.split_whitespace().nth(1).unwrap().to_owned();
                seen.lock().unwrap().push(path.clone());
                let value = &responses[&path];
                let body = value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string());
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self {
            port,
            calls,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Api {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.worker.take().unwrap().join();
    }
}

#[test]
fn connection_pins_controller_vm_state_and_ssh_credentials_before_use() {
    for (field, value, expected) in [
        ("controller", "foreign", "another controller"),
        ("id", "different-vm", "Start the selected"),
        ("state", "stopped", "Start the selected"),
        ("purpose", "queue-engine", "regular MicroVM"),
        ("ipv4", "not-an-address", "Invalid MicroVM address"),
        ("publicKey", "ssh-rsa unpinned", "pinned SSH host key"),
        ("privateKey", "invalid pem", "Invalid MicroVM operator key"),
        ("valid", "", ""),
    ] {
        let root = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let controller = resource::controller_id(root.path()).unwrap();
        let mut host = json!({"platform":{"controllerId":controller}});
        let mut vm = json!({"id":id,"state":"running","purpose":"instance","ipv4":"192.0.2.10"});
        let mut key = "ssh-ed25519 YWJjZA==".to_owned();
        let mut private = "-----BEGIN OPENSSH PRIVATE KEY-----\nfixture-only\n".to_owned();
        match field {
            "controller" => host["platform"]["controllerId"] = json!(value),
            "publicKey" => key = value.into(),
            "privateKey" => private = value.into(),
            "valid" => {}
            _ => vm[field] = json!(value),
        }
        let api = Api::start(json!({"/api/host":host,format!("/api/vms/{id}"):vm,
            format!("/api/vms/{id}/ssh-host-key"):{"publicKey":key},
            format!("/api/vms/{id}/ssh-key"):private}));
        let instance = super::super::super::dev::instance::test_instance(
            root.path(),
            "0123456789abcdef",
            api.port,
            if api.port == u16::MAX {
                api.port - 1
            } else {
                api.port + 1
            },
        );
        // The private instance and server use the same isolated identity, never the primary API.
        resource::write(
            &instance
                .managed_home
                .join("runtime/resource-controller-id.json"),
            &controller,
        )
        .unwrap();
        let error = Guest::connect(&instance.managed_home, id)
            .err()
            .expect("No native management VM is configured in this fixture")
            .to_string();
        if !expected.is_empty() {
            assert!(error.contains(expected), "{field}: {error}");
        }
        let calls = api.calls.lock().unwrap();
        if field == "controller" {
            assert_eq!(&*calls, &["/api/host"]);
        }
        if field == "valid" {
            let directory = instance
                .managed_home
                .join("runtime/agents/microvms")
                .join(id.to_string());
            assert_eq!(
                fs::read_to_string(directory.join("operator-key")).unwrap(),
                private
            );
            assert!(
                fs::read_to_string(directory.join("known_hosts"))
                    .unwrap()
                    .contains(&format!("firecrab-microvm-{id} ssh-ed25519"))
            );
        }
    }
}

#[test]
fn ssh_files_are_private_and_symlinks_cannot_overwrite_other_files() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::tempdir().unwrap();
    let key = root.path().join("key");
    private_write(&key, b"private-key").unwrap();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
    private_write(&key, b"private-key").unwrap();
    assert_eq!(
        fs::metadata(&key).unwrap().permissions().mode() & 0o777,
        0o600
    );
    private_write(&key, b"replacement-key").unwrap();
    assert_eq!(fs::read(&key).unwrap(), b"replacement-key");
    let alias = root.path().join("symlink-key");
    symlink(&key, &alias).unwrap();
    assert!(private_write(&alias, b"overwritten").is_err());
    assert_eq!(fs::read(&key).unwrap(), b"replacement-key");
}

#[test]
fn remote_commands_are_bounded_and_probes_are_cached_per_vm() {
    let (root, runtime, _) = workspace_fixture();
    let mut guest = runtime.guest.unwrap();
    shell_replies(
        root.path(),
        json!({"succeed": {"out":"guest-result"}, "reject": {"err":"denied","status":7},
        "stall": {"delayMs":2000}, "DISABLE_AUTOUPDATER": {"out":"tool:codex\ntool:herdr\nauth:codex\n"}}),
    );
    assert_eq!(
        guest.shell("succeed", Duration::from_secs(2)).unwrap(),
        "guest-result"
    );
    assert!(
        guest
            .shell("reject", Duration::from_secs(2))
            .unwrap_err()
            .to_string()
            .contains("denied")
    );
    assert!(
        guest
            .shell("stall", Duration::from_millis(60))
            .unwrap_err()
            .to_string()
            .contains("timed out")
    );
    guest.probe().unwrap();
    assert!(guest.available.contains("herdr") && guest.authenticated.contains("codex"));
    shell_replies(
        root.path(),
        json!({"DISABLE_AUTOUPDATER": {"out":"tool:claude\nauth:claude\n"}}),
    );
    guest.probe().unwrap();
    assert!(guest.available.contains("herdr"));
    guest.home = guest.home.join("another-vm");
    guest.probe().unwrap();
    assert!(guest.available.contains("claude") && !guest.available.contains("codex"));
    let args = guest
        .command("printf 'literal'\n")
        .unwrap()
        .get_args()
        .map(|s| s.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    for required in [
        "StrictHostKeyChecking=yes",
        "IdentityAgent=none",
        "ForwardAgent=no",
        "ClearAllForwardings=yes",
    ] {
        assert!(args.iter().any(|s| s == required));
    }
}

use firecrab_pki::{certificates, host_id};
use std::process::Command;
use uuid::Uuid;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_firecrab"))
}

#[test]
fn ca_help_contract() {
    let output = cli().args(["ca", "--help"]).output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout)
            .unwrap()
            .replace("firecrab.exe", "firecrab")
            .replace("\r\n", "\n"),
        include_str!("snapshots/ca-help.txt").replace("\r\n", "\n")
    );
}

#[test]
fn ca_commands_are_offline_and_preserve_existing_keys() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("pki");
    let output = cli()
        .args(["--api", "invalid-api", "ca", "init", "--dir"])
        .arg(&dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let original = std::fs::read(dir.join("ca.key")).unwrap();
    assert!(
        !cli()
            .args(["ca", "init", "--dir"])
            .arg(&dir)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(std::fs::read(dir.join("ca.key")).unwrap(), original);
    let id = Uuid::new_v4();
    let output = cli()
        .args(["ca", "issue-host-cert", &id.to_string(), "--dir"])
        .arg(&dir)
        .args(["--san", "localhost", "--san", "127.0.0.1", "--days", "7"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let leaf = dir.join("hosts").join(id.to_string()).join("host.crt");
    assert_eq!(host_id(&certificates(&leaf).unwrap()[0]).unwrap(), id);
    assert!(
        !cli()
            .args(["ca", "issue-host-cert", "not-a-uuid"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        !cli()
            .args(["ca", "issue-host-cert", &id.to_string(), "--days", "0"])
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn https_cli_uses_the_private_ca_and_client_certificate() {
    use firecrab_pki::rustls::{ServerConnection, StreamOwned};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let root = tempfile::tempdir().unwrap();
    let ca = root.path().join("pki");
    firecrab_pki::init_ca(&ca).unwrap();
    let server =
        firecrab_pki::issue_host_cert(&ca, Uuid::new_v4(), vec!["127.0.0.1".into()], 7).unwrap();
    let client_id = Uuid::new_v4();
    let client = firecrab_pki::issue_host_cert(&ca, client_id, vec![], 7).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("https://{}", listener.local_addr().unwrap());
    let config = std::sync::Arc::new(server.server_config().unwrap());
    let thread = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let (socket, _) = loop {
            if let Ok(socket) = listener.accept() {
                break socket;
            }
            assert!(std::time::Instant::now() < deadline, "CLI never connected");
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let connection = ServerConnection::new(config).unwrap();
        let mut stream = StreamOwned::new(connection, socket);
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        assert!(request.starts_with(b"GET /api/vms "));
        let id = host_id(&stream.conn.peer_certificates().unwrap()[0]).unwrap();
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]").unwrap();
        stream.flush().unwrap();
        id
    });
    let output = cli()
        .args(["--api", &base, "vm", "list", "--json"])
        .env("FIRECRAB_TLS_CERT", &client.cert)
        .env("FIRECRAB_TLS_KEY", &client.key)
        .env("FIRECRAB_TLS_CA", &client.ca)
        .output()
        .unwrap();
    let observed = thread.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(observed, client_id);
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "[]");
}

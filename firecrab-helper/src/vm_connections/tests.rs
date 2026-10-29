use super::*;
use crate::firewall::EgressPolicy;
use firecrab_helper_protocol::network::{MacAddr, MicroNetworkIpv6Spec};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

#[test]
fn socket_targets_exclude_unknown_networks_host_gateways_and_foreign_ipv6() {
    let mut network = MicroNetworkSpec {
        micro_network_id: Uuid::new_v4(),
        gateway: "172.30.0.1".parse().unwrap(),
        prefix: 24,
        internet_enabled: true,
        dns_mode: firecrab_api_types::MicroNetworkDnsMode::LegacyForward,
        uplink: None,
        ipv6: Some(MicroNetworkIpv6Spec {
            gateway: "fd00:516::1".parse().unwrap(),
            prefix: 64,
            address_mode: Default::default(),
        }),
    };
    let mut policy = VmPolicy {
        vm_id: Uuid::new_v4(),
        ipv4: "172.30.0.2".parse().unwrap(),
        ipv6: Some("fd00:516::2".parse().unwrap()),
        mac: MacAddr([2, 0, 0, 0, 0, 2]),
        egress: EgressPolicy::Internet,
        security_group: None,
        allow_host_ssh: false,
        port_forwards: vec![],
    };
    assert!(targets(&policy, &[]).is_empty());
    assert_eq!(targets(&policy, &[network.clone()]).len(), 2);
    policy.ipv6 = Some("fd00:999::2".parse().unwrap());
    assert_eq!(
        targets(&policy, &[network.clone()]),
        vec![(network.gateway.into(), policy.ipv4.into())]
    );
    policy.ipv4 = network.gateway;
    assert!(targets(&policy, &[network.clone()]).is_empty());
    policy.ipv4 = "192.0.2.1".parse().unwrap();
    assert!(targets(&policy, &[network.clone()]).is_empty());
    policy.ipv4 = "172.30.0.2".parse().unwrap();
    network.ipv6 = None;
    assert_eq!(targets(&policy, &[network]).len(), 1);
}

#[tokio::test]
async fn invalid_socket_pairs_are_rejected_before_running_a_command() {
    for (source, destination) in [("127.0.0.1", "::1"), ("127.0.0.1", "127.0.0.1")] {
        assert_eq!(
            close(source.parse().unwrap(), destination.parse().unwrap())
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}

async fn request(port: u16, host: &str) -> io::Result<Vec<u8>> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
    stream
        .write_all(
            format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    Ok(response)
}

async fn read_request(stream: &mut TcpStream) -> io::Result<String> {
    let mut request = Vec::new();
    while request.len() < 8192 {
        let mut chunk = [0; 1024];
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        request.extend_from_slice(&chunk[..count]);
        if request.windows(4).any(|part| part == b"\r\n\r\n") {
            return Ok(String::from_utf8_lossy(&request).into_owned());
        }
    }
    Err(io::ErrorKind::InvalidData.into())
}

#[tokio::test]
#[ignore = "requires disposable Linux root, CAP_NET_ADMIN and FIRECRAB_TEST_INGRESS; run by CI"]
async fn reset_dead_upstream_retries_without_cutting_healthy_requests() {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    assert_eq!(
        std::env::var("FIRECRAB_SOCKET_RESET_QA").as_deref(),
        Ok("1")
    );
    let nginx = std::fs::canonicalize(std::env::var("FIRECRAB_TEST_INGRESS").unwrap()).unwrap();
    let tag = Uuid::new_v4().as_bytes()[0];
    let dead: IpAddr = format!("127.216.{tag}.2").parse().unwrap();
    let healthy: IpAddr = format!("127.216.{tag}.3").parse().unwrap();
    let foreign: IpAddr = format!("127.216.{tag}.4").parse().unwrap();
    let source: IpAddr = "127.0.0.1".parse().unwrap();
    let existing = Command::new("ss")
        .args(["-Hnt", "dst", &dead.to_string()])
        .output()
        .await
        .unwrap();
    assert!(
        existing.status.success() && existing.stdout.is_empty(),
        "refusing existing sockets at the test address"
    );
    let dead_listener = TcpListener::bind((dead, 0)).await.unwrap();
    let dead_address = dead_listener.local_addr().unwrap();
    let healthy_listener = TcpListener::bind((healthy, 0)).await.unwrap();
    let healthy_address = healthy_listener.local_addr().unwrap();
    let (dead_started, mut dead_ready) = mpsc::channel(2);
    let (foreign_socket, foreign_ready) = oneshot::channel();
    tokio::spawn(async move {
        let mut foreign_socket = Some(foreign_socket);
        loop {
            let (mut stream, peer) = dead_listener.accept().await.unwrap();
            if peer.ip() == foreign {
                foreign_socket.take().unwrap().send(stream).unwrap();
                continue;
            }
            let started = dead_started.clone();
            tokio::spawn(async move {
                read_request(&mut stream).await.unwrap();
                started.send(()).await.unwrap();
                let _ = stream.read(&mut [0; 1]).await;
            });
        }
    });
    let (held_started, mut held_ready) = mpsc::channel(1);
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = healthy_listener.accept().await.unwrap();
            let started = held_started.clone();
            tokio::spawn(async move {
                let request = read_request(&mut stream).await.unwrap();
                if request.contains("held.example.test") {
                    started.send(()).await.unwrap();
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nold",
                    )
                    .await;
            });
        }
    });
    let port = TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("logs")).unwrap();
    let config: firecrab_helper_protocol::pipe::Config = serde_json::from_value(serde_json::json!({
        "listen": format!("127.0.0.1:{port}"), "routes": [
            {"host":"failed.example.test", "upstreams":[dead_address.to_string(), healthy_address.to_string()]},
            {"host":"held.example.test", "upstreams":[healthy_address.to_string()]}
        ]
    })).unwrap();
    // Keep test-owned temp files at this UID; the test grants only NET_ADMIN.
    let configuration = format!(
        "user root;\n{}",
        config
            .nginx()
            .replace("worker_processes auto;", "worker_processes 1;")
            .replace(
                "/run/firecrab-ingress/status.sock",
                &directory.path().join("status.sock").display().to_string(),
            )
    );
    std::fs::write(directory.path().join("nginx.conf"), configuration).unwrap();
    let log = std::fs::File::create(directory.path().join("nginx.log")).unwrap();
    let mut ingress = Command::new(nginx)
        .args([
            "-p",
            &format!("{}/", directory.path().display()),
            "-c",
            "nginx.conf",
            "-g",
            "daemon off; master_process off;",
        ])
        .stderr(Stdio::from(log))
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                break;
            }
            assert!(
                ingress.try_wait().unwrap().is_none(),
                "{}",
                std::fs::read_to_string(directory.path().join("nginx.log")).unwrap()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind((foreign, 0).into()).unwrap();
    let mut foreign_client = socket.connect(dead_address).await.unwrap();
    let mut foreign_server = foreign_ready.await.unwrap();
    let held = tokio::spawn(request(port, "held.example.test"));
    tokio::time::timeout(Duration::from_secs(2), held_ready.recv())
        .await
        .unwrap()
        .unwrap();
    let failed = tokio::spawn(request(port, "failed.example.test"));
    tokio::time::timeout(Duration::from_secs(2), dead_ready.recv())
        .await
        .unwrap()
        .unwrap();
    let sockets = Command::new("ss")
        .args(["-Hnt", "src", &source.to_string(), "dst", &dead.to_string()])
        .output()
        .await
        .unwrap();
    assert!(
        sockets.status.success() && !sockets.stdout.is_empty(),
        "missing gateway-to-guest test connection"
    );
    let started = std::time::Instant::now();
    close(source, dead).await.unwrap();
    let response = tokio::time::timeout(Duration::from_secs(2), failed)
        .await
        .unwrap_or_else(|error| {
            panic!(
                "dead upstream was not retried: {error}; {}",
                std::fs::read_to_string(directory.path().join("nginx.log")).unwrap()
            )
        })
        .unwrap()
        .unwrap();
    assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n") && response.ends_with(b"old"));
    assert!(started.elapsed() < Duration::from_secs(2));
    foreign_server.write_all(b"peer").await.unwrap();
    let mut peer = [0; 4];
    tokio::time::timeout(Duration::from_secs(1), foreign_client.read_exact(&mut peer))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&peer, b"peer", "a different source connection was reset");
    let response = tokio::time::timeout(Duration::from_secs(5), held)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n") && response.ends_with(b"old"));
    close(source, dead).await.unwrap();
    ingress.kill().await.unwrap();
    ingress.wait().await.unwrap();
}

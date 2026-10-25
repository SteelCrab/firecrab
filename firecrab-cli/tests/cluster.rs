use firecrab_api_types::*;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use uuid::Uuid;

fn placement() -> VmPlacement {
    let host = Uuid::from_u128(1);
    let id = Uuid::from_u128(2);
    let target = PlacementTarget {
        network: HostResourceRef {
            host_id: host,
            resource_id: Uuid::from_u128(3).to_string(),
        },
        storage: HostResourceRef {
            host_id: host,
            resource_id: "default".into(),
        },
    };
    VmPlacement {
        id,
        idempotency_key: "retry-key".into(),
        vm: HostResourceRef {
            host_id: host,
            resource_id: id.to_string(),
        },
        target: target.clone(),
        spec: PlacementSpec {
            name: "vm-one".into(),
            image: HostImage {
                alias: "fixture".into(),
                version: "v1".into(),
                kernel_sha256: "a".repeat(64),
                initrd_sha256: None,
                rootfs_sha256: "b".repeat(64),
                boot_args_sha256: "c".repeat(64),
            },
            cpu: 1,
            ram: 512,
            disk_gb: 2,
            egress_policy: EgressPolicy::Isolated,
            env: BTreeMap::new(),
            required_labels: BTreeMap::new(),
            targets: vec![target],
        },
        generation: 0,
        action: PlacementAction::Create,
        phase: PlacementPhase::Pending,
        observed: None,
        error: None,
        created_at_ms: 1,
    }
}
fn server(response: serde_json::Value) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut byte = [0];
        while !bytes.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            bytes.push(byte[0]);
        }
        let headers = String::from_utf8(bytes.clone()).unwrap();
        let length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        bytes.extend(body);
        let body = response.to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        String::from_utf8(bytes).unwrap()
    });
    (url, task)
}
fn cli(api: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_firecrab"));
    command.args(["--api", api]);
    command
}

#[test]
fn placement_preserves_the_composite_resources_and_retry_key() {
    let p = placement();
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("placement.json");
    std::fs::write(&file, serde_json::to_vec(&p.spec).unwrap()).unwrap();
    let (url, request) = server(serde_json::to_value(&p).unwrap());
    let output = cli(&url)
        .args(["cluster", "vm", "place", "--file"])
        .arg(file)
        .args(["--idempotency-key", "retry-key", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<VmPlacement>(&output.stdout).unwrap(),
        p
    );
    let request = request.join().unwrap();
    assert!(request.starts_with("POST /api/cluster/placements HTTP/1.1"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("idempotency-key: retry-key\r\n")
    );
    let body = request.split_once("\r\n\r\n").unwrap().1;
    assert_eq!(serde_json::from_str::<PlacementSpec>(body).unwrap(), p.spec);
}

#[test]
fn lifecycle_and_drain_commands_use_the_controller_resource_paths() {
    let p = placement();
    for action in ["start", "stop", "delete", "reconcile"] {
        let (url, request) = server(serde_json::to_value(&p).unwrap());
        let output = cli(&url)
            .args(["cluster", "vm", action, &p.id.to_string(), "--json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(request.join().unwrap().starts_with(&format!(
            "POST /api/cluster/placements/{}/{action} HTTP/1.1",
            p.id
        )));
    }
    let host = ClusterHost {
        report: HostReport {
            id: p.vm.host_id,
            name: "worker".into(),
            endpoint: "https://worker.test:5523".into(),
            architecture: "aarch64".into(),
            capacity: HostCapacity {
                cpu: 2,
                ram_mib: 1024,
            },
            unmanaged: HostCapacity::default(),
            images: vec![p.spec.image.clone()],
            networks: vec![],
            storages: vec![],
            labels: BTreeMap::new(),
        },
        last_seen_ms: 1,
        draining: true,
        online: true,
        reserved: HostCapacity::default(),
        reserved_disk_gib: BTreeMap::new(),
    };
    for (action, draining) in [("drain", true), ("resume", false)] {
        let (url, request) = server(serde_json::to_value(&host).unwrap());
        let output = cli(&url)
            .args([
                "cluster",
                "host",
                action,
                &host.report.id.to_string(),
                "--json",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let request = request.join().unwrap();
        assert!(request.starts_with(&format!(
            "PATCH /api/cluster/hosts/{} HTTP/1.1",
            host.report.id
        )));
        let body: DrainHostRequest =
            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body.draining, draining);
    }
}

#[test]
fn cluster_hosts_are_separate_from_saved_host_profiles_and_placement_requires_a_key() {
    let binary = env!("CARGO_BIN_EXE_firecrab");
    let profiles = Command::new(binary)
        .args(["host", "--help"])
        .output()
        .unwrap();
    let profiles = String::from_utf8(profiles.stdout).unwrap();
    assert!(profiles.contains("add"));
    assert!(profiles.contains("use"));
    let cluster = Command::new(binary)
        .args(["cluster", "host", "--help"])
        .output()
        .unwrap();
    let cluster = String::from_utf8(cluster.stdout).unwrap();
    assert!(cluster.contains("drain"));
    assert!(cluster.contains("resume"));
    let missing = Command::new(binary)
        .args(["cluster", "vm", "place", "--file", "missing.json"])
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("--idempotency-key"));
}

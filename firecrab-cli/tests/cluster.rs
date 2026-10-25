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

fn registered_host(id: u128, online: bool, draining: bool) -> ClusterHost {
    ClusterHost {
        report: HostReport {
            id: Uuid::from_u128(id),
            name: format!("worker-{id}"),
            endpoint: "https://worker.test:5523".into(),
            architecture: "aarch64".into(),
            capacity: HostCapacity {
                cpu: 4,
                ram_mib: 2048,
            },
            unmanaged: HostCapacity::default(),
            images: vec![],
            networks: vec![],
            storages: vec![],
            labels: BTreeMap::new(),
        },
        online,
        draining,
        last_seen_ms: 1,
        reserved: HostCapacity {
            cpu: 2,
            ram_mib: 512,
        },
        reserved_disk_gib: BTreeMap::new(),
    }
}

#[test]
fn host_list_reports_health_and_reservations_and_show_returns_a_single_json_object() {
    let hosts = vec![
        registered_host(1, true, false),
        registered_host(2, true, true),
        registered_host(3, false, true),
    ];
    let (url, request) = server(serde_json::to_value(&hosts).unwrap());
    let output = cli(&url)
        .args(["cluster", "host", "list"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    for (host, status) in hosts.iter().zip(["ready", "draining", "offline"]) {
        assert!(
            text.contains(&format!(
                "{}\t{}\t{}\t2/4\t512/2048",
                host.report.id, host.report.name, status
            )),
            "{text}"
        );
    }
    assert!(
        request
            .join()
            .unwrap()
            .starts_with("GET /api/cluster/hosts HTTP/1.1")
    );
    let (url, request) = server(serde_json::to_value(&hosts).unwrap());
    let output = cli(&url)
        .args(["cluster", "host", "list", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Vec<ClusterHost>>(&output.stdout).unwrap(),
        hosts
    );
    request.join().unwrap();
    let (url, request) = server(serde_json::to_value(&hosts[0]).unwrap());
    let output = cli(&url)
        .args([
            "cluster",
            "host",
            "show",
            &hosts[0].report.id.to_string(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<ClusterHost>(&output.stdout).unwrap(),
        hosts[0]
    );
    assert!(request.join().unwrap().starts_with(&format!(
        "GET /api/cluster/hosts/{} HTTP/1.1",
        hosts[0].report.id
    )));
}

#[test]
fn placement_list_and_show_preserve_owner_and_unresolved_error_in_human_and_json_output() {
    let mut p = placement();
    p.phase = PlacementPhase::Uncertain;
    p.error = Some("owner result is unresolved".into());
    for (command, json) in [
        ("list", false),
        ("show", false),
        ("list", true),
        ("show", true),
    ] {
        let response = if command == "list" {
            serde_json::json!([p])
        } else {
            serde_json::json!(p)
        };
        let (url, request) = server(response);
        let mut process = cli(&url);
        process.args(["cluster", "vm", command]);
        if command == "show" {
            process.arg(p.id.to_string());
        }
        if json {
            process.arg("--json");
        }
        let output = process.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if json {
            let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            if command == "list" {
                assert_eq!(result, serde_json::json!([p]));
            } else {
                assert_eq!(result, serde_json::json!(p));
            }
        } else {
            let text = String::from_utf8(output.stdout).unwrap();
            for value in [
                p.id.to_string(),
                p.vm.host_id.to_string(),
                p.spec.name.clone(),
                p.error.clone().unwrap(),
            ] {
                assert!(text.contains(&value), "{text}");
            }
            assert!(text.to_ascii_lowercase().contains("uncertain"));
        }
        let path = if command == "list" {
            "/api/cluster/placements".into()
        } else {
            format!("/api/cluster/placements/{}", p.id)
        };
        assert!(
            request
                .join()
                .unwrap()
                .starts_with(&format!("GET {path} HTTP/1.1"))
        );
    }
}

#[test]
fn unreadable_or_invalid_placement_files_fail_before_sending_a_request() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("placement.json");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    for contents in [None, Some("{invalid json")] {
        if let Some(contents) = contents {
            std::fs::write(&file, contents).unwrap();
        }
        let output = cli(&url)
            .args(["cluster", "vm", "place", "--file"])
            .arg(&file)
            .args(["--idempotency-key", "file-check"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!output.stderr.is_empty());
        assert!(
            matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "invalid files must not open an API connection"
        );
    }
}

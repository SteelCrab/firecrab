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

fn controller_service() -> ClusterService {
    let spec:ClusterServiceSpec=serde_json::from_value(serde_json::json!({
        "service":{"name":"web","image":format!("ghcr.io/team/web@sha256:{}","a".repeat(64)),"port":8080,"ram":512,"cpu":1,"replicas":1,"env":{"SESSION_URL":"https://sessions.example"}},
        "state":{"mode":"external_state","sessions":"SESSION_URL","requestsDuplicateSafe":true,"backgroundWork":"duplicate_safe"},
        "credentials":{"TOKEN":{"secretId":Uuid::from_u128(5),"version":7}},
        "targets":[{"hostId":Uuid::from_u128(1),"replicas":1,"request":{"microNetworkId":Uuid::from_u128(2),"hostPort":30100}}]
    })).unwrap();
    ClusterService {
        id: Uuid::from_u128(3),
        spec,
        generation: 1,
        release_generation: 1,
        operation_id: Uuid::from_u128(4),
        desired: ClusterServiceDesired::Running,
        observed: ClusterServiceObserved::Pending,
        pending: true,
        degraded: false,
        hosts: vec![],
    }
}

#[test]
fn service_mutations_preserve_retry_keys_specification_and_expected_generation() {
    let s = controller_service();
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("service.json");
    std::fs::write(&file, serde_json::to_vec(&s.spec).unwrap()).unwrap();
    for action in ["create", "update", "stop"] {
        let (url, request) = server(serde_json::to_value(&s).unwrap());
        let mut command = cli(&url);
        command.args(["cluster", "service", action]);
        if action != "create" {
            command.args([&s.id.to_string(), "--expected-generation", "1"]);
        }
        if action != "stop" {
            command.arg("--file").arg(&file);
        }
        command.args(["--idempotency-key", "service-retry", "--json"]);
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<ClusterService>(&output.stdout).unwrap(),
            s
        );
        let request = request.join().unwrap();
        let path = if action == "create" {
            "/api/cluster/services".into()
        } else {
            format!("/api/cluster/services/{}/{action}", s.id)
        };
        assert!(request.starts_with(&format!("POST {path} HTTP/1.1")));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("idempotency-key: service-retry")
        );
        let (_, body) = request.split_once("\r\n\r\n").unwrap();
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        if action != "create" {
            assert_eq!(body["expectedGeneration"], 1);
        }
        if action != "stop" {
            assert_eq!(
                if action == "create" {
                    body
                } else {
                    body["spec"].clone()
                },
                serde_json::to_value(&s.spec).unwrap()
            );
        }
    }
}

#[test]
fn service_status_and_operation_history_are_available_from_the_selected_controller() {
    let s = controller_service();
    let (url, request) = server(serde_json::to_value(vec![s.clone()]).unwrap());
    let output = cli(&url)
        .args(["cluster", "service", "list"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains(&s.id.to_string()));
    assert!(text.contains("Pending"));
    assert!(
        request
            .join()
            .unwrap()
            .starts_with("GET /api/cluster/services HTTP/1.1")
    );
    for action in ["show", "reconcile"] {
        let (url, request) = server(serde_json::to_value(&s).unwrap());
        let output = cli(&url)
            .args(["cluster", "service", action, &s.id.to_string(), "--json"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            serde_json::from_slice::<ClusterService>(&output.stdout).unwrap(),
            s
        );
        let expected = if action == "show" {
            format!("GET /api/cluster/services/{} HTTP/1.1", s.id)
        } else {
            format!("POST /api/cluster/services/{}/reconcile HTTP/1.1", s.id)
        };
        assert!(request.join().unwrap().starts_with(&expected));
    }
    let (url, request) = server(serde_json::json!([]));
    let output = cli(&url)
        .args(["cluster", "service", "operations", &s.id.to_string()])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!([])
    );
    assert!(request.join().unwrap().starts_with(&format!(
        "GET /api/cluster/services/{}/operations HTTP/1.1",
        s.id
    )));
}

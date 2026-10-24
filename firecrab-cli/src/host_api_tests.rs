use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use clap::Parser;
use serde_json::{Value, json};

use super::{Cli, run};

const VM_ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const NETWORK_ID: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

#[test]
fn stacks_apply_sends_reviewed_fingerprint_exact_retry_identity_and_stop_permission() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("stack.yaml");
    let reviewed = directory.path().join("plan.json");
    std::fs::write(&file, include_str!("../../examples/stacks/basic.yaml")).unwrap();
    let fingerprint = "a".repeat(64);
    std::fs::write(&reviewed,json!({"stackId":VM_ID,"name":"demo","revision":0,"fingerprint":fingerprint,"blocked":false,"changes":[]}).to_string()).unwrap();
    for (state, expected_code) in [("queued", 0), ("failed", 1), ("interrupted", 1)] {
        let status = json!({"operationId":NETWORK_ID,"stackId":VM_ID,"name":"demo","state":state,"changes":[],"error":null});
        let (code, request) = run_once(
            &[
                "stack",
                "apply",
                "--file",
                file.to_str().unwrap(),
                "--expected-plan",
                reviewed.to_str().unwrap(),
                "--operation-id",
                NETWORK_ID,
                "--allow-stop",
                "--no-wait",
                "--json",
            ],
            "202 Accepted",
            &status.to_string(),
        );
        assert_eq!(code, expected_code);
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/stacks/demo/apply");
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["operationId"], NETWORK_ID);
        assert_eq!(body["expectedFingerprint"], fingerprint);
        assert_eq!(body["allowStop"], true);
        assert_eq!(body["stack"]["name"], "demo");
    }
    let status = json!({"operationId":NETWORK_ID,"stackId":VM_ID,"name":"demo","state":"interrupted","changes":[],"error":null});
    let (code, request) = run_once(
        &["stack", "apply-status", "demo", "--id", NETWORK_ID],
        "200 OK",
        &status.to_string(),
    );
    assert_eq!(code, 0);
    assert_eq!(request.method, "GET");
    assert_eq!(
        request.path,
        format!("/api/stacks/demo/applies/{NETWORK_ID}")
    );
}

#[test]
fn stacks_apply_retries_an_existing_operation_even_when_its_current_plan_is_blocked() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("stack.yaml");
    std::fs::write(&file, include_str!("../../examples/stacks/basic.yaml")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let plan = json!({"stackId":VM_ID,"name":"demo","revision":1,"fingerprint":"b".repeat(64),"blocked":true,"changes":[]});
    let queued = json!({"operationId":NETWORK_ID,"stackId":VM_ID,"name":"demo","state":"queued","changes":[],"error":null});
    let mut succeeded = queued.clone();
    succeeded["state"] = json!("succeeded");
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, body) in [
            ("200 OK", plan),
            ("202 Accepted", queued),
            ("200 OK", succeeded),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            requests.push(read_request(&mut stream));
            let body = body.to_string();
            write!(stream,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
        requests
    });
    let code = run(Cli::try_parse_from([
        "firecrab",
        "--api",
        &base,
        "stack",
        "apply",
        "--file",
        file.to_str().unwrap(),
        "--operation-id",
        NETWORK_ID,
        "--json",
    ])
    .unwrap());
    let requests = server.join().unwrap();
    assert_eq!(code, 0);
    assert_eq!(requests[0].path, "/api/stacks/demo/plan");
    assert_eq!(requests[1].path, "/api/stacks/demo/apply");
    assert_eq!(
        requests[2].path,
        format!("/api/stacks/demo/applies/{NETWORK_ID}")
    );
    let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(body["operationId"], NETWORK_ID);
}

#[test]
fn stacks_delete_sends_the_exact_registration_uuid() {
    let (code, request) = run_once(
        &["stack", "delete", "demo", "--id", VM_ID],
        "204 No Content",
        "",
    );
    assert_eq!(code, 0);
    assert_eq!(request.method, "DELETE");
    assert_eq!(request.path, format!("/api/stacks/demo?id={VM_ID}"));
    assert!(request.body.is_empty());
    let (code, _) = run_once(
        &["stack", "delete", "demo", "--id", VM_ID],
        "409 Conflict",
        r#"{"error":{"code":"stack_not_empty"}}"#,
    );
    assert_eq!(code, 1);
}

#[test]
fn stacks_metadata_commands_send_only_registration_and_exact_uuid_bindings() {
    let stack = json!({"id": VM_ID,"name":"demo","revision":0,"bindings":[]});
    let (code, request) = run_once(
        &["stack", "register", "--name", "demo"],
        "200 OK",
        &stack.to_string(),
    );
    assert_eq!(code, 0);
    assert_eq!(request.path, "/api/stacks");
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).unwrap(),
        json!({"name":"demo"})
    );
    for action in ["bind", "unbind"] {
        let (code, request) = run_once(
            &[
                "stack", action, "--stack", "demo", "--kind", "network", "--key", "app", "--id",
                NETWORK_ID,
            ],
            "200 OK",
            &stack.to_string(),
        );
        assert_eq!(code, 0);
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.path,
            format!(
                "/api/stacks/demo/{}",
                if action == "bind" {
                    "bindings"
                } else {
                    "unbind"
                }
            )
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&request.body).unwrap(),
            json!({"kind":"network","key":"app","resourceId":NETWORK_ID})
        );
    }
    let (code, request) = run_once(&["stack", "status", "demo"], "200 OK", &stack.to_string());
    assert_eq!(code, 0);
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/api/stacks/demo");
    let (code, request) = run_once(&["stack", "list"], "200 OK", &json!([stack]).to_string());
    assert_eq!(code, 0);
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/api/stacks");
}

#[test]
fn stacks_plan_posts_the_validated_declaration_and_propagates_blocked_status() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("stack.yaml");
    std::fs::write(&file, include_str!("../../examples/stacks/basic.yaml")).unwrap();
    for blocked in [false, true] {
        let response =
            json!({"stackId":VM_ID,"name":"demo","revision":3,"blocked":blocked,"changes":[]});
        let (code, request) = run_once(
            &["stack", "plan", "--file", file.to_str().unwrap(), "--json"],
            "200 OK",
            &response.to_string(),
        );
        assert_eq!(code, i32::from(blocked));
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/stacks/demo/plan");
        let declaration: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(declaration["apiVersion"], "firecrab.dev/v1alpha1");
        assert!(declaration.get("microVMs").is_some());
    }
}

#[test]
fn stacks_invalid_manifests_fail_before_an_http_request() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("stack.yaml");
    std::fs::write(&file, "name: invalid\n").unwrap();
    let code = run(Cli::try_parse_from([
        "firecrab",
        "--api",
        &base,
        "stack",
        "plan",
        "--file",
        file.to_str().unwrap(),
    ])
    .unwrap());
    assert_eq!(code, 1);
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn schedules_send_timing_and_use_the_host_api_for_controls() {
    let response =
        json!({"id": NETWORK_ID, "name":"morning", "vmId":VM_ID, "action":"start", "enabled":true,
        "timing":{"kind":"cron","expression":"0 9 * * Mon-Fri","timeZone":"Asia/Seoul"},
        "nextRunAtMs":1,"createdAtMs":0,"updatedAtMs":0,"lastRun":null})
        .to_string();
    let flags = [
        "--name",
        "morning",
        "--vm",
        VM_ID,
        "--action",
        "start",
        "--cron",
        "0 9 * * Mon-Fri",
        "--time-zone",
        "Asia/Seoul",
    ];
    for (command, expected_method) in [("create", "POST"), ("update", "PUT")] {
        let mut args = vec!["schedule", command];
        if command == "update" {
            args.push(NETWORK_ID);
        }
        args.extend(flags);
        let (code, request) = run_once(&args, "200 OK", &response);
        assert_eq!(code, 0);
        assert_eq!(request.method, expected_method);
        assert_eq!(
            request.path,
            if command == "create" {
                "/api/schedules".to_owned()
            } else {
                format!("/api/schedules/{NETWORK_ID}")
            }
        );
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["vmId"], VM_ID);
        assert_eq!(
            body["timing"],
            json!({"kind":"cron","expression":"0 9 * * Mon-Fri","timeZone":"Asia/Seoul"})
        );
    }
    for (command, enabled) in [("enable", true), ("disable", false)] {
        let (code, request) = run_once(&["schedule", command, NETWORK_ID], "200 OK", &response);
        assert_eq!(code, 0);
        assert_eq!(request.path, format!("/api/schedules/{NETWORK_ID}/enabled"));
        assert_eq!(
            serde_json::from_slice::<Value>(&request.body).unwrap(),
            json!({"enabled":enabled})
        );
    }
    let (code, request) = run_once(&["schedule", "runs", NETWORK_ID], "200 OK", "[]");
    assert_eq!(code, 0);
    assert_eq!(request.path, format!("/api/schedules/{NETWORK_ID}/runs"));
    let (code, _) = run_once(
        &["schedule", "delete", NETWORK_ID],
        "409 Conflict",
        r#"{"error":{"code":"schedule_running"}}"#,
    );
    assert_eq!(code, 1);
}

fn service_deployment_response(phase: &str) -> Value {
    json!({
        "id": NETWORK_ID, "serviceId": VM_ID, "phase": phase,
        "spec": { "name": "web", "image": "busybox:latest", "port": 8080, "ram": 128, "cpu": 1 },
        "request": { "hostPort": 30100, "microNetworkId": null },
        "digest": null, "instances": [], "log": [], "error": null,
    })
}

#[test]
fn micro_services_deploy_status_and_stop_use_the_runtime_api() {
    let response = service_deployment_response("queued").to_string();
    let (code, request) = run_once(
        &[
            "microservice",
            "deploy",
            VM_ID,
            "--host-port",
            "30100",
            "--no-wait",
        ],
        "202 Accepted",
        &response,
    );
    assert_eq!(code, 0);
    assert_eq!(request.method, "POST");
    assert_eq!(
        request.path,
        format!("/api/micro-services/{VM_ID}/deployment")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).unwrap(),
        json!({"hostPort": 30100, "microNetworkId": null})
    );
    let (code, request) = run_once(&["microservice", "status", VM_ID], "200 OK", &response);
    assert_eq!(code, 0);
    assert_eq!(request.method, "GET");
    assert_eq!(
        request.path,
        format!("/api/micro-services/{VM_ID}/deployment")
    );
    let (code, request) = run_once(
        &["microservice", "stop", VM_ID, "--no-wait"],
        "202 Accepted",
        &service_deployment_response("stopping").to_string(),
    );
    assert_eq!(code, 0);
    assert_eq!(request.path, format!("/api/micro-services/{VM_ID}/stop"));
    assert!(request.body.is_empty());
}

#[test]
fn micro_services_waiting_deploy_returns_failure_to_the_caller() {
    let mut failed = service_deployment_response("failed");
    failed["error"] = json!("App health check failed");
    let (code, _) = run_once(
        &["microservice", "deploy", VM_ID, "--host-port", "30100"],
        "202 Accepted",
        &failed.to_string(),
    );
    assert_eq!(code, 1);
}

#[test]
fn micro_services_commands_send_configuration_security_group_secret_and_storage_references() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("orders.json");
    let spec = json!({
        "name": "orders", "image": "ghcr.io/team/orders:1.0", "runtime": "vm",
        "port": 8080, "healthPath": "/health", "domain": null, "ram": 512, "cpu": 1,
        "replicas": 2, "startupTimeout": 300, "drainTimeout": 30,
        "securityGroupId": VM_ID,
        "env": {"PROFILE": "prod"}, "secrets": {"DB_PASSWORD": NETWORK_ID},
        "storageConnectionId": "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
    });
    std::fs::write(&file, spec.to_string()).unwrap();
    let file = file.to_str().unwrap();
    let mut response = spec.clone();
    response["id"] = json!(VM_ID);
    for (args, method, path, body) in [
        (
            vec!["microservice", "list", "--json"],
            "GET",
            "/api/micro-services".to_owned(),
            json!([response.clone()]),
        ),
        (
            vec!["microservice", "get", VM_ID],
            "GET",
            format!("/api/micro-services/{VM_ID}"),
            response.clone(),
        ),
        (
            vec!["microservice", "create", "--file", file],
            "POST",
            "/api/micro-services".to_owned(),
            response.clone(),
        ),
        (
            vec!["microservice", "update", VM_ID, "--file", file],
            "PUT",
            format!("/api/micro-services/{VM_ID}"),
            response,
        ),
    ] {
        let (code, request) = run_once(&args, "200 OK", &body.to_string());
        assert_eq!(code, 0);
        assert_eq!(request.method, method);
        assert_eq!(request.path, path);
        if matches!(method, "POST" | "PUT") {
            assert_eq!(request.content_type.as_deref(), Some("application/json"));
            assert_eq!(
                serde_json::from_slice::<Value>(&request.body).unwrap(),
                spec
            );
        }
    }
    let (code, request) = run_once(&["microservice", "delete", VM_ID], "204 No Content", "");
    assert_eq!(code, 0);
    assert_eq!(request.method, "DELETE");
    assert_eq!(request.path, format!("/api/micro-services/{VM_ID}"));
}

#[test]
fn micro_services_fail_on_invalid_files_and_api_errors() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("invalid.json");
    std::fs::write(&file, "{bad json}").unwrap();
    let cli = Cli::try_parse_from([
        "firecrab",
        "microservice",
        "create",
        "--file",
        file.to_str().unwrap(),
    ])
    .unwrap();
    assert_eq!(run(cli), 1);
    let (code, _) = run_once(
        &["microservice", "get", VM_ID],
        "404 Not Found",
        r#"{"error":{"code":"not_found","message":"resource not found"}}"#,
    );
    assert_eq!(code, 1);
}

struct CapturedRequest {
    method: String,
    path: String,
    content_type: Option<String>,
    idempotency_key: Option<String>,
    body: Vec<u8>,
}

fn run_once(args: &[&str], status: &str, body: &str) -> (i32, CapturedRequest) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let status = status.to_owned();
    let response = body.as_bytes().to_vec();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            response.len()
        )
        .unwrap();
        stream.write_all(&response).unwrap();
        request
    });

    let mut argv = vec!["firecrab", "--api", base.as_str()];
    argv.extend_from_slice(args);
    let code = run(Cli::try_parse_from(argv).unwrap());
    (code, server.join().unwrap())
}

fn read_request(stream: &mut TcpStream) -> CapturedRequest {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 1024];
    let header_end = loop {
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0, "connection closed before HTTP headers");
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(position) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break position + 4;
        }
    };

    let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
    let mut request_line = headers.lines().next().unwrap().split_whitespace();
    let method = request_line.next().unwrap().to_owned();
    let path = request_line.next().unwrap().to_owned();
    let header = |wanted: &str| {
        headers.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case(wanted)
                .then(|| value.trim().to_owned())
        })
    };
    let content_type = header("content-type");
    let idempotency_key = header("idempotency-key");
    let content_length = header("content-length")
        .map(|value| value.parse::<usize>().unwrap())
        .unwrap_or(0);

    while bytes.len() < header_end + content_length {
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0, "connection closed before HTTP body");
        bytes.extend_from_slice(&chunk[..read]);
    }

    CapturedRequest {
        method,
        path,
        content_type,
        idempotency_key,
        body: bytes[header_end..header_end + content_length].to_vec(),
    }
}

fn network_response() -> Value {
    json!({
        "id": NETWORK_ID,
        "name": "lab",
        "subnetCidr": "172.31.0.0/24",
        "gateway": "172.31.0.1",
        "internetEnabled": true,
        "uplink": null,
        "ipv6Cidr": null,
        "ipv6Gateway": null,
        "ipv6AddressMode": null,
        "ipv6Egress": null
    })
}

fn vm_response() -> Value {
    json!({
        "id": VM_ID,
        "name": "web-1",
        "state": "created",
        "template": "alpine-3.24.1",
        "templateVersion": "3.24.1",
        "cpu": 1,
        "ram": 512,
        "diskGb": 2,
        "startupStep": null,
        "startupTimeline": [],
        "egressPolicy": "internet",
        "ipv4": "172.31.0.10",
        "ipv6": null,
        "mac": "02:fc:00:00:00:01",
        "hostname": "fc-aaaaaaaaaaaa",
        "microNetworkId": NETWORK_ID,
        "storageRoot": "default",
        "cpuUsagePercent": null,
        "memoryUsedMib": null,
        "memoryTotalMib": null,
        "memoryUsedPercent": null,
        "usageHistory": [],
        "shellRefs": [],
        "portForwards": [],
        "env": {}
    })
}

fn oci_inspect_response() -> Value {
    json!({
        "registry": "docker.io",
        "repository": "library/nginx",
        "version": "1.27",
        "immutable": false,
        "digest": "sha256:abc",
        "architecture": "amd64",
        "singlePlatform": false,
        "alias": "nginx-1.27"
    })
}

fn oci_import_response(status: &str, log: &str) -> Value {
    json!({
        "alias": "nginx-1.27",
        "status": status,
        "log": log,
        "startedAtMs": 1
    })
}

#[test]
fn image_list_uses_the_catalog_endpoint() {
    let response = json!([{
        "alias": "alpine-3.24.1",
        "version": "3.24.1",
        "kernelSha256": "kernel",
        "rootfsSha256": "rootfs",
        "minDiskGb": 2,
        "rootfsSizeBytes": 1024,
        "installed": true,
        "packageStaged": false,
        "description": "Alpine",
        "hasGuestService": false
    }])
    .to_string();
    let (code, request) = run_once(&["image", "list"], "200 OK", &response);

    assert_eq!(code, 0);
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/api/images");
}

#[test]
fn oci_image_commands_match_inspect_import_and_status_endpoints() {
    let inspect_body = oci_inspect_response().to_string();
    let (code, inspect) = run_once(
        &["image", "inspect", "nginx:1.27", "--json"],
        "200 OK",
        &inspect_body,
    );
    assert_eq!(code, 0);
    assert_eq!(inspect.method, "GET");
    assert_eq!(inspect.path, "/api/oci/inspect?reference=nginx%3A1.27");

    let import_body = oci_import_response("running", "import started").to_string();
    let (code, import) = run_once(
        &["image", "import", "nginx:1.27"],
        "202 Accepted",
        &import_body,
    );
    assert_eq!(code, 0);
    assert_eq!(
        (import.method.as_str(), import.path.as_str()),
        ("POST", "/api/oci/import")
    );
    assert_eq!(import.content_type.as_deref(), Some("application/json"));
    assert_eq!(
        serde_json::from_slice::<Value>(&import.body).unwrap(),
        json!({"reference": "nginx:1.27"})
    );

    let failed_body = oci_import_response("failed", "registry rejected blob").to_string();
    let (code, status) = run_once(
        &["image", "import-status", "nginx-1.27", "--json"],
        "200 OK",
        &failed_body,
    );
    assert_eq!(code, 1);
    assert_eq!(status.method, "GET");
    assert_eq!(status.path, "/api/oci/import/nginx-1.27");
}

#[test]
fn network_commands_match_the_micro_network_api() {
    let list_body = json!([network_response()]).to_string();
    let (code, list) = run_once(&["network", "list"], "200 OK", &list_body);
    assert_eq!(code, 0);
    assert_eq!(
        (list.method.as_str(), list.path.as_str()),
        ("GET", "/api/micro-networks")
    );

    let create_body = network_response().to_string();
    let (code, create) = run_once(
        &[
            "network",
            "create",
            "--name",
            "lab",
            "--subnet-cidr",
            "172.31.0.0/24",
            "--json",
        ],
        "201 Created",
        &create_body,
    );
    assert_eq!(code, 0);
    assert_eq!(
        (create.method.as_str(), create.path.as_str()),
        ("POST", "/api/micro-networks")
    );
    assert_eq!(create.content_type.as_deref(), Some("application/json"));
    let body: Value = serde_json::from_slice(&create.body).unwrap();
    assert_eq!(body["name"], "lab");
    assert_eq!(body["subnetCidr"], "172.31.0.0/24");
    assert_eq!(body["internetEnabled"], true);

    let (code, delete) = run_once(
        &["network", "delete", NETWORK_ID, "--json"],
        "204 No Content",
        "",
    );
    assert_eq!(code, 0);
    assert_eq!(
        (delete.method.as_str(), delete.path.as_str()),
        (
            "DELETE",
            format!("/api/micro-networks/{NETWORK_ID}").as_str()
        )
    );
}

#[test]
fn vm_commands_match_the_lifecycle_api() {
    let list_body = json!([vm_response()]).to_string();
    let (code, list) = run_once(&["vm", "list", "--json"], "200 OK", &list_body);
    assert_eq!(code, 0);
    assert_eq!(
        (list.method.as_str(), list.path.as_str()),
        ("GET", "/api/vms")
    );

    let vm_body = vm_response().to_string();
    let (code, create) = run_once(
        &[
            "vm",
            "create",
            "--name",
            "web-1",
            "--template",
            "alpine-3.24.1",
            "--network",
            NETWORK_ID,
        ],
        "201 Created",
        &vm_body,
    );
    assert_eq!(code, 0);
    assert_eq!(
        (create.method.as_str(), create.path.as_str()),
        ("POST", "/api/vms")
    );
    assert_eq!(create.content_type.as_deref(), Some("application/json"));
    let body: Value = serde_json::from_slice(&create.body).unwrap();
    assert_eq!(body["name"], "web-1");
    assert_eq!(body["microNetworkId"], NETWORK_ID);
    assert_eq!(body["egressPolicy"], "internet");

    for action in ["start", "stop"] {
        let expected_path = format!("/api/vms/{VM_ID}/{action}");
        let (code, request) = run_once(&["vm", action, VM_ID, "--json"], "200 OK", &vm_body);
        assert_eq!(code, 0);
        assert_eq!(
            (request.method.as_str(), request.path.as_str()),
            ("POST", expected_path.as_str())
        );
        assert!(request.body.is_empty());
    }

    let (code, delete) = run_once(&["vm", "delete", VM_ID], "204 No Content", "");
    assert_eq!(code, 0);
    assert_eq!(
        (delete.method.as_str(), delete.path.as_str()),
        ("DELETE", format!("/api/vms/{VM_ID}").as_str())
    );
}

const POOL_ID: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const LEASE_ID: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";

fn pool_response() -> Value {
    json!({
        "id": POOL_ID,
        "name": "ci",
        "template": "alpine-3.24.1",
        "templateVersion": "alpine-3.24.1-v5",
        "cpu": 1,
        "ram": 512,
        "diskGb": 2,
        "egressPolicy": "internet",
        "microNetworkId": NETWORK_ID,
        "storageRoot": "default",
        "minReady": 2,
        "maxSize": 4,
        "leaseTtlSeconds": 600,
        "deleting": false,
        "members": [{"vmId": VM_ID, "state": "ready", "createdAtMs": 1}],
        "createdAtMs": 1
    })
}

fn lease_response() -> Value {
    json!({
        "id": LEASE_ID,
        "poolId": POOL_ID,
        "vmId": VM_ID,
        "state": "active",
        "acquiredAtMs": 1_790_000_000_000_u64,
        "expiresAtMs": 1_790_000_600_000_u64,
        "vm": vm_response()
    })
}

#[test]
fn pool_commands_match_the_pool_api() {
    let (code, list) = run_once(
        &["pool", "list", "--json"],
        "200 OK",
        &json!([pool_response()]).to_string(),
    );
    assert_eq!(code, 0);
    assert_eq!(
        (list.method.as_str(), list.path.as_str()),
        ("GET", "/api/pools")
    );

    let pool_body = pool_response().to_string();
    let (code, create) = run_once(
        &[
            "pool",
            "create",
            "--name",
            "ci",
            "--template",
            "alpine-3.24.1",
            "--network",
            NETWORK_ID,
            "--min-ready",
            "2",
            "--max-size",
            "4",
        ],
        "201 Created",
        &pool_body,
    );
    assert_eq!(code, 0);
    assert_eq!(
        (create.method.as_str(), create.path.as_str()),
        ("POST", "/api/pools")
    );
    let body: Value = serde_json::from_slice(&create.body).unwrap();
    assert_eq!(body["name"], "ci");
    assert_eq!(body["microNetworkId"], NETWORK_ID);
    assert_eq!(
        (body["minReady"].clone(), body["maxSize"].clone()),
        (json!(2), json!(4))
    );
    assert_eq!(body["leaseTtlSeconds"], 600);

    let (code, update) = run_once(
        &["pool", "update", POOL_ID, "--min-ready", "3"],
        "200 OK",
        &pool_body,
    );
    assert_eq!(code, 0);
    let pool_path = format!("/api/pools/{POOL_ID}");
    assert_eq!(
        (update.method.as_str(), update.path.as_str()),
        ("PATCH", pool_path.as_str())
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&update.body).unwrap(),
        json!({"minReady": 3})
    );

    let lease_body = lease_response().to_string();
    let (code, acquire) = run_once(
        &["pool", "acquire", POOL_ID, "--idempotency-key", "job-1"],
        "201 Created",
        &lease_body,
    );
    assert_eq!(code, 0);
    assert_eq!(
        (acquire.method.as_str(), acquire.path.as_str()),
        ("POST", format!("/api/pools/{POOL_ID}/acquire").as_str())
    );
    assert_eq!(acquire.idempotency_key.as_deref(), Some("job-1"));

    let (code, leases) = run_once(
        &["pool", "leases", POOL_ID, "--json"],
        "200 OK",
        &json!([lease_response()]).to_string(),
    );
    assert_eq!(code, 0);
    assert_eq!(
        (leases.method.as_str(), leases.path.as_str()),
        ("GET", format!("/api/pools/{POOL_ID}/leases").as_str())
    );

    let (code, release) = run_once(
        &["pool", "release", POOL_ID, LEASE_ID],
        "200 OK",
        &lease_body,
    );
    assert_eq!(code, 0);
    assert_eq!(
        (release.method.as_str(), release.path.as_str()),
        (
            "DELETE",
            format!("/api/pools/{POOL_ID}/leases/{LEASE_ID}").as_str()
        )
    );

    let (code, delete) = run_once(&["pool", "delete", POOL_ID], "202 Accepted", &pool_body);
    assert_eq!(code, 0);
    assert_eq!(
        (delete.method.as_str(), delete.path.as_str()),
        ("DELETE", pool_path.as_str())
    );
}

#[test]
fn an_exhausted_pool_fails_the_acquire_with_the_api_error() {
    let (code, request) = run_once(
        &["pool", "acquire", POOL_ID],
        "409 Conflict",
        r#"{"error":{"code":"pool_exhausted","message":"no ready member right now","requestId":"00000000-0000-0000-0000-000000000000"}}"#,
    );

    assert_eq!(code, 1);
    let key = request
        .idempotency_key
        .expect("an omitted key is generated even on failure");
    assert!(uuid::Uuid::parse_str(&key).is_ok());
}

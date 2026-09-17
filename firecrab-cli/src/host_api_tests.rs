use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use clap::Parser;
use serde_json::{Value, json};

use super::{Cli, run};

const VM_ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const NETWORK_ID: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

struct CapturedRequest {
    method: String,
    path: String,
    content_type: Option<String>,
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

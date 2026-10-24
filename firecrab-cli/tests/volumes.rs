//! Exercise the public volume commands against an isolated HTTP API.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const VOLUME: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const VM: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const NETWORK: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const COLLECTION: &str = "/api/microblockstorage";

type Reply = (&'static str, String, u16, Value);

struct Request {
    method: String,
    path: String,
    body: Option<Value>,
}

fn volume(attached: bool) -> Value {
    json!({
        "id": VOLUME, "name": "app-data", "sizeGib": 10, "storageRoot": "ssd",
        "state": if attached { "attached" } else { "available" },
        "attachedVmId": if attached { Some(VM) } else { None },
        "mountPath": if attached { Some("/data") } else { None }
    })
}

fn read_request(stream: &mut TcpStream) -> Request {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut chunk = [0; 1024];
    let end = loop {
        let count = stream.read(&mut chunk).unwrap();
        assert_ne!(count, 0, "HTTP request ended before its headers");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = std::str::from_utf8(&bytes[..end]).unwrap();
    let mut parts = headers.lines().next().unwrap().split_whitespace();
    let method = parts.next().unwrap().to_owned();
    let path = parts.next().unwrap().to_owned();
    let length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    while bytes.len() < end + length {
        let count = stream.read(&mut chunk).unwrap();
        assert_ne!(count, 0, "HTTP request ended before its body");
        bytes.extend_from_slice(&chunk[..count]);
    }
    Request {
        method,
        path,
        body: (length > 0).then(|| serde_json::from_slice(&bytes[end..end + length]).unwrap()),
    }
}

fn invoke(args: &[&str], replies: Vec<Reply>) -> (Output, Vec<Request>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for (method, path, status, response) in replies {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "missing {method} {path}");
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            let request = read_request(&mut stream);
            assert_eq!(request.method, method);
            assert_eq!(request.path, path);
            requests.push(request);
            let body = if status == 204 {
                String::new()
            } else {
                response.to_string()
            };
            write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
        requests
    });
    let output = Command::new(env!("CARGO_BIN_EXE_firecrab"))
        .args(["--api", &base])
        .args(args)
        .output()
        .unwrap();
    (output, server.join().unwrap())
}

fn successful(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn volume_creation_and_listing_report_the_selected_storage_and_capacity() {
    for json_output in [false, true] {
        let mut args = vec![
            "volume",
            "create",
            "app-data",
            "--size",
            "10",
            "--storage-root",
            "ssd",
        ];
        if json_output {
            args.push("--json");
        }
        let (output, requests) =
            invoke(&args, vec![("POST", COLLECTION.into(), 201, volume(false))]);
        assert_eq!(
            requests[0].body,
            Some(json!({"name":"app-data", "sizeGib":10, "storageRoot":"ssd"}))
        );
        let text = successful(&output);
        if json_output {
            assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), volume(false));
        } else {
            assert!(text.contains("app-data\t10\tavailable\t-\t-"));
        }
        let args = if json_output {
            vec!["volume", "list", "--json"]
        } else {
            vec!["microblockstorage", "list"]
        };
        let (output, requests) = invoke(
            &args,
            vec![("GET", COLLECTION.into(), 200, json!([volume(false)]))],
        );
        assert!(requests[0].body.is_none());
        let text = successful(&output);
        if json_output {
            assert_eq!(
                serde_json::from_str::<Value>(&text).unwrap(),
                json!([volume(false)])
            );
        } else {
            assert!(text.starts_with("ID\tNAME\tSIZE_GIB\tSTATE\tVM\tMOUNT\n"));
            assert!(text.contains(VOLUME));
        }
    }
}

#[test]
fn inspection_resolves_names_and_displays_the_attached_vm_and_mount() {
    let path = format!("{COLLECTION}/{VOLUME}");
    let (output, _) = invoke(
        &["volume", "inspect", "app-data", "--json"],
        vec![
            ("GET", COLLECTION.into(), 200, json!([volume(true)])),
            ("GET", path.clone(), 200, volume(true)),
        ],
    );
    assert_eq!(
        serde_json::from_str::<Value>(&successful(&output)).unwrap(),
        volume(true)
    );
    let (output, _) = invoke(
        &["volume", "inspect", VOLUME],
        vec![("GET", path, 200, volume(true))],
    );
    assert!(successful(&output).contains(&format!("attached\t{VM}\t/data")));
}

#[test]
fn attach_and_detach_preserve_the_volume_identity_and_report_ownership() {
    let path = format!("{COLLECTION}/{VOLUME}");
    let (output, requests) = invoke(
        &[
            "volume",
            "attach",
            "app-data",
            "--vm",
            VM,
            "--mount-path",
            "/data",
            "--json",
        ],
        vec![
            ("GET", COLLECTION.into(), 200, json!([volume(false)])),
            ("POST", format!("{path}/attach"), 200, volume(true)),
        ],
    );
    assert_eq!(
        requests[1].body,
        Some(json!({"vmId":VM, "mountPath":"/data"}))
    );
    assert_eq!(
        serde_json::from_str::<Value>(&successful(&output)).unwrap(),
        volume(true)
    );
    let (output, requests) = invoke(
        &["volume", "detach", VOLUME, "--json"],
        vec![("POST", format!("{path}/detach"), 200, volume(false))],
    );
    assert!(requests[0].body.is_none());
    assert_eq!(
        serde_json::from_str::<Value>(&successful(&output)).unwrap(),
        volume(false)
    );
}

#[test]
fn deletion_reports_success_only_after_the_api_accepts_it() {
    for json_output in [false, true] {
        let mut args = vec!["volume", "delete", VOLUME];
        if json_output {
            args.push("--json");
        }
        let (output, requests) = invoke(
            &args,
            vec![("DELETE", format!("{COLLECTION}/{VOLUME}"), 204, Value::Null)],
        );
        assert!(requests[0].body.is_none());
        let text = successful(&output);
        if json_output {
            assert_eq!(
                serde_json::from_str::<Value>(&text).unwrap(),
                json!({"deleted":VOLUME})
            );
        } else {
            assert_eq!(text, format!("deleted volume {VOLUME}\n"));
        }
    }
    let (output, _) = invoke(
        &["volume", "delete", VOLUME],
        vec![(
            "DELETE",
            format!("{COLLECTION}/{VOLUME}"),
            409,
            json!({"error":{"code":"volume_conflict", "message":"detach the volume before deleting it"}}),
        )],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("409") && error.contains("detach the volume"));
}

#[test]
fn unknown_names_and_api_conflicts_are_returned_as_command_failures() {
    let (output, _) = invoke(
        &[
            "volume",
            "attach",
            "missing",
            "--vm",
            VM,
            "--mount-path",
            "/data",
        ],
        vec![("GET", COLLECTION.into(), 200, json!([]))],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("no volume named")
    );
    let (output, _) = invoke(
        &[
            "volume",
            "attach",
            VOLUME,
            "--vm",
            VM,
            "--mount-path",
            "/data",
        ],
        vec![(
            "POST",
            format!("{COLLECTION}/{VOLUME}/attach"),
            409,
            json!({"error":{"code":"volume_conflict", "message":"volume is already attached"}}),
        )],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("already attached")
    );
}

#[test]
fn vm_creation_resolves_a_named_volume_into_the_requested_guest_path() {
    let response = json!({
        "id": VM, "name":"app", "state":"created", "template":"alpine", "templateVersion":"1",
        "cpu":1, "ram":512, "diskGb":2, "startupStep":null, "egressPolicy":"internet",
        "hostname":"fc-app", "startupTimeline":[], "microNetworkId":NETWORK, "storageRoot":"default",
        "usageHistory":[], "env":{}
    });
    let (output, requests) = invoke(
        &[
            "vm",
            "create",
            "--name",
            "app",
            "--template",
            "alpine",
            "--network",
            NETWORK,
            "--volume",
            "app-data:/var/lib/postgresql/data",
            "--json",
        ],
        vec![
            ("GET", COLLECTION.into(), 200, json!([volume(false)])),
            ("POST", "/api/vms".into(), 201, response.clone()),
        ],
    );
    assert_eq!(
        requests[1].body.as_ref().unwrap()["blockStorages"],
        json!([{"volumeId":VOLUME, "mountPath":"/var/lib/postgresql/data"}])
    );
    assert_eq!(
        serde_json::from_str::<Value>(&successful(&output)).unwrap()["id"],
        response["id"]
    );
}

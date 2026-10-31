use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::Command,
    sync::mpsc,
    thread,
};

type Request = (String, String, Value);

fn server(responses: Vec<Value>) -> (String, mpsc::Receiver<Request>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (send, receive) = mpsc::channel();
    let task = thread::spawn(move || {
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            let mut bytes = vec![];
            let mut chunk = [0; 1024];
            let end = loop {
                let count = stream.read(&mut chunk).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
                if let Some(n) = bytes.windows(4).position(|x| x == b"\r\n\r\n") {
                    break n + 4;
                }
            };
            let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
            let mut first = headers.lines().next().unwrap().split_whitespace();
            let method = first.next().unwrap().to_owned();
            let path = first.next().unwrap().to_owned();
            let len = headers
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            while bytes.len() < end + len {
                let count = stream.read(&mut chunk).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
            }
            let body = if len == 0 {
                Value::Null
            } else {
                serde_json::from_slice(&bytes[end..end + len]).unwrap()
            };
            send.send((method, path, body)).unwrap();
            let body = response.to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    (url, receive, task)
}

fn command(url: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_firecrab"));
    command.args(["--api", url, "iac"]);
    command
}
fn observed() -> Value {
    json!({"endpointId":"11111111-1111-4111-8111-111111111111","resource":{"kind":"network","id":"22222222-2222-4222-8222-222222222222"},"generation":3,
        "owner":{"manager":"terraform","id":"33333333-3333-4333-8333-333333333333","key":"firecrab_network.application"}})
}

#[test]
fn source_removal_cli_retries_one_exact_request_and_reads_retained_generation() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("remove.json");
    let mut ownership = observed();
    ownership["resource"]["kind"] = json!("stack_source");
    ownership["owner"] = Value::Null;
    let input = json!({"operationId":"44444444-4444-4444-8444-444444444444","expectedEndpointId":ownership["endpointId"],"expectedStackId":ownership["resource"]["id"],"expectedGeneration":1});
    std::fs::write(&file, input.to_string()).unwrap();
    let result = json!({"operationId":input["operationId"],"ownership":ownership,"action":"delete_stack_source","state":"succeeded","errorCode":null,"configurationGeneration":1});
    let (url, receive, task) = server(vec![result.clone(), result, json!({"generation":2})]);
    for _ in 0..2 {
        let output = Command::new(env!("CARGO_BIN_EXE_firecrab"))
            .args(["--api", &url, "stack", "source", "remove", "demo", "--file"])
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = Command::new(env!("CARGO_BIN_EXE_firecrab"))
        .args(["--api", &url, "stack", "source", "generation", "demo"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["generation"],
        2
    );
    task.join().unwrap();
    let requests = receive.try_iter().collect::<Vec<_>>();
    assert_eq!(requests[0], requests[1]);
    assert_eq!(
        requests[0],
        ("DELETE".into(), "/api/stacks/demo/source".into(), input)
    );
    assert_eq!(
        requests[2],
        (
            "GET".into(),
            "/api/stacks/demo/source/generation".into(),
            Value::Null
        )
    );
}

#[test]
fn native_operation_cli_observes_without_replay_and_recovers_only_the_original_id() {
    let operation = "44444444-4444-4444-8444-444444444444";
    let project = "22222222-2222-4222-8222-222222222222";
    let mut ownership = observed();
    ownership["resource"]["kind"] = json!("sandbox_project");
    let status = json!({"operationId":operation,"ownership":ownership,"action":"delete_sandbox_project","state":"needs_recovery","errorCode":"server_restarted"});
    let mut completed = status.clone();
    completed["state"] = json!("succeeded");
    completed["errorCode"] = Value::Null;
    let (url, receive, task) = server(vec![json!([status]), status.clone(), completed]);
    for args in [
        vec![
            "native-operations",
            "--kind",
            "sandbox_project",
            "--id",
            project,
        ],
        vec!["native-operation", "--id", operation],
        vec!["recover-native-operation", "--id", operation],
    ] {
        let output = command(&url).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(serde_json::from_slice::<Value>(&output.stdout).is_ok());
    }
    task.join().unwrap();
    let requests = receive.try_iter().collect::<Vec<_>>();
    assert_eq!(
        requests[0],
        (
            "GET".into(),
            format!("/api/iac/native-operations/resources/sandbox_project/{project}"),
            Value::Null
        )
    );
    assert_eq!(
        requests[1],
        (
            "GET".into(),
            format!("/api/iac/native-operations/{operation}"),
            Value::Null
        )
    );
    assert_eq!(
        requests[2],
        (
            "POST".into(),
            format!("/api/iac/native-operations/{operation}/recover"),
            json!({})
        )
    );
}

#[test]
fn ownership_cli_reads_exact_identity_and_immutable_acknowledgements_as_json() {
    let ownership = observed();
    let operation = "44444444-4444-4444-8444-444444444444";
    let (url, receive, task) = server(vec![
        json!({"endpointId":ownership["endpointId"]}),
        json!([ownership]),
        ownership.clone(),
        json!({"operationId":operation,"ownership":ownership}),
    ]);
    for args in [
        vec!["identity"],
        vec!["owners"],
        vec![
            "owner",
            "--kind",
            "network",
            "--id",
            "{22222222-2222-4222-8222-222222222222}",
        ],
        vec!["owner-change", "--id", operation],
    ] {
        let output = command(&url).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(serde_json::from_slice::<Value>(&output.stdout).is_ok());
    }
    task.join().unwrap();
    let requests = receive.try_iter().collect::<Vec<_>>();
    assert_eq!(requests[0].1, "/api/iac/identity");
    assert_eq!(requests[1].1, "/api/iac/owners");
    assert_eq!(
        requests[2].1,
        "/api/iac/owners/network/22222222-2222-4222-8222-222222222222"
    );
    assert_eq!(requests[3].1, format!("/api/iac/owner-changes/{operation}"));
    assert!(requests.iter().all(|request| request.0 == "GET"));
}

#[test]
fn ownership_cli_retries_the_exact_retained_request_without_changing_operation_identity() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("transfer.json");
    let ownership = observed();
    let request = json!({"operationId":"44444444-4444-4444-8444-444444444444","expectedEndpointId":ownership["endpointId"],
        "resource":ownership["resource"],"expectedGeneration":2,"expectedOwner":null,"owner":ownership["owner"]});
    std::fs::write(&file, request.to_string()).unwrap();
    let (url, receive, task) = server(vec![ownership.clone(), ownership]);
    for _ in 0..2 {
        let output = command(&url)
            .args(["change-owner", "--file"])
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    task.join().unwrap();
    let requests = receive.try_iter().collect::<Vec<_>>();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], requests[1]);
    assert_eq!(requests[0].0, "POST");
    assert_eq!(requests[0].1, "/api/iac/owners/transition");
    assert_eq!(requests[0].2, request);
}

#[test]
fn ownership_cli_rejects_invalid_or_oversized_requests_without_echoing_values() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("transfer.json");
    for invalid in [
        b"{\"unexpected\":\"DO-NOT-PRINT\"}".to_vec(),
        vec![b' '; 16 * 1024 + 1],
        vec![0xff],
    ] {
        std::fs::write(&file, invalid).unwrap();
        let output = command("http://127.0.0.1:1")
            .args(["change-owner", "--file"])
            .arg(&file)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("ownership request"));
        assert!(!error.contains("DO-NOT-PRINT"));
        assert!(!error.contains("unreachable"));
    }
}

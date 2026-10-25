use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::Command,
    sync::mpsc,
    thread,
};

type Request = (String, String, Value);

fn source() -> Value {
    json!({"repository":"https://github.com/example/infra","ref":"main","manifestPath":"stack.yaml","paused":true})
}

fn status() -> Value {
    json!({"stackId":"11111111-1111-4111-8111-111111111111","name":"demo","generation":1,
        "source":source(),"nextCheckAt":0,"failures":0,"latest":null})
}

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
    command.args(["--api", url, "stack", "source"]);
    command
}

#[test]
fn source_cli_configures_and_reads_status_and_history_as_json() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("source.json");
    std::fs::write(&file, source().to_string()).unwrap();
    let (url, receive, task) = server(vec![status(), status(), json!([]), status()]);
    let configured = command(&url)
        .args(["configure", "demo", "--expected-generation", "0", "--file"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        configured.status.success(),
        "{}",
        String::from_utf8_lossy(&configured.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&configured.stdout).unwrap()["generation"],
        1
    );
    for args in [
        vec!["status", "demo"],
        vec!["history", "demo"],
        vec!["check", "demo", "--generation", "1"],
    ] {
        let result = command(&url).args(args).output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(serde_json::from_slice::<Value>(&result.stdout).is_ok());
    }
    task.join().unwrap();
    let requests = receive.try_iter().collect::<Vec<_>>();
    assert_eq!(requests[0].0, "PUT");
    assert_eq!(requests[0].1, "/api/stacks/demo/source");
    assert_eq!(requests[0].2["expectedGeneration"], 0);
    assert_eq!(requests[0].2["source"]["pollIntervalSeconds"], 900);
    assert_eq!(requests[1].0, "GET");
    assert_eq!(requests[2].1, "/api/stacks/demo/source/history");
    assert_eq!(requests[3].0, "POST");
    assert_eq!(requests[3].2, json!({"generation":1}));
}

#[test]
fn source_cli_pause_and_resume_preserve_configuration_and_require_current_generation() {
    for (action, paused) in [("pause", true), ("resume", false)] {
        let (url, receive, task) = server(vec![status(), status()]);
        let result = command(&url)
            .args([action, "demo", "--generation", "1"])
            .output()
            .unwrap();
        assert!(result.status.success());
        task.join().unwrap();
        let requests = receive.try_iter().collect::<Vec<_>>();
        assert_eq!(requests[1].2["source"]["paused"], paused);
        assert_eq!(
            requests[1].2["source"]["repository"],
            source()["repository"]
        );
        assert_eq!(requests[1].2["expectedGeneration"], 1);
    }
    let (url, receive, task) = server(vec![status()]);
    let result = command(&url)
        .args(["pause", "demo", "--generation", "0"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    task.join().unwrap();
    assert_eq!(receive.try_iter().count(), 1);
}

#[test]
fn source_cli_rejects_embedded_tokens_and_invalid_names_before_contacting_api() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("source.json");
    for invalid in [
        json!({"repository":"https://private-secret@github.com/example/infra","ref":"main","manifestPath":"stack.yaml"}),
        json!({"repository":"https://github.com/example/infra","ref":"main","manifestPath":"stack.yaml","token":"private-secret"}),
    ] {
        std::fs::write(&file, invalid.to_string()).unwrap();
        let result = command("http://127.0.0.1:1")
            .args(["configure", "demo", "--expected-generation", "0", "--file"])
            .arg(&file)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(!String::from_utf8_lossy(&result.stderr).contains("private-secret"));
        assert!(!String::from_utf8_lossy(&result.stdout).contains("private-secret"));
    }
    let result = command("http://127.0.0.1:1")
        .args(["status", "../other"])
        .output()
        .unwrap();
    assert!(!result.status.success());
}

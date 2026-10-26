use super::*;
use sha2::{Digest, Sha256};

fn task(id: Uuid, attempt: Uuid, metadata: Value, state: &str) -> Value {
    json!({"id":id,"projectId":Uuid::new_v4(),"attemptId":attempt,"runtime":null,"request":{"kind":"chat","input":"fixture","selection":null,"retryOf":null},"selection":{"connectionId":Uuid::new_v4(),"model":"fixture"},"state":state,"createdAt":1,"updatedAt":2,"result":"retained","errorCode":null,"error":null,"events":[],"changes":[],"artifacts":metadata})
}

fn reads(
    replies: Vec<(String, Vec<u8>)>,
    stall_last: bool,
) -> (ApiClient, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = ApiClient::new(format!("http://{}", listener.local_addr().unwrap()));
    let worker = std::thread::spawn(move || {
        for (path, bytes) in replies {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, format!("GET {path} HTTP/1.1\r\n"));
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            )
            .unwrap();
            stream.write_all(&bytes).unwrap();
        }
        if stall_last {
            let (_stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(1100));
        }
    });
    (client, worker)
}

#[test]
fn artifact_export_is_binary_exact_atomic_and_never_overwrites_an_existing_file() {
    let id = Uuid::new_v4();
    let attempt = Uuid::new_v4();
    let artifact = Uuid::new_v4();
    let bytes = b"owned\0binary\n";
    let metadata = json!({"id":artifact,"taskId":id,"attemptId":attempt,"path":"result.bin","size":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes))});
    let row = task(id, attempt, json!([metadata]), "result");
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("result.bin");
    for exists in [false, true] {
        let (client, worker) = reads(
            vec![
                (
                    format!("/api/ai-sandbox/tasks/{id}"),
                    row.to_string().into_bytes(),
                ),
                (
                    format!("/api/ai-sandbox/tasks/{id}/artifacts/{artifact}"),
                    bytes.to_vec(),
                ),
            ],
            false,
        );
        let result = request(
            &client,
            Command::Artifact {
                id,
                artifact,
                output: output.clone(),
            },
            [].as_slice(),
        );
        if exists {
            assert!(result.is_err());
            drop(result);
        } else {
            assert_eq!(result.unwrap(), metadata);
        }
        worker.join().unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}

#[test]
fn artifact_export_rejects_foreign_attempt_oversize_and_corrupt_bytes_without_creating_a_file() {
    let id = Uuid::new_v4();
    let attempt = Uuid::new_v4();
    let artifact = Uuid::new_v4();
    let bytes = b"owned";
    for case in ["foreign", "oversize", "corrupt", "short"] {
        let mut metadata = json!({"id":artifact,"taskId":id,"attemptId":attempt,"path":"result.bin","size":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes))});
        if case == "foreign" {
            metadata["attemptId"] = json!(Uuid::new_v4());
        }
        if case == "oversize" {
            metadata["size"] = json!(16 * 1024 * 1024 + 1);
        }
        let row = task(id, attempt, json!([metadata]), "result");
        let mut replies = vec![(
            format!("/api/ai-sandbox/tasks/{id}"),
            row.to_string().into_bytes(),
        )];
        if matches!(case, "corrupt" | "short") {
            replies.push((
                format!("/api/ai-sandbox/tasks/{id}/artifacts/{artifact}"),
                if case == "short" {
                    vec![]
                } else {
                    vec![b'x'; bytes.len()]
                },
            ));
        }
        let (client, worker) = reads(replies, false);
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("result.bin");
        let error = request(
            &client,
            Command::Artifact {
                id,
                artifact,
                output: output.clone(),
            },
            [].as_slice(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains(match case {
                "foreign" => "does not belong",
                "oversize" => "bound",
                _ => "SHA-256",
            }),
            "{case}: {error}"
        );
        assert!(!output.exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        worker.join().unwrap();
    }
}

#[test]
fn task_wait_observes_progress_and_keeps_last_durable_state_when_the_next_poll_times_out() {
    let id = Uuid::new_v4();
    let attempt = Uuid::new_v4();
    let path = format!("/api/ai-sandbox/tasks/{id}");
    let initial = task(id, attempt, json!([]), "accepted");
    let mut final_task = initial.clone();
    final_task["state"] = json!("result");
    let (client, worker) = reads(
        vec![
            (path.clone(), initial.to_string().into_bytes()),
            (path.clone(), final_task.to_string().into_bytes()),
        ],
        false,
    );
    let result = request(
        &client,
        Command::Wait {
            id,
            timeout_seconds: 1,
        },
        [].as_slice(),
    )
    .unwrap();
    assert_eq!(result["reached"], true);
    assert_eq!(result["task"]["attemptId"], json!(attempt));
    worker.join().unwrap();
    let (client, worker) = reads(vec![(path, initial.to_string().into_bytes())], true);
    let start = Instant::now();
    let result = wait_for_task(&client, id, 1).unwrap();
    assert!(!result.reached);
    assert_eq!(result.task.state, SandboxTaskState::Accepted);
    assert_eq!(result.task.attempt_id, attempt);
    assert!(start.elapsed() < Duration::from_secs(2));
    worker.join().unwrap();
    for seconds in [0, 61] {
        assert!(
            wait_for_task(&client, id, seconds)
                .unwrap_err()
                .to_string()
                .contains("between 1 and 60")
        );
    }
}

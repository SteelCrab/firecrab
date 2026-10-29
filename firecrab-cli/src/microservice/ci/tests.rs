use super::*;
use serde_json::json;
use std::{
    io::Write,
    net::TcpListener,
    sync::{Arc, Mutex},
};

fn outcome(operation: Uuid, release: Uuid) -> CiDeployStatus {
    serde_json::from_value(json!({"serviceId":Uuid::new_v4(),"operationId":operation,"expectedReleaseId":release,
        "image":format!("ghcr.io/team/app@sha256:{}","b".repeat(64)),"strategy":"rolling","phase":"running",
        "completed":true,"succeeded":true,"digest":format!("sha256:{}","c".repeat(64))})).unwrap()
}

fn connection(
    responses: Vec<(u16, String)>,
) -> (
    Connection,
    Arc<Mutex<Vec<String>>>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let task = std::thread::spawn(move || {
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut data = Vec::new();
            let mut buf = [0; 4096];
            loop {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&buf[..n]);
                if let Some(offset) = data.windows(4).position(|v| v == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&data[..offset]);
                    let len = headers
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|n| n.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if data.len() >= offset + 4 + len {
                        break;
                    }
                }
            }
            captured
                .lock()
                .unwrap()
                .push(String::from_utf8(data).unwrap());
            if status == 0 {
                continue;
            }
            let location = if status == 302 {
                "Location: https://other.invalid/__firecrab/deploy\r\n"
            } else {
                ""
            };
            write!(stream,"HTTP/1.1 {status} response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{location}\r\n{body}",body.len()).unwrap();
        }
    });
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        HeaderValue::from_static("Bearer fixture-only"),
    );
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .default_headers(headers)
        .build()
        .unwrap();
    (
        Connection {
            client,
            url: format!("http://{address}/__firecrab/deploy"),
            deadline: Instant::now() + Duration::from_secs(10),
        },
        requests,
        task,
    )
}

#[test]
fn submission_retains_expected_identity_and_failed_attempt_does_not_follow_active_release() {
    let operation = Uuid::new_v4();
    let release = Uuid::new_v4();
    let mut failed = outcome(operation, release);
    failed.phase = firecrab_api_types::ServicePhase::Failed;
    failed.succeeded = false;
    let (client,requests,task)=connection(vec![(404,"{}".into()),(200,json!({"serviceId":failed.service_id,"releaseId":release,"image":"ghcr.io/team/app:v1","phase":"running"}).to_string()),(202,serde_json::to_string(&failed).unwrap())]);
    assert!(matches!(
        wait(
            &client,
            &failed.image,
            operation,
            None,
            DeployStrategy::Rolling
        ),
        Err(Error::Failed)
    ));
    task.join().unwrap();
    let writes = requests.lock().unwrap();
    assert!(writes[2].starts_with("POST /__firecrab/deploy "));
    let body: serde_json::Value =
        serde_json::from_str(writes[2].split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["expectedReleaseId"], release.to_string());
    assert_eq!(body.as_object().unwrap().len(), 4);
}

#[test]
fn existing_success_resumes_without_submitting_again_and_rejects_changed_inputs() {
    let operation = Uuid::new_v4();
    let release = Uuid::new_v4();
    let status = outcome(operation, release);
    let (client, requests, task) = connection(vec![(200, serde_json::to_string(&status).unwrap())]);
    assert!(
        wait(
            &client,
            &status.image,
            operation,
            Some(release),
            DeployStrategy::Rolling
        )
        .is_ok()
    );
    task.join().unwrap();
    assert_eq!(requests.lock().unwrap().len(), 1);
    let (client, _, task) = connection(vec![(200, serde_json::to_string(&status).unwrap())]);
    assert!(matches!(
        wait(
            &client,
            &status.image,
            operation,
            Some(Uuid::new_v4()),
            DeployStrategy::Rolling
        ),
        Err(Error::Invalid(_))
    ));
    task.join().unwrap();
}

#[test]
fn lost_acknowledgement_reads_the_exact_operation_instead_of_creating_another() {
    let operation = Uuid::new_v4();
    let release = Uuid::new_v4();
    let status = outcome(operation, release);
    let (client, requests, task) = connection(vec![
        (404, "{}".into()),
        (0, String::new()),
        (200, serde_json::to_string(&status).unwrap()),
    ]);
    assert!(
        wait(
            &client,
            &status.image,
            operation,
            Some(release),
            DeployStrategy::Rolling
        )
        .is_ok()
    );
    task.join().unwrap();
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.starts_with("POST "))
            .count(),
        1
    );
}

#[test]
fn redirects_and_response_bodies_do_not_expose_credentials_or_upstream_errors() {
    let operation = Uuid::new_v4();
    let status = outcome(operation, Uuid::new_v4());
    let (client, _, task) = connection(vec![(302, "private upstream credentials".into())]);
    let error = wait(
        &client,
        &status.image,
        operation,
        None,
        DeployStrategy::Rolling,
    )
    .unwrap_err();
    task.join().unwrap();
    assert_eq!(error.to_string(), "Deploy endpoint returned HTTP 302");
}

#[test]
fn a_waiting_operation_exits_unsuccessfully_at_the_total_deadline() {
    let operation = Uuid::new_v4();
    let mut status = outcome(operation, Uuid::new_v4());
    status.phase = firecrab_api_types::ServicePhase::Queued;
    status.completed = false;
    status.succeeded = false;
    let (mut client, _, task) = connection(vec![(200, serde_json::to_string(&status).unwrap())]);
    client.deadline = Instant::now() + Duration::from_secs(1);
    assert!(matches!(
        wait(
            &client,
            &status.image,
            operation,
            None,
            DeployStrategy::Rolling
        ),
        Err(Error::Timeout)
    ));
    task.join().unwrap();
}

#[test]
fn endpoints_reject_plaintext_remote_hosts_credentials_queries_and_operator_paths() {
    for url in [
        "http://example.test/__firecrab/deploy",
        "https://user:secret@example.test/__firecrab/deploy",
        "https://example.test/api/vms",
        "https://example.test/__firecrab/deploy?token=secret",
        "https://example.test/__firecrab/deploy#token",
        "https://example.test/__firecrab/deploy/../../api/vms",
    ] {
        assert!(endpoint(url).is_err());
    }
    assert!(endpoint("https://app.example.test/__firecrab/deploy").is_ok());
    assert!(endpoint("http://[::1]:5525/__firecrab/deploy").is_ok());
    assert!(!pinned("ghcr.io/team/app:latest"));
}

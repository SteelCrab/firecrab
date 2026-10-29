use super::*;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
#[path = "ai_sandbox_artifact_tests.rs"]
mod artifact_limits;
#[path = "ai_sandbox_resources_tests.rs"]
mod resources;

fn reply(
    method: &str,
    path: String,
    body: Value,
    status: u16,
    response: Value,
) -> (ApiClient, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = ApiClient::new(format!("http://{}", listener.local_addr().unwrap()));
    let expected = format!("{method} {path} HTTP/1.1\r\n");
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, expected);
        let mut length = 0;
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some((key, value)) = line.split_once(':')
                && key.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse().unwrap();
            }
        }
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).unwrap();
        let actual = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        assert_eq!(actual, body);
        let response = response.to_string();
        write!(stream, "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
    });
    (client, worker)
}

#[test]
fn sandbox_access_cli_uses_the_same_typed_operator_rest_policy() {
    let principal = Uuid::new_v4();
    let policy = json!({"grants":[{"resource":{"kind":"project","id":Uuid::new_v4()},"permissions":["read","execute"]}]});
    for (command, method, body, response, input) in [
        (
            Command::GetAccess { principal },
            "GET",
            Value::Null,
            policy.clone(),
            vec![],
        ),
        (
            Command::SetAccess { principal },
            "PUT",
            policy.clone(),
            policy.clone(),
            policy.to_string().into_bytes(),
        ),
        (
            Command::RevokeAccess { principal },
            "DELETE",
            Value::Null,
            json!({"grants":[]}),
            vec![],
        ),
    ] {
        let (client, server) = reply(
            method,
            format!("/api/ai-sandbox/access/{principal}"),
            body,
            200,
            response.clone(),
        );
        assert_eq!(
            request(&client, command, input.as_slice()).unwrap(),
            response
        );
        server.join().unwrap();
    }
    let client = ApiClient::new("http://127.0.0.1:1".into());
    assert!(
        request(
            &client,
            Command::SetAccess { principal },
            br#"{"grants":[],"operator":true}"#.as_slice()
        )
        .is_err()
    );
}

#[test]
fn sandbox_cli_preserves_ids_overrides_and_explicit_review_paths_on_the_wire() {
    let id = Uuid::new_v4();
    let project = Uuid::new_v4();
    let connection = Uuid::new_v4();
    let selection = json!({"connectionId":connection,"model":"configured-model"});
    let spec = json!({"name":"Project","sourcePath":"/project","vmId":null,"selection":selection,"instructions":"Keep changes small","environmentId":null,"context":{"gitRef":"main","selectedSkills":["skills/review/SKILL.md"],"references":["README.md"]}});
    let project_row = json!({"id":project,"owner":"local","spec":spec,"createdAt":1});
    let input = json!({"kind":"code","input":"Check the build","selection":selection,"retryOf":Uuid::new_v4(),"coordination":{"dependencies":[],"assignedAgent":"debug","retryLimit":2},"environmentRole":null});
    let task = json!({"id":id,"projectId":project,"attemptId":Uuid::new_v4(),"runtime":null,"request":input,"selection":selection,"state":"interrupted","createdAt":1,"updatedAt":2,"result":"","errorCode":"sandbox_restart","error":"Reconcile first","events":[],"changes":[],"gitCommit":null,"retryRoot":null,"retryNumber":0,"dependencyResults":[],"tests":[],"artifacts":[],"environment":null});
    let summary = serde_json::to_value(SandboxTaskSummary::from(
        &serde_json::from_value::<SandboxTask>(task.clone()).unwrap(),
    ))
    .unwrap();
    let model = json!({"id":"configured-model","name":"Configured model","verified":true,"toolsVerified":true});
    let catalog = json!({"models":[model],"discoveryAvailable":false});
    let base = "/api/ai-sandbox";
    let cases = vec![
        (
            Command::Projects,
            "GET",
            format!("{base}/projects"),
            Value::Null,
            json!([project_row]),
            Vec::new(),
        ),
        (
            Command::SaveProject { id: project },
            "PUT",
            format!("{base}/projects/{project}"),
            spec.clone(),
            project_row,
            spec.to_string().into_bytes(),
        ),
        (
            Command::Tasks { project },
            "GET",
            format!("{base}/projects/{project}/tasks"),
            Value::Null,
            json!([summary]),
            Vec::new(),
        ),
        (
            Command::Run { project, id },
            "PUT",
            format!("{base}/projects/{project}/tasks/{id}"),
            input.clone(),
            task.clone(),
            input.to_string().into_bytes(),
        ),
        (
            Command::Inspect { id },
            "GET",
            format!("{base}/tasks/{id}"),
            Value::Null,
            task.clone(),
            Vec::new(),
        ),
        (
            Command::Cancel { id },
            "POST",
            format!("{base}/tasks/{id}/cancel"),
            json!({}),
            task.clone(),
            Vec::new(),
        ),
        (
            Command::Reconcile { id },
            "POST",
            format!("{base}/tasks/{id}/reconcile"),
            json!({}),
            task.clone(),
            Vec::new(),
        ),
        (
            Command::Apply {
                id,
                paths: vec!["src/main.rs".into()],
            },
            "POST",
            format!("{base}/tasks/{id}/apply"),
            json!({"paths":["src/main.rs"]}),
            task,
            Vec::new(),
        ),
        (
            Command::Models {
                connection,
                discover: true,
            },
            "GET",
            format!("{base}/connections/{connection}/models?discover=true"),
            Value::Null,
            catalog,
            Vec::new(),
        ),
        (
            Command::Verify {
                connection,
                model: "configured-model".into(),
                tools: true,
            },
            "POST",
            format!("{base}/connections/{connection}/verify"),
            json!({"model":"configured-model","tools":true}),
            model,
            Vec::new(),
        ),
    ];
    for (command, method, path, body, response, input) in cases {
        let (client, server) = reply(method, path, body, 200, response.clone());
        assert_eq!(
            request(&client, command, input.as_slice()).unwrap(),
            response
        );
        server.join().unwrap();
    }
}

#[test]
fn sandbox_cli_bounds_stdin_and_rejects_credentials_before_contacting_the_server() {
    let client = ApiClient::new("http://127.0.0.1:1".into());
    let oversized = vec![b' '; 65537];
    assert_eq!(
        read_input::<SandboxTaskInput>(oversized.as_slice())
            .unwrap_err()
            .to_string(),
        "Sandbox input exceeds 64 KiB"
    );
    let secret = br#"{"kind":"chat","input":"hello","apiKey":"must-not-be-sent"}"#;
    let error = request(
        &client,
        Command::Run {
            project: Uuid::new_v4(),
            id: Uuid::new_v4(),
        },
        secret.as_slice(),
    )
    .unwrap_err()
    .to_string();
    assert_eq!(error, "Invalid Sandbox request JSON");
    assert!(!error.contains("must-not-be-sent"));
    let mut boundary = br#"{"kind":"chat","input":"hello"}"#.to_vec();
    boundary.resize(65536, b' ');
    assert_eq!(
        read_input::<SandboxTaskInput>(boundary.as_slice())
            .unwrap()
            .input,
        "hello"
    );
    assert!(read_input::<SandboxProjectSpec>(&b"{broken"[..]).is_err());
}

#[test]
fn sandbox_cli_reports_api_rejection_without_retrying_admission() {
    let id = Uuid::new_v4();
    let input = json!({"kind":"chat","input":"hello","selection":null,"retryOf":null,"coordination":{"dependencies":[],"assignedAgent":null,"retryLimit":3},"environmentRole":null});
    let (client, server) = reply(
        "PUT",
        format!("/api/ai-sandbox/projects/{id}/tasks/{id}"),
        input.clone(),
        409,
        json!({"error":"sandbox_model_not_verified"}),
    );
    let error = request(
        &client,
        Command::Run { project: id, id },
        input.to_string().as_bytes(),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("409"));
    assert!(error.contains("sandbox_model_not_verified"));
    server.join().unwrap();
}

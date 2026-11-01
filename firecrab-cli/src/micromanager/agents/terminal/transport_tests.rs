use super::*;
use axum::{Router, routing::get};
use futures_util::StreamExt;
use tokio_tungstenite::{connect_async, tungstenite::Message as ClientMessage};

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
struct Fixture {
    root: tempfile::TempDir,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn connect(mode: &'static str) -> (Fixture, Client) {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("terminal.py");
    fs::write(
        &script,
        r#"import sys,json,pathlib,time
root=pathlib.Path(__file__).parent
mode=sys.argv[1]
frame=json.dumps({'type':'terminal.frame','encoding':'ansi','bytes':'cmVhZHk='})
if mode=='invalid':print('not-json',flush=True)
elif mode=='unsupported':print(json.dumps({'type':'host.exec'}),flush=True)
elif mode=='closed':print(json.dumps({'type':'terminal.closed'}),flush=True)
elif mode=='busy':print('FIRECRAB_TERMINAL_BUSY',file=sys.stderr,flush=True)
elif mode=='fragmented':
 sys.stdout.write(frame[:20]);sys.stdout.flush();time.sleep(.1)
 sys.stdout.write(frame[20:]+'\n');sys.stdout.flush()
else:print(frame,flush=True)
for line in sys.stdin:
 value=json.loads(line)
 with (root/'input.jsonl').open('a') as f:f.write(json.dumps(value)+'\n')
 if value['type']=='terminal.release':
  (root/'released').write_text('detached');break
"#,
    )
    .unwrap();
    let app = Router::new().route(
        "/terminal",
        get(move |ws: axum::extract::WebSocketUpgrade| {
            let mut command = if mode == "spawn-failure" {
                Command::new("/nonexistent/firecrab-test-terminal")
            } else {
                Command::new("python3")
            };
            command
                .arg("-u")
                .arg(&script)
                .arg(mode)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            async move { ws.on_upgrade(move |socket| bridge(socket, command)) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (client, _) = connect_async(format!("ws://127.0.0.1:{port}/terminal"))
        .await
        .unwrap();
    (Fixture { root, server }, client)
}
async fn frame(client: &mut Client) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(3), client.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let ClientMessage::Text(raw) = message {
            return serde_json::from_str(&raw).unwrap();
        }
    }
}
async fn released(fixture: &Fixture) {
    for _ in 0..100 {
        if fixture.root.path().join("released").is_file() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("The native bridge did not release its terminal controller");
}

#[tokio::test]
async fn fragmented_output_survives_input_resize_ping_and_clean_detach() {
    let (fixture, mut client) = connect("fragmented").await;
    client
        .send(ClientMessage::Ping(vec![1, 2].into()))
        .await
        .unwrap();
    let bytes = STANDARD.encode("한글\u{1b}[A\r\u{3}");
    let inputs = [
        json!({"type":"terminal.input","bytes":bytes}),
        json!({"type":"terminal.resize","cols":100,"rows":30}),
        json!({"type":"terminal.scroll","direction":"up","lines":2,"column":3,"row":4,"modifiers":0}),
        json!({"type":"terminal.mouse","action":"down","button":"left","column":3,"row":4,"modifiers":1}),
    ];
    for input in &inputs {
        client
            .send(ClientMessage::Text(input.to_string().into()))
            .await
            .unwrap();
    }
    let value = frame(&mut client).await;
    assert_eq!(value["type"], "terminal.frame");
    assert_eq!(
        STANDARD.decode(value["bytes"].as_str().unwrap()).unwrap(),
        b"ready"
    );
    client.close(None).await.unwrap();
    released(&fixture).await;
    let records = fs::read_to_string(fixture.root.path().join("input.jsonl"))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str::<Value>(s).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(&records[..inputs.len()], &inputs);
    assert_eq!(records.last().unwrap()["type"], "terminal.release");
}

#[tokio::test]
async fn bad_streams_busy_workspaces_and_spawn_failures_report_errors() {
    for (mode, expected) in [
        ("invalid", "Invalid terminal frame"),
        ("unsupported", "Unsupported terminal stream"),
        ("busy", "Verification is running"),
        ("spawn-failure", "No such file"),
    ] {
        let (fixture, mut client) = connect(mode).await;
        let value = frame(&mut client).await;
        assert_eq!(value["type"], "terminal.error");
        assert!(
            value["message"].as_str().unwrap().contains(expected),
            "{mode}: {value}"
        );
        if mode != "spawn-failure" {
            released(&fixture).await;
        }
    }
}

#[tokio::test]
async fn invalid_input_is_rejected_before_it_reaches_the_terminal() {
    let (fixture, mut client) = connect("ready").await;
    assert_eq!(frame(&mut client).await["type"], "terminal.frame");
    client
        .send(ClientMessage::Text(
            json!({"type":"terminal.input","bytes":"YQ==","cwd":"/root"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    assert_eq!(frame(&mut client).await["type"], "terminal.error");
    released(&fixture).await;
    let records = fs::read_to_string(fixture.root.path().join("input.jsonl")).unwrap();
    assert_eq!(records.lines().count(), 1);
    assert_eq!(
        serde_json::from_str::<Value>(&records).unwrap()["type"],
        "terminal.release"
    );
    let (fixture, mut client) = connect("closed").await;
    assert_eq!(frame(&mut client).await["type"], "terminal.closed");
    released(&fixture).await;
}

#[test]
fn a_terminal_cannot_attach_to_closed_foreign_or_busy_vm_workspaces() {
    use super::super::tests::{shell_replies, workspace_fixture};
    let (root, runtime, mut agent) = workspace_fixture();
    let attach = Attach {
        controller: "test-controller".into(),
        cols: 100,
        rows: 30,
    };
    runtime.terminal_command(agent.id, &attach).unwrap();
    shell_replies(
        root.path(),
        json!({"then printf running":{"out":"running"}}),
    );
    assert!(
        runtime
            .terminal_command(agent.id, &attach)
            .unwrap_err()
            .to_string()
            .contains("Verification is running")
    );
    agent.closed = true;
    runtime.save(std::slice::from_ref(&agent)).unwrap();
    assert!(
        runtime
            .terminal_command(agent.id, &attach)
            .unwrap_err()
            .to_string()
            .contains("closed")
    );
    assert!(runtime.terminal_command(Uuid::new_v4(), &attach).is_err());
}

use super::*;
use std::{
    io::{BufReader, Read},
    net::TcpListener,
    thread::JoinHandle,
};

fn mock(replies: Vec<(&'static str, u16, Value)>) -> (ApiClient, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for (expected, status, body) in replies {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.starts_with(expected), "{line} expected {expected}");
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
            if !bytes.is_empty() {
                assert!(!String::from_utf8_lossy(&bytes).contains("apiKey"));
            }
            // Zero models a create whose response was lost after it committed.
            if status == 0 {
                continue;
            }
            let text = if status == 204 {
                String::new()
            } else {
                body.to_string()
            };
            write!(stream,"HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",text.len()).unwrap();
        }
    });
    (ApiClient::new(base), server)
}

fn network(id: Uuid, operation: Uuid) -> Value {
    json!({"id":id,"name":format!("test-{}",operation.simple()),"subnetCidr":"172.31.0.0/24","gateway":"172.31.0.1","internetEnabled":true,
        "uplink":null,"ipv6Cidr":null,"ipv6Gateway":null,"ipv6AddressMode":null,"ipv6Egress":null})
}

#[test]
fn stdio_handshake_schema_errors_and_notifications_preserve_json_rpc() {
    let client = ApiClient::new("http://127.0.0.1:1".into());
    let input=[
        "{broken".to_owned(),
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":PROTOCOL,"capabilities":{},"clientInfo":{"name":"test","version":"1"}}}).to_string(),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string(),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}).to_string(),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"unknown","arguments":{}}}).to_string(),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"network_delete","arguments":{"id":Uuid::new_v4()}}}).to_string(),
    ].join("\n")+"\n";
    let mut output = Vec::new();
    serve(&client, input.as_bytes(), &mut output).unwrap();
    let rows: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0]["error"]["code"], -32700);
    assert_eq!(rows[1]["result"]["protocolVersion"], PROTOCOL);
    assert!(
        rows[2]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "vm_create")
    );
    assert_eq!(rows[3]["error"]["code"], -32602);
    assert_eq!(rows[4]["result"]["isError"], true);
    let tools = rows[2]["result"]["tools"].as_array().unwrap();
    for tool in tools {
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert!(tool["inputSchema"].to_string().find("apiKey").is_none());
    }
    let mut session = Session::default();
    let invalid = session
        .message(
            &client,
            json!({"jsonrpc":"2.0","id":9,"method":"tools/list"}),
        )
        .unwrap();
    assert_eq!(invalid["error"]["code"], -32000);
}

#[test]
fn only_confirmed_session_resources_can_be_deleted_and_retries_do_not_create_duplicates() {
    let id = Uuid::new_v4();
    let operation = Uuid::new_v4();
    let row = network(id, operation);
    let (client, server) = mock(vec![
        ("GET /api/micro-networks ", 200, json!([])),
        ("POST /api/micro-networks ", 201, row.clone()),
        ("GET /api/micro-networks ", 200, json!([row.clone()])),
        ("DELETE /api/micro-networks/", 204, Value::Null),
    ]);
    let mut session = Session::default();
    assert!(
        session
            .call(&client, "network_delete", json!({"id":id}))
            .is_err()
    );
    let args = json!({"operationId":operation,"name":"test","subnetCidr":"172.31.0.0/24"});
    assert_eq!(
        session
            .call(&client, "network_create", args.clone())
            .unwrap()["id"],
        id.to_string()
    );
    assert_eq!(
        session.call(&client, "network_create", args).unwrap()["id"],
        id.to_string()
    );
    assert_eq!(session.owned.len(), 1);
    assert_eq!(
        session
            .call(&client, "network_delete", json!({"id":id}))
            .unwrap()["deleted"],
        id.to_string()
    );
    assert!(session.owned.is_empty());
    server.join().unwrap();
}

#[test]
fn uncertain_creation_is_inspected_and_never_grants_deletion_of_a_borrowed_candidate() {
    let id = Uuid::new_v4();
    let operation = Uuid::new_v4();
    let row = network(id, operation);
    let (client, server) = mock(vec![
        ("GET /api/micro-networks ", 200, json!([])),
        ("POST /api/micro-networks ", 0, Value::Null),
        ("GET /api/micro-networks ", 200, json!([row.clone()])),
        ("GET /api/micro-networks ", 200, json!([row])),
    ]);
    let mut session = Session::default();
    let args = json!({"operationId":operation,"name":"test","subnetCidr":"172.31.0.0/24"});
    for _ in 0..2 {
        let result = session
            .call(&client, "network_create", args.clone())
            .unwrap();
        assert_eq!(result["outcome"], "uncertain");
        assert_eq!(result["owned"], false);
    }
    assert!(session.owned.is_empty());
    assert!(
        session
            .call(&client, "network_delete", json!({"id":id}))
            .is_err()
    );
    server.join().unwrap();
}

#[test]
fn stdio_bounds_input_and_omits_guest_environment_from_model_context() {
    let client = ApiClient::new("http://127.0.0.1:1".into());
    assert!(serve(&client, vec![b'x'; MAX_MESSAGE + 1].as_slice(), Vec::new()).is_err());
    let mut value = json!({"vm":{"env":{"SECRET":"private"},"id":"visible"},"items":[{"env":{"KEY":"private"}}]});
    redact_guest_env(&mut value);
    assert!(!value.to_string().contains("private"));
    assert_eq!(value["vm"]["id"], "visible");
    let mut session = Session::default();
    assert!(
        session
            .call(
                &client,
                "vm_wait",
                json!({"id":Uuid::new_v4(),"state":"running","timeoutSeconds":61})
            )
            .is_err()
    );
    assert!(
        session
            .call(&client, "vm_reassign_network", json!({}))
            .unwrap_err()
            .contains("Unsupported")
    );
}

use super::*;
#[path = "sandbox.rs"]
mod sandbox_resources;
use std::{
    io::{BufReader, Read},
    net::TcpListener,
    thread::JoinHandle,
};

#[test]
fn sandbox_resource_creation_defaults_to_no_internet_and_preserves_explicit_policy() {
    let operation = Uuid::new_v4();
    let network = json!({"operationId":operation,"name":"test","subnetCidr":"172.31.0.0/24"});
    assert!(
        !parse::<NetworkCreate>(network.clone())
            .unwrap()
            .internet_enabled
    );
    let mut explicit = network;
    explicit["internetEnabled"] = json!(true);
    assert!(parse::<NetworkCreate>(explicit).unwrap().internet_enabled);
    let mut vm = json!({"operationId":operation,"name":"test","template":"ubuntu","microNetworkId":Uuid::new_v4(),"cpu":1,"ram":512,"diskGb":2});
    assert_eq!(
        parse::<VmCreate>(vm.clone()).unwrap().egress_policy,
        EgressPolicy::Isolated
    );
    vm["egressPolicy"] = json!("internet");
    assert_eq!(
        parse::<VmCreate>(vm.clone()).unwrap().egress_policy,
        EgressPolicy::Internet
    );
    vm["egressPolicy"] = json!("invalid");
    assert!(parse::<VmCreate>(vm).is_err());
}

#[test]
fn sandbox_access_mcp_exposes_operator_only_grants_without_self_elevation() {
    let id = Uuid::new_v4();
    let policy = json!({"grants":[{"resource":{"kind":"vm","id":Uuid::new_v4()},"permissions":["execute"]}]});
    let (client, server) = mock(vec![
        ("GET /api/ai-sandbox/access/", 200, policy.clone()),
        ("PUT /api/ai-sandbox/access/", 200, policy.clone()),
        ("DELETE /api/ai-sandbox/access/", 200, json!({"grants":[]})),
    ]);
    let mut session = Session::default();
    assert_eq!(
        session
            .call(&client, "sandbox_access_get", json!({"id":id}))
            .unwrap(),
        policy
    );
    assert_eq!(
        session
            .call(
                &client,
                "sandbox_access_set",
                json!({"id":id,"policy":policy})
            )
            .unwrap(),
        policy
    );
    assert_eq!(
        session
            .call(&client, "sandbox_access_revoke", json!({"id":id}))
            .unwrap(),
        json!({"grants":[]})
    );
    server.join().unwrap();
    assert!(
        session
            .call(
                &client,
                "sandbox_access_set",
                json!({"id":id,"policy":{"grants":[]},"operator":true})
            )
            .is_err()
    );
    assert!(session.owned.is_empty());
    let tool = catalog()
        .into_iter()
        .find(|t| t["name"] == "sandbox_access_set")
        .unwrap();
    assert_eq!(tool["annotations"]["destructiveHint"], true);
}

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
        (
            "GET /api/micro-networks/",
            404,
            json!({"error":"not_found"}),
        ),
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

#[test]
fn sandbox_mcp_routes_durable_tasks_through_rest_and_requires_explicit_review_paths() {
    let id = Uuid::new_v4();
    let project = Uuid::new_v4();
    let connection = Uuid::new_v4();
    let task = json!({"id":id,"projectId":project,"attemptId":Uuid::new_v4(),"runtime":null,"request":{"kind":"chat","input":"hello","selection":null,"retryOf":null},"selection":{"connectionId":connection,"model":"fixture-model"},"state":"result","createdAt":1,"updatedAt":1,"result":"done","errorCode":null,"error":null,"events":[],"changes":[]});
    let (client, server) = mock(vec![
        ("PUT /api/ai-sandbox/projects/", 202, task.clone()),
        ("GET /api/ai-sandbox/tasks/", 200, task.clone()),
        ("POST /api/ai-sandbox/tasks/", 200, task.clone()),
    ]);
    let mut session = Session::default();
    assert_eq!(
        session
            .call(
                &client,
                "sandbox_task_start",
                json!({"id":id,"projectId":project,"request":{"kind":"chat","input":"hello"}})
            )
            .unwrap()["id"],
        id.to_string()
    );
    assert_eq!(
        session
            .call(&client, "sandbox_task_get", json!({"id":id}))
            .unwrap()["state"],
        "result"
    );
    assert_eq!(
        session
            .call(
                &client,
                "sandbox_task_apply",
                json!({"id":id,"paths":["src/main.rs"]})
            )
            .unwrap()["id"],
        id.to_string()
    );
    assert!(
        session
            .call(&client, "sandbox_task_apply", json!({"id":id}))
            .is_err()
    );
    server.join().unwrap();
    let tools = catalog();
    let start = tools
        .iter()
        .find(|v| v["name"] == "sandbox_task_start")
        .unwrap();
    assert_eq!(start["annotations"]["destructiveHint"], true);
}

#[test]
fn sandbox_mcp_lists_saves_and_recovers_without_creating_a_new_attempt() {
    let id = Uuid::new_v4();
    let project = Uuid::new_v4();
    let connection = Uuid::new_v4();
    let selection = json!({"connectionId":connection,"model":"configured-model"});
    let spec = json!({"name":"Project","sourcePath":null,"vmId":null,"selection":selection,"instructions":"Be concise","context":{"gitRef":null,"selectedSkills":[],"references":[]},"environmentId":null});
    let project_row = json!({"id":project,"owner":"local","spec":spec,"createdAt":1});
    let model =
        json!({"id":"configured-model","name":"Configured","verified":true,"toolsVerified":true});
    let task = json!({"id":id,"projectId":project,"attemptId":Uuid::new_v4(),"runtime":null,"request":{"kind":"chat","input":"hello","selection":null,"retryOf":null},"selection":selection,"state":"interrupted","createdAt":1,"updatedAt":2,"result":"","errorCode":"sandbox_restart","error":"Reconcile first","events":[],"changes":[]});
    let mut summary = task.clone();
    summary.as_object_mut().unwrap().remove("events");
    summary.as_object_mut().unwrap().remove("changes");
    summary["eventCount"] = json!(0);
    summary["changeCount"] = json!(0);
    summary["lastTool"] = Value::Null;
    let (client, server) = mock(vec![
        ("GET /api/ai-sandbox/projects ", 200, json!([project_row])),
        ("PUT /api/ai-sandbox/projects/", 200, project_row.clone()),
        (
            "GET /api/ai-sandbox/connections/",
            200,
            json!({"models":[model],"discoveryAvailable":false}),
        ),
        ("POST /api/ai-sandbox/connections/", 200, model.clone()),
        ("GET /api/ai-sandbox/projects/", 200, json!([summary])),
        ("POST /api/ai-sandbox/tasks/", 200, task.clone()),
        ("POST /api/ai-sandbox/tasks/", 200, task.clone()),
    ]);
    let mut session = Session::default();
    assert_eq!(
        session
            .call(&client, "sandbox_projects_list", json!({}))
            .unwrap()[0]["id"],
        project.to_string()
    );
    assert_eq!(
        session
            .call(
                &client,
                "sandbox_project_save",
                json!({"id":project,"spec":spec})
            )
            .unwrap(),
        project_row
    );
    assert_eq!(
        session
            .call(&client, "sandbox_models_list", json!({"id":connection}))
            .unwrap()["discoveryAvailable"],
        false
    );
    assert_eq!(
        session
            .call(
                &client,
                "sandbox_model_verify",
                json!({"id":connection,"model":"configured-model","tools":true})
            )
            .unwrap(),
        model
    );
    assert_eq!(
        session
            .call(&client, "sandbox_tasks_list", json!({"projectId":project}))
            .unwrap()[0]["attemptId"],
        task["attemptId"]
    );
    for operation in ["sandbox_task_cancel", "sandbox_task_reconcile"] {
        let result = session.call(&client, operation, json!({"id":id})).unwrap();
        assert_eq!(result["attemptId"], task["attemptId"]);
        assert_eq!(result["state"], "interrupted");
    }
    server.join().unwrap();
    assert!(session.owned.is_empty());
}

#[test]
fn sandbox_mcp_rejects_credentials_and_unknown_fields_before_any_http_request() {
    let client = ApiClient::new("http://127.0.0.1:1".into());
    let id = Uuid::new_v4();
    let mut session = Session::default();
    for (operation, args) in [
        ("sandbox_projects_list", json!({"apiKey":"private"})),
        (
            "sandbox_project_save",
            json!({"id":id,"spec":{"name":"Project","apiKey":"private"}}),
        ),
        ("sandbox_models_list", json!({"id":id,"discover":"true"})),
        (
            "sandbox_model_verify",
            json!({"id":id,"model":"model","tools":true,"token":"private"}),
        ),
        ("sandbox_tasks_list", json!({"projectId":"invalid"})),
        (
            "sandbox_task_start",
            json!({"id":id,"projectId":id,"request":{"kind":"chat","input":"hello","env":{"KEY":"private"}}}),
        ),
        ("sandbox_task_cancel", json!({"id":id,"force":true})),
        ("sandbox_task_reconcile", json!({"id":id,"rerun":true})),
        (
            "sandbox_task_apply",
            json!({"id":id,"paths":["a"],"all":true}),
        ),
    ] {
        let error = session.call(&client, operation, args).unwrap_err();
        assert!(
            error.starts_with("Invalid tool arguments"),
            "{operation}: {error}"
        );
        assert!(!error.contains("private"));
    }
}

#[test]
fn sandbox_catalog_advertises_typed_results_and_rejects_unknown_resource_inputs() {
    let client = ApiClient::new("http://127.0.0.1:1".into());
    let mut session = Session::default();
    let id = Uuid::new_v4();
    for tool in catalog()
        .iter()
        .filter(|t| t["name"].as_str().unwrap().starts_with("sandbox_"))
    {
        assert!(
            tool["outputSchema"]["properties"].is_object(),
            "{} has no typed result",
            tool["name"]
        );
    }
    for (name, args) in [
        (
            "sandbox_environment_get",
            json!({"projectId":id,"id":id,"token":"private"}),
        ),
        (
            "sandbox_environment_delete",
            json!({"projectId":id,"id":id,"force":true}),
        ),
        (
            "sandbox_tunnel_save",
            json!({"projectId":id,"id":id,"spec":{"taskId":id,"subdomain":"preview","target":{"kind":"nginx","host":"app.example"},"token":"private"}}),
        ),
        (
            "sandbox_tunnel_verify",
            json!({"id":id,"request":{"path":"/","endpoint":"https://other.example"}}),
        ),
        (
            "sandbox_guest_signal",
            json!({"projectId":id,"id":id,"request":{"process":{"pid":42,"startTicks":1,"bootId":"boot","state":"S"},"signal":"terminate","shell":"private"}}),
        ),
    ] {
        assert!(
            session
                .call(&client, name, args)
                .unwrap_err()
                .starts_with("Invalid tool arguments"),
            "{name}"
        );
    }
}

#[test]
fn sandbox_project_cleanup_checks_final_rest_absence() {
    let id = Uuid::new_v4();
    let (client, server) = mock(vec![
        ("DELETE /api/ai-sandbox/projects/", 204, Value::Null),
        (
            "GET /api/ai-sandbox/projects/",
            404,
            json!({"error":"not found"}),
        ),
    ]);
    assert_eq!(
        Session::default()
            .call(&client, "sandbox_project_delete", json!({"id":id}))
            .unwrap(),
        json!({"id":id,"deleteAccepted":true,"confirmedAbsent":true})
    );
    server.join().unwrap();
}

#[test]
fn owned_cleanup_retains_ownership_until_final_rest_absence_is_confirmed() {
    let id = Uuid::new_v4();
    let operation = Uuid::new_v4();
    let row = network(id, operation);
    let (client, server) = mock(vec![
        ("DELETE /api/micro-networks/", 202, Value::Null),
        ("GET /api/micro-networks/", 200, row),
    ]);
    let mut session = Session::default();
    session.owned.insert(("/api/micro-networks".into(), id));
    let result = session
        .call(&client, "network_delete", json!({"id":id}))
        .unwrap();
    assert_eq!(result["confirmedAbsent"], false);
    assert!(result["deleted"].is_null());
    assert_eq!(result["owned"], true);
    assert!(session.owned.contains(&("/api/micro-networks".into(), id)));
    server.join().unwrap();
}

use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use firecrab_api_types::SandboxTask;
use std::io::{BufRead, Write};

struct Reply {
    path: String,
    body: Value,
    status: u16,
    response: Vec<u8>,
}
fn response(method: &str, path: String, body: Value, value: Value) -> Reply {
    Reply {
        path: format!("{method} {path} HTTP/1.1\r\n"),
        body,
        status: 200,
        response: value.to_string().into_bytes(),
    }
}
fn exchange(replies: Vec<Reply>) -> (ApiClient, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = ApiClient::new(format!("http://{}", listener.local_addr().unwrap()));
    let worker = std::thread::spawn(move || {
        for reply in replies {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, reply.path);
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
            assert_eq!(
                if bytes.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&bytes).unwrap()
                },
                reply.body
            );
            write!(
                stream,
                "HTTP/1.1 {} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                reply.status,
                reply.response.len()
            )
            .unwrap();
            // Bound readers can close before consuming an oversized fixture body.
            let _ = stream.write_all(&reply.response);
        }
    });
    (client, worker)
}
fn environment(project: Uuid, id: Uuid) -> Value {
    let spec = json!({"version":1,"subnetCidr":"172.31.0.0/24","roles":[{"name":"backend","template":{"name":"ubuntu","version":"sha256:pinned","kernelSha256":"1".repeat(64),"rootfsSha256":"2".repeat(64),"initrdSha256":null,"bootArgsSha256":"3".repeat(64)},"cpu":1,"memoryMib":256,"diskGb":1,"dependsOn":[],"services":[{"name":"app","port":8080,"readiness":true}],"executionMode":{"kind":"persistent_workspace"}}],"allow":[],"retention":{"ttlSeconds":null}});
    json!({"id":id,"projectId":project,"owner":"local","generation":Uuid::new_v4(),"spec":spec,"networkId":Uuid::new_v4(),"dnsMode":"private_only","roles":[{"role":"backend","vmId":Uuid::new_v4(),"securityGroupId":Uuid::new_v4(),"runtime":null,"ipv4":null,"agent":"pending","network":"pending","application":"pending","services":[],"error":null}],"state":"provisioning","createdAt":1,"updatedAt":2,"expiresAt":null,"exportedAt":null,"error":null})
}
fn tunnel(project: Uuid, id: Uuid) -> Value {
    let runtime = json!({"vmId":Uuid::new_v4(),"runtimeId":Uuid::new_v4()});
    let task = Uuid::new_v4();
    json!({"id":id,"projectId":project,"taskId":task,"attemptId":Uuid::new_v4(),"runtime":runtime,"owner":"local","spec":{"taskId":task,"subdomain":"saved-preview","target":{"kind":"vm_app","runtime":runtime,"port":8080}},"state":"needs_reconciliation","appReady":false,"connectorId":null,"appliedTarget":null,"publicUrl":"https://saved-preview.firecrab.dev","helperConnected":false,"publicHttps":"unverified","webSocketHandshake":"unverified","publicShutdown":"unverified","remoteRetirement":"operator_required","localRemoved":false,"createdAt":1,"updatedAt":2,"errorCode":"sandbox_preview_publish_uncertain"})
}
fn task(id: Uuid, attempt: Uuid, artifacts: Value, state: &str) -> Value {
    json!({"id":id,"projectId":Uuid::new_v4(),"attemptId":attempt,"runtime":null,"request":{"kind":"chat","input":"fixture","selection":null,"retryOf":null},"selection":{"connectionId":Uuid::new_v4(),"model":"fixture"},"state":state,"createdAt":1,"updatedAt":2,"result":"retained","errorCode":null,"error":null,"events":[],"changes":[],"artifacts":artifacts})
}

#[test]
fn environment_mcp_keeps_reserved_refs_pending_readiness_and_explicit_workspace_delete_consent() {
    let project = Uuid::new_v4();
    let id = Uuid::new_v4();
    let row = environment(project, id);
    let base = format!("/api/ai-sandbox/projects/{project}/environments/{id}");
    let exported = json!({"environment":{"id":id,"generation":row["generation"],"role":""},"taskIds":[],"exportedAt":3});
    let cases = vec![
        (
            "sandbox_environment_images",
            json!({"projectId":project}),
            "GET",
            format!("/api/ai-sandbox/projects/{project}/environment-images"),
            Value::Null,
            json!([]),
        ),
        (
            "sandbox_environments_list",
            json!({"projectId":project}),
            "GET",
            format!("/api/ai-sandbox/projects/{project}/environments"),
            Value::Null,
            json!([row]),
        ),
        (
            "sandbox_environment_save",
            json!({"projectId":project,"id":id,"spec":row["spec"]}),
            "PUT",
            base.clone(),
            row["spec"].clone(),
            row.clone(),
        ),
        (
            "sandbox_environment_get",
            json!({"projectId":project,"id":id}),
            "GET",
            base.clone(),
            Value::Null,
            row.clone(),
        ),
        (
            "sandbox_environment_reconcile",
            json!({"projectId":project,"id":id}),
            "POST",
            format!("{base}/reconcile"),
            json!({}),
            row.clone(),
        ),
        (
            "sandbox_environment_stop",
            json!({"projectId":project,"id":id}),
            "POST",
            format!("{base}/stop"),
            json!({}),
            row.clone(),
        ),
        (
            "sandbox_environment_resume",
            json!({"projectId":project,"id":id}),
            "POST",
            format!("{base}/resume"),
            json!({}),
            row.clone(),
        ),
        (
            "sandbox_environment_export",
            json!({"projectId":project,"id":id}),
            "POST",
            format!("{base}/export"),
            json!({}),
            exported,
        ),
        (
            "sandbox_environment_delete",
            json!({"projectId":project,"id":id}),
            "DELETE",
            base.clone(),
            json!({"authorizeWorkspaceDeletion":false}),
            row.clone(),
        ),
        (
            "sandbox_environment_delete",
            json!({"projectId":project,"id":id,"authorizeWorkspaceDeletion":true}),
            "DELETE",
            base,
            json!({"authorizeWorkspaceDeletion":true}),
            row,
        ),
    ];
    let replies = cases
        .iter()
        .map(|(_, _, method, path, body, row)| {
            response(method, path.clone(), body.clone(), row.clone())
        })
        .collect();
    let (client, worker) = exchange(replies);
    let mut session = Session::default();
    for (tool, args, _, _, _, expected) in cases {
        assert_eq!(
            session.call(&client, tool, args).unwrap(),
            expected,
            "{tool}"
        );
    }
    assert!(session.owned.is_empty());
    worker.join().unwrap();
}

#[test]
fn tunnel_mcp_keeps_uncertain_and_remote_retirement_receipts_separate_from_public_evidence() {
    let project = Uuid::new_v4();
    let id = Uuid::new_v4();
    let row = tunnel(project, id);
    let base = format!("/api/ai-sandbox/tunnels/{id}");
    let retarget =
        json!({"id":Uuid::new_v4(),"taskId":Uuid::new_v4(),"target":row["spec"]["target"]});
    let check = json!({"mode":"application","path":"/health","bodySha256":null,"webSocket":true});
    let address = json!({"subdomain":"saved-preview","publicUrl":row["publicUrl"],"connected":false,"claimedBy":id});
    let mut cases = vec![
        (
            "sandbox_tunnels_inventory",
            json!({}),
            "GET",
            "/api/ai-sandbox/tunnels".into(),
            Value::Null,
            json!([row]),
        ),
        (
            "sandbox_tunnel_addresses",
            json!({}),
            "GET",
            "/api/ai-sandbox/tunnel-addresses".into(),
            Value::Null,
            json!([address]),
        ),
        (
            "sandbox_tunnels_list",
            json!({"projectId":project}),
            "GET",
            format!("/api/ai-sandbox/projects/{project}/tunnels"),
            Value::Null,
            json!([row]),
        ),
        (
            "sandbox_tunnel_save",
            json!({"projectId":project,"id":id,"spec":row["spec"]}),
            "PUT",
            format!("/api/ai-sandbox/projects/{project}/tunnels/{id}"),
            row["spec"].clone(),
            row.clone(),
        ),
        (
            "sandbox_tunnel_retarget",
            json!({"id":id,"request":retarget}),
            "POST",
            format!("{base}/retarget"),
            retarget,
            row.clone(),
        ),
        (
            "sandbox_tunnel_verify",
            json!({"id":id,"request":check}),
            "POST",
            format!("{base}/verify"),
            check,
            row.clone(),
        ),
    ];
    for (tool, method, action) in [
        ("sandbox_tunnel_get", "GET", ""),
        ("sandbox_tunnel_publish", "POST", "/publish"),
        ("sandbox_tunnel_reconcile", "POST", "/reconcile"),
        ("sandbox_tunnel_stop", "POST", "/stop"),
        ("sandbox_tunnel_delete", "DELETE", ""),
    ] {
        cases.push((
            tool,
            json!({"id":id}),
            method,
            format!("{base}{action}"),
            if method == "POST" {
                json!({})
            } else {
                Value::Null
            },
            row.clone(),
        ));
    }
    let replies = cases
        .iter()
        .map(|(_, _, method, path, body, row)| {
            response(method, path.clone(), body.clone(), row.clone())
        })
        .collect();
    let (client, worker) = exchange(replies);
    let mut session = Session::default();
    for (tool, args, _, _, _, expected) in cases {
        assert_eq!(
            session.call(&client, tool, args).unwrap(),
            expected,
            "{tool}"
        );
    }
    worker.join().unwrap();
}

#[test]
fn tunnel_mcp_advertises_and_preserves_shutdown_evidence_without_claiming_token_retirement() {
    let id = Uuid::new_v4();
    let mut row = tunnel(Uuid::new_v4(), id);
    row["state"] = json!("deleted");
    row["localRemoved"] = json!(true);
    row["publicShutdown"] = json!("verified");
    row["shutdownProof"] =
        json!({"path":"/","status":503,"errorCode":"target_offline","checkedAt":123});
    let (client, worker) = exchange(vec![response(
        "POST",
        format!("/api/ai-sandbox/tunnels/{id}/verify"),
        json!({"mode":"shutdown","path":"/","bodySha256":null,"webSocket":false}),
        row.clone(),
    )]);
    let mut session = Session::default();
    assert_eq!(
        session
            .call(
                &client,
                "sandbox_tunnel_verify",
                json!({"id":id,"request":{"mode":"shutdown","path":"/"}})
            )
            .unwrap(),
        row
    );
    worker.join().unwrap();
    let tools = catalog();
    let tool = tools
        .iter()
        .find(|tool| tool["name"] == "sandbox_tunnel_verify")
        .unwrap();
    assert_eq!(
        tool["inputSchema"]["properties"]["request"]["properties"]["mode"]["enum"],
        json!(["application", "shutdown"])
    );
    assert!(
        tool["outputSchema"]["properties"]["shutdownProof"]["properties"]["checkedAt"].is_object()
    );
}

#[test]
fn guest_mcp_reports_unsupported_capabilities_and_signal_acceptance_without_termination_claim() {
    let project = Uuid::new_v4();
    let id = Uuid::new_v4();
    let base = format!("/api/ai-sandbox/projects/{project}/tasks/{id}/guest");
    let health = json!({"version":1,"init":"openrc","execution":false,"docker":false,"nspawn":false,"unsupported":"missing supervisor"});
    let mut crun_health = health.clone();
    crun_health["crun"] = json!(true);
    let process = json!({"pid":42,"startTicks":100,"bootId":"boot-1","state":"S"});
    let signal = json!({"process":process,"signal":"terminate"});
    let mut sent = response(
        "POST",
        format!("{base}/signal"),
        signal.clone(),
        Value::Null,
    );
    sent.status = 204;
    sent.response.clear();
    let (client, worker) = exchange(vec![
        response("GET", format!("{base}/health"), Value::Null, health.clone()),
        response(
            "GET",
            format!("{base}/health"),
            Value::Null,
            crun_health.clone(),
        ),
        response(
            "GET",
            format!("{base}/processes"),
            Value::Null,
            json!([process]),
        ),
        sent,
    ]);
    let mut session = Session::default();
    let mut expected_health = health;
    expected_health["crun"] = json!(false);
    assert_eq!(
        session
            .call(
                &client,
                "sandbox_guest_health",
                json!({"projectId":project,"id":id})
            )
            .unwrap(),
        expected_health
    );
    assert_eq!(
        session
            .call(
                &client,
                "sandbox_guest_health",
                json!({"projectId":project,"id":id})
            )
            .unwrap(),
        crun_health
    );
    assert_eq!(
        session
            .call(
                &client,
                "sandbox_guest_processes",
                json!({"projectId":project,"id":id})
            )
            .unwrap(),
        json!([process])
    );
    assert_eq!(
        session
            .call(
                &client,
                "sandbox_guest_signal",
                json!({"projectId":project,"id":id,"request":signal})
            )
            .unwrap(),
        json!({"sent":true,"terminationConfirmed":false})
    );
    worker.join().unwrap();
}

#[test]
fn artifact_mcp_checks_attempt_size_digest_and_stream_bound_before_returning_bytes() {
    use sha2::{Digest, Sha256};
    let id = Uuid::new_v4();
    let attempt = Uuid::new_v4();
    let artifact = Uuid::new_v4();
    let payload = b"retained\0artifact\n";
    let metadata = json!({"id":artifact,"taskId":id,"attemptId":attempt,"path":"proof.bin","size":payload.len(),"sha256":format!("{:x}",Sha256::digest(payload))});
    for case in [
        "valid",
        "foreign-attempt",
        "wrong-response-task",
        "wrong-artifact-task",
        "declared-oversize",
        "wrong-digest",
        "stream-oversize",
    ] {
        let mut entry = metadata.clone();
        let (response_task, response_attempt) = if case == "wrong-response-task" {
            let task = Uuid::new_v4();
            let attempt = Uuid::new_v4();
            entry["taskId"] = json!(task);
            entry["attemptId"] = json!(attempt);
            (task, attempt)
        } else {
            (id, attempt)
        };
        if case == "wrong-artifact-task" {
            entry["taskId"] = json!(Uuid::new_v4());
        }
        if case == "foreign-attempt" {
            entry["attemptId"] = json!(Uuid::new_v4());
        }
        if case == "declared-oversize" {
            entry["size"] = json!(65_537);
        }
        let row = task(response_task, response_attempt, json!([entry]), "result");
        let mut replies = vec![response(
            "GET",
            format!("/api/ai-sandbox/tasks/{id}"),
            Value::Null,
            row,
        )];
        if !matches!(
            case,
            "foreign-attempt" | "wrong-response-task" | "wrong-artifact-task" | "declared-oversize"
        ) {
            let mut bytes = response(
                "GET",
                format!("/api/ai-sandbox/tasks/{id}/artifacts/{artifact}"),
                Value::Null,
                Value::Null,
            );
            bytes.response = match case {
                "wrong-digest" => vec![b'x'; payload.len()],
                "stream-oversize" => vec![0; 65_537],
                _ => payload.to_vec(),
            };
            replies.push(bytes);
        }
        let (client, worker) = exchange(replies);
        let result = Session::default().call(
            &client,
            "sandbox_artifact_read",
            json!({"id":id,"artifact":artifact}),
        );
        if case == "valid" {
            assert_eq!(
                result.unwrap(),
                json!({"artifact":metadata,"encoding":"base64","content":STANDARD.encode(payload)})
            );
        } else {
            let error = result.unwrap_err();
            assert!(
                error.contains(match case {
                    "wrong-response-task" => "does not match the requested task",
                    "foreign-attempt" | "wrong-artifact-task" => "does not belong",
                    "declared-oversize" | "stream-oversize" => "bound",
                    _ => "SHA-256",
                }),
                "{case}: {error}"
            );
        }
        worker.join().unwrap();
    }
}

#[test]
fn task_mcp_wait_returns_interrupted_without_reconciling_or_rerunning() {
    let id = Uuid::new_v4();
    let row = task(id, Uuid::new_v4(), json!([]), "interrupted");
    let (client, worker) = exchange(vec![response(
        "GET",
        format!("/api/ai-sandbox/tasks/{id}"),
        Value::Null,
        row.clone(),
    )]);
    assert_eq!(
        Session::default()
            .call(
                &client,
                "sandbox_task_wait",
                json!({"id":id,"timeoutSeconds":1})
            )
            .unwrap(),
        json!({"reached":false,"task":serde_json::from_value::<SandboxTask>(row).unwrap()})
    );
    worker.join().unwrap();
}

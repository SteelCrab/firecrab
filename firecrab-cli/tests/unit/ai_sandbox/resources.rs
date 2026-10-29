use super::*;

#[test]
fn discovery_and_guest_cli_return_actual_readiness_and_unsupported_metadata() {
    let project = Uuid::new_v4();
    let id = Uuid::new_v4();
    let env = environment(project, id);
    let binding = tunnel(project, id);
    let health = json!({"version":1,"init":"openrc","execution":false,"docker":false,"crun":false,"nspawn":false,"unsupported":"missing supervisor"});
    let process = json!({"pid":42,"startTicks":100,"bootId":"boot-1","state":"S"});
    let address = json!({"subdomain":"test-preview","publicUrl":binding["publicUrl"],"connected":false,"claimedBy":id});
    let cases = vec![
        (
            Command::Environment {
                command: EnvironmentCommand::Images { project },
            },
            format!("/api/ai-sandbox/projects/{project}/environment-images"),
            json!([]),
        ),
        (
            Command::Environment {
                command: EnvironmentCommand::List { project },
            },
            format!("/api/ai-sandbox/projects/{project}/environments"),
            json!([env]),
        ),
        (
            Command::Tunnel {
                command: TunnelCommand::Addresses,
            },
            "/api/ai-sandbox/tunnel-addresses".into(),
            json!([address]),
        ),
        (
            Command::Tunnel {
                command: TunnelCommand::List { project },
            },
            format!("/api/ai-sandbox/projects/{project}/tunnels"),
            json!([binding]),
        ),
        (
            Command::Guest {
                project,
                id,
                command: super::super::resources::GuestCommand::Health,
            },
            format!("/api/ai-sandbox/projects/{project}/tasks/{id}/guest/health"),
            health,
        ),
        (
            Command::Guest {
                project,
                id,
                command: super::super::resources::GuestCommand::Processes,
            },
            format!("/api/ai-sandbox/projects/{project}/tasks/{id}/guest/processes"),
            json!([process]),
        ),
    ];
    for (command, path, row) in cases {
        let (client, worker) = reply("GET", path, Value::Null, 200, row.clone());
        assert_eq!(request(&client, command, [].as_slice()).unwrap(), row);
        worker.join().unwrap();
    }
}

#[test]
fn guest_health_cli_accepts_legacy_replies_and_reports_crun_support() {
    let project = Uuid::new_v4();
    let id = Uuid::new_v4();
    for crun in [None, Some(true)] {
        let mut health = json!({"version":1,"init":"openrc","execution":true,"docker":false,"nspawn":false,"unsupported":null});
        if let Some(value) = crun {
            health["crun"] = json!(value);
        }
        let (client, worker) = reply(
            "GET",
            format!("/api/ai-sandbox/projects/{project}/tasks/{id}/guest/health"),
            Value::Null,
            200,
            health.clone(),
        );
        health["crun"] = json!(crun.unwrap_or(false));
        let command = Command::Guest {
            project,
            id,
            command: super::super::resources::GuestCommand::Health,
        };
        assert_eq!(request(&client, command, [].as_slice()).unwrap(), health);
        worker.join().unwrap();
    }
}

fn environment(project: Uuid, id: Uuid) -> Value {
    let spec = json!({"version":1,"subnetCidr":"172.29.80.0/24","roles":[{"name":"backend","template":{"name":"ubuntu","version":"1","kernelSha256":"1".repeat(64),"rootfsSha256":"2".repeat(64),"initrdSha256":null,"bootArgsSha256":"3".repeat(64)},"cpu":1,"memoryMib":512,"diskGb":4,"dependsOn":[],"services":[{"name":"app","port":8080,"readiness":true}],"executionMode":{"kind":"persistent_workspace"}}],"allow":[],"retention":{"ttlSeconds":null}});
    json!({"id":id,"projectId":project,"owner":"local","generation":Uuid::new_v4(),"spec":spec,"networkId":Uuid::new_v4(),"dnsMode":"private_only","roles":[],"state":"provisioning","createdAt":1,"updatedAt":1,"expiresAt":null,"exportedAt":null,"error":null})
}
fn tunnel(project: Uuid, id: Uuid) -> Value {
    let runtime = json!({"vmId":Uuid::new_v4(),"runtimeId":Uuid::new_v4()});
    let task = Uuid::new_v4();
    json!({"id":id,"projectId":project,"taskId":task,"attemptId":Uuid::new_v4(),"runtime":runtime,"owner":"local","spec":{"taskId":task,"subdomain":"test-preview","target":{"kind":"vm_app","runtime":runtime,"port":8080}},"state":"connected","appReady":true,"connectorId":Uuid::new_v4(),"appliedTarget":{"type":"apps","upstreams":["172.29.80.2:8080"]},"publicUrl":"https://test-preview.firecrab.dev","helperConnected":true,"publicHttps":"unverified","webSocketHandshake":"unverified","publicShutdown":"unverified","remoteRetirement":"not_requested","localRemoved":false,"createdAt":1,"updatedAt":2,"errorCode":null})
}
#[test]
fn environment_cli_preserves_project_scope_manifest_and_final_deletion_authorization() {
    let project = Uuid::new_v4();
    let id = Uuid::new_v4();
    let row = environment(project, id);
    let base = format!("/api/ai-sandbox/projects/{project}/environments/{id}");
    let cases = vec![
        (
            EnvironmentCommand::Create { project, id },
            "PUT",
            base.clone(),
            row["spec"].clone(),
            row["spec"].to_string().into_bytes(),
        ),
        (
            EnvironmentCommand::Inspect { project, id },
            "GET",
            base.clone(),
            Value::Null,
            vec![],
        ),
        (
            EnvironmentCommand::Reconcile { project, id },
            "POST",
            format!("{base}/reconcile"),
            json!({}),
            vec![],
        ),
        (
            EnvironmentCommand::Stop { project, id },
            "POST",
            format!("{base}/stop"),
            json!({}),
            vec![],
        ),
        (
            EnvironmentCommand::Resume { project, id },
            "POST",
            format!("{base}/resume"),
            json!({}),
            vec![],
        ),
        (
            EnvironmentCommand::Delete {
                project,
                id,
                authorize_workspace_deletion: false,
            },
            "DELETE",
            base.clone(),
            json!({"authorizeWorkspaceDeletion":false}),
            vec![],
        ),
        (
            EnvironmentCommand::Delete {
                project,
                id,
                authorize_workspace_deletion: true,
            },
            "DELETE",
            base,
            json!({"authorizeWorkspaceDeletion":true}),
            vec![],
        ),
    ];
    for (command, method, path, body, input) in cases {
        let (client, server) = reply(method, path, body, 200, row.clone());
        assert_eq!(
            request(&client, Command::Environment { command }, input.as_slice()).unwrap(),
            row
        );
        server.join().unwrap();
    }
    let export = json!({"environment":{"id":id,"generation":row["generation"],"role":""},"taskIds":[],"exportedAt":2});
    let (client, server) = reply(
        "POST",
        format!("/api/ai-sandbox/projects/{project}/environments/{id}/export"),
        json!({}),
        200,
        export.clone(),
    );
    assert_eq!(
        request(
            &client,
            Command::Environment {
                command: EnvironmentCommand::Export { project, id }
            },
            [].as_slice()
        )
        .unwrap(),
        export
    );
    server.join().unwrap();
}
#[test]
fn tunnel_cli_keeps_task_runtime_fixed_and_uses_distinct_stop_delete_and_verification_actions() {
    let project = Uuid::new_v4();
    let id = Uuid::new_v4();
    let row = tunnel(project, id);
    let base = format!("/api/ai-sandbox/tunnels/{id}");
    let check = json!({"mode":"application","path":"/health","bodySha256":"0".repeat(64),"webSocket":false});
    for (command, method, path, body, input) in [
        (
            TunnelCommand::Save { project, id },
            "PUT",
            format!("/api/ai-sandbox/projects/{project}/tunnels/{id}"),
            row["spec"].clone(),
            row["spec"].to_string().into_bytes(),
        ),
        (
            TunnelCommand::Inspect { id },
            "GET",
            base.clone(),
            Value::Null,
            vec![],
        ),
        (
            TunnelCommand::Publish { id },
            "POST",
            format!("{base}/publish"),
            json!({}),
            vec![],
        ),
        (
            TunnelCommand::Reconcile { id },
            "POST",
            format!("{base}/reconcile"),
            json!({}),
            vec![],
        ),
        (
            TunnelCommand::Stop { id },
            "POST",
            format!("{base}/stop"),
            json!({}),
            vec![],
        ),
        (
            TunnelCommand::Delete { id },
            "DELETE",
            base.clone(),
            Value::Null,
            vec![],
        ),
        (
            TunnelCommand::Verify { id },
            "POST",
            format!("{base}/verify"),
            check.clone(),
            check.to_string().into_bytes(),
        ),
    ] {
        let (client, server) = reply(method, path, body, 200, row.clone());
        assert_eq!(
            request(&client, Command::Tunnel { command }, input.as_slice()).unwrap(),
            row
        );
        server.join().unwrap();
    }
}

#[test]
fn tunnel_cli_preserves_explicit_shutdown_proof_and_operator_retirement_on_a_tombstone() {
    let project = Uuid::new_v4();
    let id = Uuid::new_v4();
    let mut row = tunnel(project, id);
    row["state"] = json!("deleted");
    row["localRemoved"] = json!(true);
    row["helperConnected"] = json!(false);
    row["publicShutdown"] = json!("verified");
    row["remoteRetirement"] = json!("operator_required");
    row["shutdownProof"] =
        json!({"path":"/","status":404,"errorCode":"unknown_address","checkedAt":123});
    let (client, server) = reply(
        "POST",
        format!("/api/ai-sandbox/tunnels/{id}/verify"),
        json!({"mode":"shutdown","path":"/","bodySha256":null,"webSocket":false}),
        200,
        row.clone(),
    );
    let input = br#"{"mode":"shutdown","path":"/"}"#;
    assert_eq!(
        request(
            &client,
            Command::Tunnel {
                command: TunnelCommand::Verify { id }
            },
            input.as_slice()
        )
        .unwrap(),
        row
    );
    server.join().unwrap();
}
#[test]
fn guest_signal_acceptance_does_not_claim_process_termination() {
    let project = Uuid::new_v4();
    let id = Uuid::new_v4();
    let signal = json!({"process":{"pid":42,"startTicks":99,"bootId":"boot","state":"S"},"signal":"terminate"});
    let (client, server) = reply(
        "POST",
        format!("/api/ai-sandbox/projects/{project}/tasks/{id}/guest/signal"),
        signal.clone(),
        204,
        Value::Null,
    );
    let result = request(
        &client,
        Command::Guest {
            project,
            id,
            command: super::super::resources::GuestCommand::Signal,
        },
        signal.to_string().as_bytes(),
    )
    .unwrap();
    assert_eq!(result, json!({"sent":true,"terminationConfirmed":false}));
    server.join().unwrap();
}

#[test]
fn inventory_and_successor_retarget_preserve_the_fixed_address_and_new_task_identity() {
    let project = Uuid::new_v4();
    let id = Uuid::new_v4();
    let old = Uuid::new_v4();
    let row = tunnel(project, id);
    let input = json!({"id":id,"taskId":row["taskId"],"target":row["spec"]["target"]});
    let (client, server) = reply(
        "POST",
        format!("/api/ai-sandbox/tunnels/{old}/retarget"),
        input.clone(),
        200,
        row.clone(),
    );
    assert_eq!(
        request(
            &client,
            Command::Tunnel {
                command: TunnelCommand::Retarget { id: old }
            },
            input.to_string().as_bytes()
        )
        .unwrap(),
        row
    );
    server.join().unwrap();
    let (client, server) = reply(
        "GET",
        "/api/ai-sandbox/tunnels".into(),
        Value::Null,
        200,
        json!([row]),
    );
    assert_eq!(
        request(
            &client,
            Command::Tunnel {
                command: TunnelCommand::Inventory
            },
            [].as_slice()
        )
        .unwrap()[0]["id"],
        id.to_string()
    );
    server.join().unwrap();
}
#[test]
fn bounded_task_wait_returns_interruption_without_retry_and_rejects_invalid_bounds() {
    let id = Uuid::new_v4();
    let client = ApiClient::new("http://127.0.0.1:1".into());
    for timeout in [0, 61] {
        assert!(
            wait_for_task(&client, id, timeout)
                .unwrap_err()
                .to_string()
                .contains("between 1 and 60")
        );
    }
    let task:SandboxTask=serde_json::from_value(json!({"id":id,"projectId":Uuid::new_v4(),"attemptId":Uuid::new_v4(),"runtime":null,"request":{"kind":"chat","input":"hello","selection":null,"retryOf":null},"selection":{"connectionId":Uuid::new_v4(),"model":"model"},"state":"interrupted","createdAt":1,"updatedAt":2,"result":"","errorCode":"sandbox_restart","error":"Reconcile first","events":[],"changes":[]})).unwrap();
    let (client, server) = reply(
        "GET",
        format!("/api/ai-sandbox/tasks/{id}"),
        Value::Null,
        200,
        serde_json::to_value(&task).unwrap(),
    );
    let result = wait_for_task(&client, id, 1).unwrap();
    assert!(!result.reached);
    assert_eq!(result.task.attempt_id, task.attempt_id);
    server.join().unwrap();
}

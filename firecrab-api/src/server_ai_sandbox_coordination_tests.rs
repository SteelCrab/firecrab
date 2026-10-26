use super::*;

#[tokio::test]
async fn bounded_explicit_retry_delivers_latest_debug_result_to_waiting_test_once() {
    let f = fixture().await;
    let connection = connect(&f, "openai").await;
    verify(&f, connection, false).await;
    let project = project(&f, connection, false).await;
    f.provider
        .replies
        .lock()
        .unwrap()
        .push_back(json!({"status":"failed","output":[]}));
    let debug = Uuid::new_v4();
    let path = format!("/api/ai-sandbox/projects/{project}/tasks/{debug}");
    let input = json!({"kind":"chat","input":"Debug reproduce","coordination":{"assignedAgent":"Debug","retryLimit":1}});
    assert_eq!(
        call(&f.app, "PUT", &path, input).await.0,
        StatusCode::ACCEPTED
    );
    assert_eq!(finish(&f, debug).await["state"], "failed");
    let test = Uuid::new_v4();
    let path = format!("/api/ai-sandbox/projects/{project}/tasks/{test}");
    let test_input = json!({"kind":"chat","input":"Test Debug fix","coordination":{"dependencies":[debug],"assignedAgent":"Test","retryLimit":0}});
    assert_eq!(
        call(&f.app, "PUT", &path, test_input.clone()).await.0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        call(
            &f.app,
            "GET",
            &format!("/api/ai-sandbox/tasks/{test}"),
            Value::Null
        )
        .await
        .1["state"],
        "waiting"
    );
    f.provider
        .replies
        .lock()
        .unwrap()
        .extend([text("Debug known fix"), text("Test verified Debug result")]);
    let retry = Uuid::new_v4();
    let retry_input = json!({"kind":"chat","input":"Debug apply known fix","retryOf":debug,"coordination":{"assignedAgent":"Debug","retryLimit":1}});
    assert_eq!(
        call(
            &f.app,
            "PUT",
            &format!("/api/ai-sandbox/projects/{project}/tasks/{retry}"),
            retry_input.clone()
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(finish(&f, retry).await["state"], "result");
    let result = finish(&f, test).await;
    assert_eq!(result["state"], "result");
    assert_eq!(result["dependencyResults"][0]["taskId"], retry.to_string());
    assert_eq!(result["dependencyResults"][0]["result"], "Debug known fix");
    assert_eq!(
        call(&f.app, "PUT", &path, test_input).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &f.app,
            "PUT",
            &format!("/api/ai-sandbox/projects/{project}/tasks/{retry}"),
            retry_input
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(f.state.store.sandbox_task_count(project).unwrap(), 3);
    assert_eq!(f.provider.requests.lock().unwrap().len(), 4);
    assert!(
        f.provider
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .to_string()
            .contains("Debug known fix")
    );
}

#[tokio::test]
async fn exhausted_dependency_stops_test_before_another_provider_call() {
    let f = fixture().await;
    let connection = connect(&f, "openai").await;
    verify(&f, connection, false).await;
    let project = project(&f, connection, false).await;
    f.provider
        .replies
        .lock()
        .unwrap()
        .push_back(json!({"status":"failed","output":[]}));
    let debug = Uuid::new_v4();
    assert_eq!(
        call(
            &f.app,
            "PUT",
            &format!("/api/ai-sandbox/projects/{project}/tasks/{debug}"),
            json!({"kind":"chat","input":"fail","coordination":{"retryLimit":0}})
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(finish(&f, debug).await["state"], "failed");
    let test = Uuid::new_v4();
    assert_eq!(
        call(
            &f.app,
            "PUT",
            &format!("/api/ai-sandbox/projects/{project}/tasks/{test}"),
            json!({"kind":"chat","input":"must not run","coordination":{"dependencies":[debug]}})
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    let result = finish(&f, test).await;
    assert_eq!(result["state"], "failed");
    assert_eq!(result["errorCode"], "sandbox_dependency_failed");
    assert_eq!(f.provider.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn exported_artifact_download_survives_work_file_removal_and_router_reload() {
    let f = fixture().await;
    let connection = connect(&f, "openai").await;
    verify(&f, connection, true).await;
    let project = project(&f, connection, true).await;
    f.provider.replies.lock().unwrap().extend([
        tool(
            "write",
            "write_file",
            json!({"path":"report.txt","content":"retained artifact bytes"}),
        ),
        tool("export", "retain_artifact", json!({"path":"report.txt"})),
        tool("delete", "delete_file", json!({"path":"report.txt"})),
        text("Report exported"),
    ]);
    let key = Uuid::new_v4();
    start(&f, project, key, "code", "Export report").await;
    let result = finish(&f, key).await;
    assert_eq!(result["state"], "result", "{result}");
    assert_eq!(result["artifacts"].as_array().unwrap().len(), 1);
    let artifact = result["artifacts"][0]["id"].as_str().unwrap();
    let restored = router(&f.state);
    let response = restored
        .oneshot(
            Request::builder()
                .uri(format!("/api/ai-sandbox/tasks/{key}/artifacts/{artifact}"))
                .header("host", "127.0.0.1:5523")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(&bytes[..], b"retained artifact bytes");
    let saved = f.state.store.sandbox_task(key).unwrap().unwrap();
    assert!(saved.results_exported);
    assert!(!saved.workspace.unwrap().join("work/report.txt").exists());
}

#[tokio::test]
async fn managed_environment_admission_pins_role_runtime_and_blocks_project_deletion() {
    use firecrab_api_types::*;
    let f = fixture().await;
    let connection = connect(&f, "openai").await;
    verify(&f, connection, true).await;
    let project = project(&f, connection, true).await;
    let spec: SandboxEnvironmentSpec = serde_json::from_value(json!({
        "version":1,"subnetCidr":"10.241.33.0/24","roles":[{
            "name":"debug","template":{"name":"fixture","version":"v1","kernelSha256":"a".repeat(64),"rootfsSha256":"b".repeat(64),"bootArgsSha256":"c".repeat(64)},
            "cpu":1,"memoryMib":256,"diskGb":1,
            "executionMode":{"kind":"agent_sandbox","runtime":"docker","image":format!("example.invalid/image@sha256:{}", "d".repeat(64))}
        }]
    })).unwrap();
    let mut environment = crate::ai_sandbox::environment::reservation(
        Uuid::new_v4(),
        project,
        Default::default(),
        spec,
    )
    .unwrap();
    f.state
        .store
        .reserve_sandbox_environment(&environment)
        .unwrap();
    let mut saved_project = f.state.store.sandbox_project(project).unwrap().unwrap();
    saved_project.spec.vm_id = None;
    saved_project.spec.environment_id = Some(environment.id);
    assert_eq!(
        call(
            &f.app,
            "PUT",
            &format!("/api/ai-sandbox/projects/{project}"),
            serde_json::to_value(&saved_project.spec).unwrap()
        )
        .await
        .0,
        StatusCode::OK
    );
    let input =
        json!({"kind":"code","input":"Inspect environment context","environmentRole":"debug"});
    let rejected = Uuid::new_v4();
    let count = f.provider.requests.lock().unwrap().len();
    assert!(
        call(
            &f.app,
            "PUT",
            &format!("/api/ai-sandbox/projects/{project}/tasks/{rejected}"),
            input.clone()
        )
        .await
        .0
        .is_client_error()
    );
    assert_eq!(f.provider.requests.lock().unwrap().len(), count);
    let role = &mut environment.roles[0];
    let runtime = SandboxRuntimeRef {
        vm_id: role.vm_id,
        runtime_id: Uuid::new_v4(),
    };
    role.runtime = Some(runtime);
    role.agent = SandboxReadiness::Ready;
    role.network = SandboxReadiness::Ready;
    role.application = SandboxReadiness::Ready;
    environment.state = SandboxEnvironmentState::Ready;
    let definition = &environment.spec.roles[0];
    let mut vm = crate::handlers::vms::test_support::record(
        &format!("ase-{}-debug", &environment.id.simple().to_string()[..12]),
        runtime.vm_id,
    );
    vm.state = VmState::Running;
    vm.last_runtime_id = Some(runtime.runtime_id);
    vm.micro_network_id = environment.network_id;
    vm.security_group_id = Some(role.security_group_id);
    vm.template = definition.template.name.clone();
    vm.template_version = definition.template.version.clone();
    vm.template_kernel_sha256 = definition.template.kernel_sha256.clone();
    vm.template_rootfs_sha256 = definition.template.rootfs_sha256.clone();
    vm.template_boot_args_sha256 = definition.template.boot_args_sha256.clone();
    vm.cpu = definition.cpu;
    vm.ram = definition.memory_mib;
    vm.disk_gb = definition.disk_gb;
    vm.egress_policy = EgressPolicy::Isolated;
    vm.env.clear();
    crate::handlers::vms::test_support::seed_vm(&f.state, &vm);
    f.state
        .store
        .save_sandbox_environment(&environment)
        .unwrap();
    let key = Uuid::new_v4();
    f.provider
        .replies
        .lock()
        .unwrap()
        .push_back(text("Environment context inspected"));
    let (status, admitted) = call(
        &f.app,
        "PUT",
        &format!("/api/ai-sandbox/projects/{project}/tasks/{key}"),
        input,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{admitted}");
    assert_eq!(admitted["environment"]["id"], environment.id.to_string());
    assert_eq!(
        admitted["environment"]["generation"],
        environment.generation.to_string()
    );
    assert_eq!(admitted["runtime"]["vmId"], runtime.vm_id.to_string());
    assert_eq!(finish(&f, key).await["state"], "result");
    let saved = f.state.store.sandbox_task(key).unwrap().unwrap();
    assert_eq!(
        saved.execution_mode,
        Some(definition.execution_mode.clone())
    );
    let (status, response) = call(
        &f.app,
        "DELETE",
        &format!("/api/ai-sandbox/projects/{project}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    environment.state = SandboxEnvironmentState::Stopped;
    f.state
        .store
        .save_sandbox_environment(&environment)
        .unwrap();
    assert!(crate::ai_sandbox::access::attempt(&f.state, &saved).is_err());
}

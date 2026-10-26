use super::*;

async fn failed_prerequisite(f: &Fixture, project: Uuid, name: &str) -> Uuid {
    let key = Uuid::new_v4();
    f.provider
        .replies
        .lock()
        .unwrap()
        .push_back(json!({"status":"failed","output":[]}));
    let (status, task) = call(
        &f.app,
        "PUT",
        &format!("/api/ai-sandbox/projects/{project}/tasks/{key}"),
        json!({"kind":"chat","input":name,"coordination":{"retryLimit":1}}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(finish(f, key).await["state"], "failed");
    key
}

async fn dependent(f: &Fixture, project: Uuid, prerequisite: Uuid) -> Uuid {
    let key = Uuid::new_v4();
    let (status, task) = call(&f.app, "PUT", &format!("/api/ai-sandbox/projects/{project}/tasks/{key}"),
        json!({"kind":"chat","input":"Wait for the actual prerequisite result","coordination":{"dependencies":[prerequisite],"retryLimit":0}})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["state"], "waiting");
    key
}

async fn explicit_retry(f: &Fixture, project: Uuid, prerequisite: Uuid) -> Uuid {
    let key = Uuid::new_v4();
    let (status, task) = call(&f.app, "PUT", &format!("/api/ai-sandbox/projects/{project}/tasks/{key}"),
        json!({"kind":"chat","input":"Explicitly retry the known failure","retryOf":prerequisite,"coordination":{"retryLimit":1}})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    key
}

#[tokio::test]
async fn independent_waiting_chains_do_not_block_each_others_explicit_retries() {
    let f = fixture().await;
    let connection = connect(&f, "openai").await;
    verify(&f, connection, false).await;
    let project = project(&f, connection, false).await;
    let first = failed_prerequisite(&f, project, "First prerequisite").await;
    let second = failed_prerequisite(&f, project, "Independent prerequisite").await;
    let first_waiter = dependent(&f, project, first).await;
    let second_waiter = dependent(&f, project, second).await;
    let first_retry = explicit_retry(&f, project, first).await;
    assert_eq!(finish(&f, first_retry).await["state"], "result");
    let result = finish(&f, first_waiter).await;
    assert_eq!(result["state"], "result");
    assert_eq!(
        result["dependencyResults"][0]["taskId"],
        first_retry.to_string()
    );
    assert_eq!(
        f.state
            .store
            .sandbox_task(second_waiter)
            .unwrap()
            .unwrap()
            .task
            .state,
        firecrab_api_types::SandboxTaskState::Waiting
    );
    let second_retry = explicit_retry(&f, project, second).await;
    assert_eq!(finish(&f, second_retry).await["state"], "result");
    let result = finish(&f, second_waiter).await;
    assert_eq!(result["state"], "result");
    assert_eq!(
        result["dependencyResults"][0]["taskId"],
        second_retry.to_string()
    );
    assert_eq!(f.state.store.sandbox_pending_count().unwrap(), 0);
    assert_eq!(f.provider.requests.lock().unwrap().len(), 7);
}

#[tokio::test]
async fn full_dependency_wait_queue_reserves_one_slot_for_its_prerequisite_retry() {
    let f = fixture().await;
    let connection = connect(&f, "openai").await;
    verify(&f, connection, false).await;
    let project = project(&f, connection, false).await;
    let prerequisite = failed_prerequisite(&f, project, "Retry needed by all waiters").await;
    let mut waiters = Vec::new();
    for _ in 0..crate::ai_sandbox::coordination::DEPENDENCY_ADMISSION_LIMIT {
        waiters.push(dependent(&f, project, prerequisite).await);
    }
    assert_eq!(f.state.store.sandbox_pending_count().unwrap(), 15);
    let rejected = Uuid::new_v4();
    let (status, response) = call(&f.app, "PUT", &format!("/api/ai-sandbox/projects/{project}/tasks/{rejected}"),
        json!({"kind":"chat","input":"Do not occupy the reserved retry slot","coordination":{"dependencies":[prerequisite]}})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    assert!(f.state.store.sandbox_task(rejected).unwrap().is_none());
    assert_eq!(f.provider.requests.lock().unwrap().len(), 2);
    let retry = explicit_retry(&f, project, prerequisite).await;
    assert_eq!(finish(&f, retry).await["state"], "result");
    for waiter in waiters {
        let result = finish(&f, waiter).await;
        assert_eq!(result["state"], "result");
        assert_eq!(result["dependencyResults"][0]["taskId"], retry.to_string());
    }
    assert_eq!(f.state.store.sandbox_pending_count().unwrap(), 0);
    assert_eq!(f.provider.requests.lock().unwrap().len(), 18);
}

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
    f.state
        .store
        .insert_micro_network(&MicroNetworkResponse {
            id: environment.network_id,
            name: format!("ase-{}-network", &environment.id.simple().to_string()[..12]),
            subnet_cidr: environment.spec.subnet_cidr.clone(),
            gateway: "10.241.33.1".into(),
            internet_enabled: false,
            uplink: None,
            ipv6_cidr: None,
            ipv6_gateway: None,
            ipv6_address_mode: None,
            ipv6_egress: None,
        })
        .unwrap();
    f.state
        .store
        .insert_security_group(&SecurityGroupResponse {
            id: role.security_group_id,
            name: vm.name.clone(),
            micro_network_id: environment.network_id,
            rules: vec![],
        })
        .unwrap();
    f.state.store.insert(&vm).unwrap();
    f.state.vms.lock().unwrap().insert(vm.id, vm.clone());
    let lease = f
        .state
        .store
        .allocate_lease(
            vm.id,
            crate::ipam::SubnetSpec::parse(environment.network_id, &environment.spec.subnet_cidr)
                .unwrap(),
        )
        .unwrap();
    role.ipv4 = Some(lease.ipv4.to_string());
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

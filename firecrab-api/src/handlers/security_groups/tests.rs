use super::*;
use crate::{
    model::VmRecord,
    server::{HttpConfig, build_router},
    templates::TemplateRegistry,
};
use axum::{Router, body::Body, http::Request};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn app(root: &std::path::Path) -> (AppState, Router) {
    let state = AppState::with_db_file(
        TemplateRegistry::from_specs(root, std::iter::empty()).unwrap(),
        root.join("state.db"),
    )
    .await
    .unwrap();
    state
        .store
        .put_pipe_auth(&crate::pipe_auth::SavedAuth::Github {
            username: "qa".into(),
            token: "test-only-secret".into(),
        })
        .unwrap();
    let config = HttpConfig {
        bind_addr: "127.0.0.1:3000".parse().unwrap(),
        allowed_origins: vec![],
        max_concurrent_requests: 128,
        request_timeout: std::time::Duration::from_secs(10),
        static_root: None,
    };
    (state.clone(), build_router(state, &config))
}

async fn request(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}

fn network(state: &AppState, ipv6: bool) -> Uuid {
    let id = Uuid::new_v4();
    let row = serde_json::from_value(json!({"id":id,"name":"lab","subnetCidr":"172.30.0.0/24","gateway":"172.30.0.1","internetEnabled":true,"uplink":null,"ipv6Cidr":if ipv6 {Some("fd00::/64")} else {None},"ipv6Gateway":null,"ipv6AddressMode":null,"ipv6Egress":null})).unwrap();
    state.store.insert_micro_network(&row).unwrap();
    id
}

async fn create_group(app: &Router, network: Uuid) -> Uuid {
    let (status, body) = request(
        app,
        "POST",
        "/api/security-groups",
        json!({"name":"db","microNetworkId":network}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["rules"], json!([]));
    Uuid::parse_str(body["id"].as_str().unwrap()).unwrap()
}

fn seed_vm(state: &AppState, group: Uuid, network: Uuid) -> VmRecord {
    let vm: VmRecord = serde_json::from_value(json!({"id":Uuid::new_v4(),"name":"worker","state":"created","template":"test","cpu":1,"ram":128,"micro_network_id":network,"security_group_id":group})).unwrap();
    state.store.insert(&vm).unwrap();
    state.vms.lock().unwrap().insert(vm.id, vm.clone());
    vm
}

fn rule() -> Value {
    json!({"direction":"ingress","protocol":"tcp","portFrom":5432,"portTo":5432,"peerCidr":"172.30.0.42/24"})
}

#[tokio::test]
async fn crud_normalizes_rules_and_survives_restart_without_losing_empty_policy() {
    let root = tempfile::tempdir().unwrap();
    let (state, router) = app(root.path()).await;
    let net = network(&state, false);
    let group = create_group(&router, net).await;
    assert!(
        network_policy::resolve(&state.store, Some(group), Some(net))
            .unwrap()
            .unwrap()
            .rules
            .is_empty()
    );
    let path = format!("/api/security-groups/{group}");
    let rules = format!("{path}/rules");
    let (status, saved) = request(&router, "PUT", &rules, json!({"rules":[rule()]})).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["rules"][0]["peerCidr"], "172.30.0.0/24");
    for (key, value) in [
        ("peerCidr", json!("::/0")),
        ("portFrom", json!(0)),
        ("portFrom", json!(6000)),
        ("protocol", json!("icmp")),
        ("peerCidr", json!("0.0.0.0/33")),
    ] {
        let mut invalid = rule();
        invalid[key] = value;
        assert_eq!(
            request(&router, "PUT", &rules, json!({"rules":[invalid]}))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(request(&router, "GET", &path, Value::Null).await.1, saved);
    }
    let vm = seed_vm(&state, group, net);
    drop(router);
    drop(state);
    let (restarted, router) = app(root.path()).await;
    assert_eq!(
        restarted.store.load_all().unwrap()[&vm.id].security_group_id,
        Some(group)
    );
    assert_eq!(request(&router, "GET", &path, Value::Null).await.1, saved);
    assert_eq!(
        request(&router, "DELETE", &path, Value::Null).await.0,
        StatusCode::CONFLICT
    );
    restarted.store.delete(vm.id).unwrap();
    restarted.vms.lock().unwrap().remove(&vm.id);
    assert_eq!(
        request(&router, "PUT", &rules, json!({"rules":[]})).await.0,
        StatusCode::OK
    );
    assert!(
        network_policy::resolve(&restarted.store, Some(group), Some(net))
            .unwrap()
            .unwrap()
            .rules
            .is_empty()
    );
    assert_eq!(
        request(&router, "DELETE", &path, Value::Null).await.0,
        StatusCode::NO_CONTENT
    );
    assert!(network_policy::resolve(&restarted.store, Some(group), Some(net)).is_err());
}

#[tokio::test]
async fn active_and_orphaned_vm_runtimes_block_policy_changes() {
    use std::os::fd::AsRawFd;
    let root = tempfile::tempdir().unwrap();
    let (state, router) = app(root.path()).await;
    let net = network(&state, false);
    let group = create_group(&router, net).await;
    let mut vm = seed_vm(&state, group, net);
    let path = format!("/api/security-groups/{group}/rules");
    state
        .store
        .allocate_lease(
            vm.id,
            crate::ipam::SubnetSpec::parse(net, "172.30.0.0/24").unwrap(),
        )
        .unwrap();
    for state_value in [
        crate::model::VmState::Starting,
        crate::model::VmState::Running,
        crate::model::VmState::Pausing,
        crate::model::VmState::Paused,
        crate::model::VmState::Resuming,
        crate::model::VmState::Stopping,
    ] {
        state.vms.lock().unwrap().get_mut(&vm.id).unwrap().state = state_value;
        vm.state = state_value;
        state.store.update(&vm).unwrap();
        assert_eq!(
            crate::handlers::vms::active_vm_policy_specs(&state)
                .await
                .unwrap()[0]
                .security_group
                .as_ref()
                .unwrap()
                .id,
            group
        );
        assert_eq!(
            request(&router, "PUT", &path, json!({"rules":[rule()]}))
                .await
                .0,
            StatusCode::CONFLICT
        );
    }
    state.vms.lock().unwrap().get_mut(&vm.id).unwrap().state = crate::model::VmState::Error;
    vm.state = crate::model::VmState::Error;
    state.store.update(&vm).unwrap();
    let directory =
        crate::artifacts::VmArtifactPaths::for_vm(&state.vms_dir_for(&vm.storage_root), vm.id).dir;
    std::fs::create_dir_all(&directory).unwrap();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("vm.lock"))
        .unwrap();
    // SAFETY: test-owned file descriptor, released before removing the temp directory.
    assert_eq!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    assert_eq!(
        request(&router, "PUT", &path, json!({"rules":[]})).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &router,
            "DELETE",
            &format!("/api/vms/{}", vm.id),
            Value::Null
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        crate::handlers::vms::active_vm_policy_specs(&state)
            .await
            .unwrap()[0]
            .security_group
            .as_ref()
            .unwrap()
            .id,
        group
    );
    // Explicit unlock also releases descriptors inherited by concurrent fork/exec tests.
    // SAFETY: this test still owns the same valid descriptor.
    assert_eq!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) }, 0);
    drop(file);
    assert!(
        crate::handlers::vms::active_vm_policy_specs(&state)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        request(&router, "PUT", &path, json!({"rules":[]})).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn services_reference_groups_even_without_replicas_and_block_queued_rule_edits() {
    let root = tempfile::tempdir().unwrap();
    let (state, router) = app(root.path()).await;
    let net = network(&state, false);
    let group = create_group(&router, net).await;
    let spec = json!({"name":"web","image":"ghcr.io/qa/app:v1","ram":128,"cpu":1,"port":8080,"securityGroupId":group});
    let (status, service) = request(&router, "POST", "/api/micro-services", spec).await;
    assert_eq!(status, StatusCode::CREATED, "{service}");
    assert_eq!(service["securityGroupId"], group.to_string());
    let path = format!("/api/security-groups/{group}");
    assert_eq!(
        request(&router, "DELETE", &path, Value::Null).await.0,
        StatusCode::CONFLICT
    );
    let _permit = state.service_deploy_permits.acquire().await.unwrap();
    let deployment = format!(
        "/api/micro-services/{}/deployment",
        service["id"].as_str().unwrap()
    );
    let (status, snapshot) = request(
        &router,
        "POST",
        &deployment,
        json!({"hostPort":30100,"microNetworkId":net}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{snapshot}");
    assert_eq!(snapshot["spec"]["securityGroupId"], group.to_string());
    assert_eq!(
        request(
            &router,
            "PUT",
            &format!("{path}/rules"),
            json!({"rules":[]})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn ipv6_foreign_and_missing_selections_are_rejected_before_replica_creation() {
    let root = tempfile::tempdir().unwrap();
    let (state, router) = app(root.path()).await;
    let ipv6 = network(&state, true);
    assert_eq!(
        request(
            &router,
            "POST",
            "/api/security-groups",
            json!({"name":"bad","microNetworkId":ipv6})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let net = network(&state, false);
    let group = create_group(&router, net).await;
    assert!(network_policy::resolve(&state.store, Some(group), Some(ipv6)).is_err());
    let spec = json!({"name":"web","image":"ghcr.io/qa/app:v1","ram":128,"cpu":1,"port":8080,"securityGroupId":group});
    let (status, service) = request(&router, "POST", "/api/micro-services", spec.clone()).await;
    assert_eq!(status, StatusCode::CREATED);
    let path = format!(
        "/api/micro-services/{}/deployment",
        service["id"].as_str().unwrap()
    );
    assert_eq!(
        request(
            &router,
            "POST",
            &path,
            json!({"hostPort":30100,"microNetworkId":ipv6})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert!(state.store.load_all().unwrap().is_empty());
    let mut missing = spec;
    missing["name"] = json!("missing");
    missing["securityGroupId"] = json!(Uuid::new_v4());
    assert_eq!(
        request(&router, "POST", "/api/micro-services", missing)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn malformed_saved_rules_fail_resolution_instead_of_disabling_the_policy() {
    let root = tempfile::tempdir().unwrap();
    let (state, router) = app(root.path()).await;
    let net = network(&state, false);
    let group = create_group(&router, net).await;
    request(
        &router,
        "PUT",
        &format!("/api/security-groups/{group}/rules"),
        json!({"rules":[rule()]}),
    )
    .await;
    let conn = rusqlite::Connection::open(root.path().join("state.db")).unwrap();
    conn.execute(
        "UPDATE security_group_rules SET rule = '{invalid' WHERE group_id = ?1",
        [group.to_string()],
    )
    .unwrap();
    assert!(network_policy::resolve(&state.store, Some(group), Some(net)).is_err());
    assert_eq!(
        request(
            &router,
            "GET",
            &format!("/api/security-groups/{group}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn vm_selection_preserves_omission_clears_null_and_rejects_active_or_foreign_groups() {
    let root = tempfile::tempdir().unwrap();
    let (state, router) = app(root.path()).await;
    let net = network(&state, false);
    let group = create_group(&router, net).await;
    let foreign_net = network(&state, false);
    let foreign_group = create_group(&router, foreign_net).await;
    let vm = seed_vm(&state, group, net);
    let path = format!("/api/vms/{}", vm.id);
    let mut body = json!({"cpu":1,"ram":128,"diskGb":2,"egressPolicy":"internet"});
    assert_eq!(
        request(&router, "PUT", &path, body.clone()).await.0,
        StatusCode::OK
    );
    assert_eq!(
        state.store.load_all().unwrap()[&vm.id].security_group_id,
        Some(group)
    );
    body["securityGroupId"] = json!(foreign_group);
    assert_eq!(
        request(&router, "PUT", &path, body.clone()).await.0,
        StatusCode::BAD_REQUEST
    );
    body["securityGroupId"] = Value::Null;
    state.vms.lock().unwrap().get_mut(&vm.id).unwrap().state = crate::model::VmState::Running;
    assert_eq!(
        request(&router, "PUT", &path, body.clone()).await.0,
        StatusCode::CONFLICT
    );
    state.vms.lock().unwrap().get_mut(&vm.id).unwrap().state = crate::model::VmState::Stopped;
    assert_eq!(
        request(&router, "PUT", &path, body.clone()).await.0,
        StatusCode::OK
    );
    assert_eq!(
        state.store.load_all().unwrap()[&vm.id].security_group_id,
        None
    );
    body["securityGroupId"] = json!(group);
    assert_eq!(request(&router, "PUT", &path, body).await.0, StatusCode::OK);
    assert_eq!(
        state.store.load_all().unwrap()[&vm.id].security_group_id,
        Some(group)
    );
}

#[tokio::test]
async fn service_ownership_blocks_rules_and_replica_attachment_edits_until_cleanup() {
    use firecrab_api_types::{
        DeployMicroServiceRequest, MicroServiceDeployment, ServiceInstance, ServicePhase,
    };
    let root = tempfile::tempdir().unwrap();
    let (state, router) = app(root.path()).await;
    let net = network(&state, false);
    let group = create_group(&router, net).await;
    let vm = seed_vm(&state, group, net);
    let (status,service)=request(&router,"POST","/api/micro-services",json!({"name":"web","image":"ghcr.io/qa/app:v1","ram":128,"cpu":1,"port":8080,"securityGroupId":group})).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = Uuid::parse_str(service["id"].as_str().unwrap()).unwrap();
    let mut deployment = MicroServiceDeployment {
        id: Uuid::new_v4(),
        service_id: id,
        phase: ServicePhase::Running,
        spec: state.store.micro_service(id).unwrap().unwrap().spec,
        request: DeployMicroServiceRequest {
            micro_network_id: Some(net),
            host_port: 30100,
        },
        digest: None,
        instances: vec![ServiceInstance {
            vm_id: vm.id,
            ipv4: "172.30.0.2".into(),
            host_port: 30100,
            state: crate::model::VmState::Stopped,
            healthy: false,
        }],
        log: vec![],
        error: None,
        domain_route_owned: false,
    };
    state.store.save_service_deployment(&deployment).unwrap();
    let rules = format!("/api/security-groups/{group}/rules");
    assert_eq!(
        request(&router, "PUT", &rules, json!({"rules":[]})).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &router,
            "PUT",
            &format!("/api/vms/{}", vm.id),
            json!({"cpu":1,"ram":128,"diskGb":2,"securityGroupId":null})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    deployment.phase = ServicePhase::Failed;
    state.store.save_service_deployment(&deployment).unwrap();
    assert_eq!(
        request(&router, "PUT", &rules, json!({"rules":[]})).await.0,
        StatusCode::CONFLICT
    );
    deployment.instances.clear();
    state.store.save_service_deployment(&deployment).unwrap();
    assert_eq!(
        request(&router, "PUT", &rules, json!({"rules":[]})).await.0,
        StatusCode::OK
    );
}

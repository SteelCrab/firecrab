use super::*;
use crate::{
    server::{HttpConfig, build_router},
    templates::TemplateRegistry,
};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use firecrab_stack::{Binding, ResourceKind};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

async fn fixture(root: &std::path::Path) -> (AppState, Router, Uuid) {
    let templates = TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
    let state = AppState::with_db_file(templates, root.join("state.db"))
        .await
        .unwrap();
    let id = Uuid::new_v4();
    state.store.insert_micro_network(&serde_json::from_value(json!({"id":id,"name":"test","subnetCidr":"172.31.20.0/24","gateway":"172.31.20.1","internetEnabled":true})).unwrap()).unwrap();
    let app = build_router(
        state.clone(),
        &HttpConfig {
            bind_addr: "127.0.0.1:5523".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 128,
            request_timeout: std::time::Duration::from_secs(10),
            static_root: None,
        },
    );
    (state, app, id)
}

async fn call(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
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
            serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                panic!(
                    "{method} {path}: {status}: {} ({error})",
                    String::from_utf8_lossy(&bytes)
                )
            })
        },
    )
}

#[tokio::test]
async fn iac_owner_api_transfers_without_recreation_and_blocks_direct_writes() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, id) = fixture(root.path()).await;
    let capabilities = call(&app, "GET", "/api/iac/identity", Value::Null).await;
    assert_eq!(capabilities.0, StatusCode::OK);
    assert_eq!(
        capabilities.1["inventoryKinds"].as_array().unwrap().len(),
        43
    );
    assert_eq!(
        capabilities.1["transferKinds"],
        json!([
            "network",
            "vm",
            "service",
            "storage_root",
            "volume",
            "security_group",
            "shell",
            "schedule"
        ])
    );
    assert_eq!(
        capabilities.1["endpointId"],
        state.store.iac_endpoint_id().unwrap().to_string()
    );
    let stack = state.store.register_stack("demo").unwrap();
    state
        .store
        .bind_stack_resource(
            "demo",
            &Binding {
                kind: ResourceKind::Network,
                key: "app".into(),
                resource_id: id,
            },
        )
        .unwrap();
    let path = format!("/api/iac/owners/network/{id}");
    let (code, before) = call(&app, "GET", &path, Value::Null).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(before["owner"]["id"], stack.id.to_string());
    let body = json!({"operationId":Uuid::new_v4(),"expectedEndpointId":before["endpointId"],"resource":before["resource"],"expectedGeneration":before["generation"],"expectedOwner":before["owner"],"owner":{"manager":"terraform","id":Uuid::new_v4(),"key":"firecrab_network.app"}});
    let (code, owned) = call(&app, "POST", "/api/iac/owners/transition", body.clone()).await;
    assert_eq!(code, StatusCode::OK, "{owned}");
    assert!(
        state
            .store
            .stack("demo")
            .unwrap()
            .unwrap()
            .bindings
            .is_empty()
    );
    let (code, error) = call(
        &app,
        "PATCH",
        &format!("/api/micro-networks/{id}"),
        json!({"internetEnabled":false}),
    )
    .await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "iac_resource_managed");
    let encoded = id.to_string().replace('-', "%2d");
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("/api/micro-networks/{encoded}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert!(
        state
            .store
            .micro_network(id)
            .unwrap()
            .unwrap()
            .internet_enabled
    );
    assert_eq!(
        call(&app, "GET", "/api/iac/owners", Value::Null).await.1,
        json!([owned])
    );
    assert_eq!(
        call(&app, "POST", "/api/iac/owners/transition", body.clone())
            .await
            .1,
        owned
    );
    let released=call(&app,"POST","/api/iac/owners/transition",json!({"operationId":Uuid::new_v4(),"expectedEndpointId":owned["endpointId"],"resource":owned["resource"],"expectedGeneration":owned["generation"],"expectedOwner":owned["owner"],"owner":null})).await;
    let receipt = call(
        &app,
        "GET",
        &format!(
            "/api/iac/owner-changes/{}",
            body["operationId"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await;
    assert_eq!(receipt.0, StatusCode::OK);
    assert_eq!(receipt.1["ownership"], owned);
    assert_eq!(released.0, StatusCode::OK, "{}", released.1);
    assert!(released.1["owner"].is_null());
    assert_eq!(
        call(&app, "GET", "/api/iac/owners", Value::Null).await.1,
        json!([])
    );
}

fn configuration_resource(
    state: &AppState,
    root: &std::path::Path,
    network: Uuid,
    kind: &str,
) -> ManagedResource {
    let id = Uuid::new_v4();
    match kind {
        "storage_root" => state
            .store
            .insert_micro_storage(id, "owned-root", root.to_str().unwrap())
            .unwrap(),
        "volume" => {
            state
                .store
                .insert_block_storage(
                    &firecrab_api_types::MicroBlockStorageResponse {
                        id,
                        name: "owned-volume".into(),
                        size_gib: 1,
                        storage_root: "default".into(),
                        state: "creating".into(),
                        attached_vm_id: None,
                        mount_path: None,
                    },
                    None,
                )
                .unwrap();
            state.store.finish_block_storage_create(id).unwrap();
        }
        "security_group" => state
            .store
            .insert_security_group(
                &serde_json::from_value(json!({
                    "id":id,"name":"owned-policy","microNetworkId":network,"rules":[]
                }))
                .unwrap(),
            )
            .unwrap(),
        "shell" => {
            use sha2::{Digest, Sha256};
            let script = "#!/bin/sh\ntrue\n";
            state
                .store
                .create_shell(&crate::persistence::NewShell {
                    id,
                    name: "owned-script",
                    description: None,
                    revision_id: Uuid::new_v4(),
                    content: script,
                    content_sha256: &format!("{:x}", Sha256::digest(script)),
                    now_ms: 1,
                })
                .unwrap();
        }
        "schedule" => state
            .store
            .insert_schedule(
                &serde_json::from_value(json!({
                    "id":id,"name":"owned-schedule","vmId":Uuid::new_v4(),"action":"start",
                    "timing":{"kind":"once","at":"2099-01-01T00:00:00Z"},"enabled":false,
                    "nextRunAtMs":null,"createdAtMs":1,"updatedAtMs":1,"lastRun":null
                }))
                .unwrap(),
            )
            .unwrap(),
        _ => panic!("unsupported fixture kind"),
    }
    ManagedResource {
        kind: kind.into(),
        id: id.to_string(),
    }
}

#[tokio::test]
async fn iac_configuration_ownership_transfers_and_guards_each_public_write_route() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let stack = state.store.register_stack("configuration").unwrap();
    let terraform = Uuid::new_v4();
    for (kind, namespace, edits) in [
        ("storage_root", "micro-storages", vec![("DELETE", "")]),
        (
            "volume",
            "microblockstorage",
            vec![("DELETE", ""), ("POST", "/attach"), ("POST", "/detach")],
        ),
        (
            "security_group",
            "security-groups",
            vec![("DELETE", ""), ("PUT", ""), ("PUT", "/rules")],
        ),
        (
            "shell",
            "shells",
            vec![("DELETE", ""), ("POST", "/revisions")],
        ),
        (
            "schedule",
            "schedules",
            vec![("DELETE", ""), ("PUT", ""), ("PUT", "/enabled")],
        ),
    ] {
        let resource = configuration_resource(&state, root.path(), network, kind);
        let free = state.store.iac_resource_owner(&resource).unwrap();
        let request = json!({"operationId":Uuid::new_v4(),"expectedEndpointId":free.endpoint_id,
            "resource":resource,"expectedGeneration":0,"expectedOwner":null,
            "owner":{"manager":"stack","id":stack.id,"key":"configuration"}});
        let (code, native) = call(&app, "POST", "/api/iac/owners/transition", request).await;
        assert_eq!(code, StatusCode::OK, "{kind}: {native}");
        assert_eq!(native["generation"], 1);
        assert_eq!(
            call(
                &app,
                "DELETE",
                &format!("/api/stacks/configuration?id={}", stack.id),
                Value::Null
            )
            .await
            .0,
            StatusCode::CONFLICT
        );

        let request = json!({"operationId":Uuid::new_v4(),"expectedEndpointId":free.endpoint_id,
            "resource":resource,"expectedGeneration":1,"expectedOwner":native["owner"],
            "owner":{"manager":"terraform","id":terraform,"key":format!("firecrab_{kind}.configuration")}});
        let (code, owned) = call(&app, "POST", "/api/iac/owners/transition", request.clone()).await;
        assert_eq!(code, StatusCode::OK, "{kind}: {owned}");
        assert_eq!(owned["generation"], 2);
        let encoded = resource.id.replace('-', "%2D");
        for (method, suffix) in edits {
            let (code, error) = call(
                &app,
                method,
                &format!("/api/{namespace}/{encoded}{suffix}"),
                json!({}),
            )
            .await;
            assert_eq!(code, StatusCode::CONFLICT, "{kind} {suffix}: {error}");
            assert_eq!(error["error"]["code"], "iac_resource_managed");
        }
        let (code, released) = call(&app, "POST", "/api/iac/owners/transition", json!({
            "operationId":Uuid::new_v4(),"expectedEndpointId":free.endpoint_id,"resource":resource,
            "expectedGeneration":2,"expectedOwner":owned["owner"],"owner":null
        })).await;
        assert_eq!(code, StatusCode::OK, "{kind}: {released}");
        assert!(released["owner"].is_null());
        assert_eq!(
            call(&app, "POST", "/api/iac/owners/transition", request)
                .await
                .1,
            owned
        );
        assert_eq!(
            state
                .store
                .iac_resource_owner(&resource)
                .unwrap()
                .generation,
            3
        );
    }
    assert_eq!(
        call(&app, "GET", "/api/iac/owners", Value::Null).await.1,
        json!([])
    );
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("/api/stacks/configuration?id={}", stack.id),
            Value::Null
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn iac_owner_api_rejects_stale_and_invalid_requests_without_changing_the_native_binding() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, id) = fixture(root.path()).await;
    state.store.register_stack("demo").unwrap();
    state
        .store
        .bind_stack_resource(
            "demo",
            &Binding {
                kind: ResourceKind::Network,
                key: "app".into(),
                resource_id: id,
            },
        )
        .unwrap();
    let body = json!({"operationId":Uuid::new_v4(),"expectedEndpointId":state.store.iac_endpoint_id().unwrap(),"resource":{"kind":"network","id":id},"expectedGeneration":0,"expectedOwner":null,"owner":{"manager":"terraform","id":Uuid::new_v4(),"key":"firecrab_network.app"}});
    let (code, error) = call(&app, "POST", "/api/iac/owners/transition", body).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "stale_iac_owner");
    assert_eq!(
        state.store.stack("demo").unwrap().unwrap().bindings.len(),
        1
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/iac/owners/transition",
            json!({"unexpected":"private-value"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let list = call(&app, "GET", "/api/iac/owners", Value::Null).await.1;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert!(!list.to_string().contains("private-value"));
}

#[tokio::test]
#[ignore = "manual local browser evidence; serves isolated fixture records and compiled dashboard"]
async fn iac_owner_browser_fixture() {
    let info = std::path::PathBuf::from(
        std::env::var_os("FIRECRAB_IAC_QA_INFO").expect("set a temporary QA info path"),
    );
    let assets = std::path::PathBuf::from(
        std::env::var_os("FIRECRAB_IAC_QA_ASSETS").expect("set compiled dashboard directory"),
    );
    assert!(assets.join("index.html").is_file());
    let root = tempfile::tempdir().unwrap();
    let (state, _router, id) = fixture(root.path()).await;
    let resource = ManagedResource {
        kind: "network".into(),
        id: id.to_string(),
    };
    let before = state.store.iac_resource_owner(&resource).unwrap();
    let change = ChangeResourceOwner {
        expected_endpoint_id: before.endpoint_id,
        operation_id: Uuid::new_v4(),
        resource: resource.clone(),
        expected_generation: before.generation,
        expected_owner: before.owner,
        owner: Some(firecrab_stack::ResourceOwner {
            manager: firecrab_stack::Manager::Terraform,
            id: Uuid::new_v4(),
            key: "firecrab_network.application".into(),
        }),
    };
    let ownership = state.store.change_iac_resource_owner(&change).unwrap();
    let mut ownerships = vec![ownership.clone()];
    for kind in [
        "storage_root",
        "volume",
        "security_group",
        "shell",
        "schedule",
    ] {
        let resource = configuration_resource(&state, root.path(), id, kind);
        let before = state.store.iac_resource_owner(&resource).unwrap();
        ownerships.push(
            state
                .store
                .change_iac_resource_owner(&ChangeResourceOwner {
                    expected_endpoint_id: before.endpoint_id,
                    operation_id: Uuid::new_v4(),
                    resource,
                    expected_generation: before.generation,
                    expected_owner: before.owner,
                    owner: Some(firecrab_stack::ResourceOwner {
                        manager: firecrab_stack::Manager::Terraform,
                        id: change.owner.as_ref().unwrap().id,
                        key: format!("firecrab_{kind}.application"),
                    }),
                })
                .unwrap(),
        );
    }
    ownerships.sort_by(|a, b| {
        (&a.resource.kind, &a.resource.id).cmp(&(&b.resource.kind, &b.resource.id))
    });
    let router = build_router(
        state.clone(),
        &HttpConfig {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 128,
            request_timeout: std::time::Duration::from_secs(10),
            static_root: Some(assets),
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let finished = std::sync::Arc::new(tokio::sync::Notify::new());
    let shutdown = finished.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { shutdown.notified().await })
            .await
            .unwrap();
    });
    std::fs::write(&info,serde_json::to_vec_pretty(&json!({"url":url,"ownership":ownership,"ownerships":ownerships,"operationId":change.operation_id,"fixture":true})).unwrap()).unwrap();
    let stop = info.with_extension("stop");
    let started = std::time::Instant::now();
    while !stop.exists() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(600),
            "browser fixture exceeded its deadline"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    finished.notify_one();
    server.await.unwrap();
    std::fs::remove_file(&info).unwrap();
    std::fs::remove_file(stop).unwrap();
}

#[tokio::test]
async fn iac_owner_api_waits_for_service_mutation_admission() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, id) = fixture(root.path()).await;
    let path = format!("/api/iac/owners/network/{id}");
    let before = call(&app, "GET", &path, Value::Null).await.1;
    let body = json!({"operationId":Uuid::new_v4(),"expectedEndpointId":before["endpointId"],"resource":before["resource"],"expectedGeneration":before["generation"],"expectedOwner":null,"owner":{"manager":"terraform","id":Uuid::new_v4(),"key":"firecrab_network.application"}});
    let services = state.service_mutations.lock().await;
    let mut pending =
        tokio::spawn(async move { call(&app, "POST", "/api/iac/owners/transition", body).await });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), &mut pending)
            .await
            .is_err()
    );
    assert!(
        state
            .store
            .iac_resource_owner(&ManagedResource {
                kind: "network".into(),
                id: id.to_string()
            })
            .unwrap()
            .owner
            .is_none()
    );
    drop(services);
    assert_eq!(pending.await.unwrap().0, StatusCode::OK);
}

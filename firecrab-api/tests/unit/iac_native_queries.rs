use super::*;
use crate::handlers::vms::test_support::{record, seed_vm, test_state};
use firecrab_stack::{NativeOperation, NativeOperationState};

fn router(state: AppState) -> Router {
    build_router(
        state,
        &HttpConfig {
            bind_addr: "127.0.0.1:5523".parse().unwrap(),
            allowed_origins: vec![],
            max_concurrent_requests: 128,
            request_timeout: std::time::Duration::from_secs(10),
            static_root: None,
        },
    )
}

#[tokio::test]
async fn iac_native_pages_include_unowned_work_and_exclude_completed_receipts() {
    let root = crate::firecracker::test_support::short_tempdir();
    let state = test_state(root.path()).await;
    for i in 10..13 {
        let vm = record("query-work", Uuid::new_v4());
        seed_vm(&state, &vm);
        let intent = state
            .store
            .begin_native_vm_delete(&vm, Uuid::from_u128(i), false)
            .unwrap();
        if i == 10 {
            state.store.fail_native_vm_delete(&intent).unwrap();
        }
        if i == 12 {
            state.store.finish_native_vm_delete(&intent).unwrap();
        }
    }
    let app = router(state.clone());
    let (status, first) = call(
        &app,
        "GET",
        "/api/iac/native-operations?limit=1",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["operations"].as_array().unwrap().len(), 1);
    assert_eq!(
        first["operations"][0]["operationId"],
        Uuid::from_u128(11).to_string()
    );
    assert!(first["operations"][0]["ownership"]["owner"].is_null());
    let before = first["nextBefore"].as_str().unwrap();
    let (status, second) = call(
        &app,
        "GET",
        &format!("/api/iac/native-operations?limit=1&before={before}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(
        second["operations"][0]["operationId"],
        Uuid::from_u128(10).to_string()
    );
    assert_eq!(second["operations"][0]["state"], "needs_recovery");
    assert!(second["nextBefore"].is_null());
    for query in [
        "limit=0",
        "limit=101",
        "before=00000000-0000-0000-0000-000000000000",
    ] {
        assert_eq!(
            call(
                &app,
                "GET",
                &format!("/api/iac/native-operations?{query}"),
                Value::Null
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/api/iac/native-operations/{}", Uuid::from_u128(12)),
            Value::Null
        )
        .await
        .1["state"],
        "succeeded"
    );
}

#[tokio::test]
async fn iac_native_vm_recovery_requires_operator_and_pins_expected_endpoint() {
    let root = crate::firecracker::test_support::short_tempdir();
    let state = test_state(root.path()).await;
    let vm = record("recovery-scope", Uuid::new_v4());
    seed_vm(&state, &vm);
    let id = Uuid::new_v4();
    let storage = crate::artifacts::cleanup::VmCleanupStorage::capture(
        root.path(),
        vm.id,
        vm.disk_generation,
    )
    .unwrap();
    let intent = state
        .store
        .begin_vm_delete_pinned(&vm, id, true, None, storage)
        .unwrap();
    state.store.fail_native_vm_delete(&intent).unwrap();
    let path = format!("/api/iac/native-operations/{id}/recover");
    let app = router(state.clone());
    assert_eq!(
        call(&app, "POST", &path, Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    let app = app.layer(axum::Extension(crate::ai_sandbox::access::LocalOperator));
    let wrong = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&path)
                .header("x-firecrab-endpoint-id", Uuid::new_v4().to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::CONFLICT);
    assert_eq!(
        state.store.iac_native_operation(id).unwrap().unwrap().state,
        NativeOperationState::NeedsRecovery
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&path)
                .header(
                    "x-firecrab-endpoint-id",
                    state.store.iac_endpoint_id().unwrap().to_string(),
                )
                .header("x-firecrab-execution-generation", "0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let completed: NativeOperation = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(completed.state, NativeOperationState::Succeeded);
    assert_eq!(completed.retain_disks, Some(true));
    let revision = state.store.lease_revision().unwrap();
    let repeated = call(&app, "POST", &path, Value::Null).await;
    assert_eq!(repeated.0, StatusCode::OK, "{}", repeated.1);
    assert_eq!(repeated.1, serde_json::to_value(completed).unwrap());
    assert_eq!(state.store.lease_revision().unwrap(), revision);
}

#[tokio::test]
async fn iac_native_recovery_rejects_missing_and_stale_execution_preconditions() {
    let root = crate::firecracker::test_support::short_tempdir();
    let state = test_state(root.path()).await;
    let vm = record("recovery-attempt", Uuid::new_v4());
    seed_vm(&state, &vm);
    let id = Uuid::new_v4();
    let first = state.store.begin_native_vm_delete(&vm, id, true).unwrap();
    state.store.fail_native_vm_delete(&first).unwrap();
    let current = state.store.recover_native_vm_delete_at(id, 0).unwrap();
    state.store.fail_native_vm_delete(&current).unwrap();
    assert!(matches!(
        state.store.recover_native_vm_delete_at(id, 0),
        Err(crate::persistence::PersistenceError::StaleIacExecution)
    ));
    let app =
        router(state.clone()).layer(axum::Extension(crate::ai_sandbox::access::LocalOperator));
    let path = format!("/api/iac/native-operations/{id}/recover");
    for (attempt, status) in [
        (None, StatusCode::BAD_REQUEST),
        (Some("invalid"), StatusCode::BAD_REQUEST),
        (Some("0"), StatusCode::CONFLICT),
    ] {
        let mut request = Request::builder().method("POST").uri(&path).header(
            "x-firecrab-endpoint-id",
            state.store.iac_endpoint_id().unwrap().to_string(),
        );
        if let Some(attempt) = attempt {
            request = request.header("x-firecrab-execution-generation", attempt);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        let unchanged = state.store.iac_native_operation(id).unwrap().unwrap();
        assert_eq!(unchanged.execution_generation, 1);
        assert_eq!(unchanged.state, NativeOperationState::NeedsRecovery);
    }
}

#[tokio::test]
async fn iac_native_browser_preflight_permits_only_explicit_retry_metadata_headers() {
    let root = crate::firecracker::test_support::short_tempdir();
    let state = test_state(root.path()).await;
    let origin = "http://localhost:8080";
    let app = build_router(
        state,
        &HttpConfig {
            bind_addr: "127.0.0.1:5523".parse().unwrap(),
            allowed_origins: vec![origin.parse().unwrap()],
            max_concurrent_requests: 128,
            request_timeout: std::time::Duration::from_secs(10),
            static_root: None,
        },
    );
    let response=app.oneshot(Request::builder().method("OPTIONS").uri(format!("/api/iac/native-operations/{}/recover",Uuid::new_v4())).header("origin",origin).header("access-control-request-method","POST").header("access-control-request-headers","x-firecrab-endpoint-id,x-firecrab-execution-generation,x-firecrab-operation-id").body(Body::empty()).unwrap()).await.unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.headers()["access-control-allow-origin"], origin);
    let allowed = response.headers()["access-control-allow-headers"]
        .to_str()
        .unwrap();
    for header in [
        "x-firecrab-endpoint-id",
        "x-firecrab-execution-generation",
        "x-firecrab-operation-id",
    ] {
        assert!(allowed.contains(header), "{allowed}");
    }
}

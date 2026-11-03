use super::*;

pub(super) async fn assert_resource_state(app: &Router, receipt: &Value, desired: Value) -> Value {
    let resource = &receipt["ownership"]["resource"];
    let path = format!(
        "/api/iac/resources/{}/{}",
        resource["kind"].as_str().unwrap(),
        resource["id"].as_str().unwrap()
    );
    let (code, state) = send(app, "GET", &path, Value::Null).await;
    assert_eq!(code, StatusCode::OK, "{state}");
    let expected = if desired.is_null() {
        Value::Null
    } else {
        serde_json::to_value(
            serde_json::from_value::<firecrab_stack::ResourceConfiguration>(desired).unwrap(),
        )
        .unwrap()
    };
    assert_eq!(state["configuration"], expected);
    assert_eq!(state["ownership"], receipt["ownership"]);
    assert_eq!(state["revisionId"], receipt["revisionId"]);
    assert_eq!(state["activeOperationIds"], json!([]));
    state
}

#[tokio::test]
async fn iac_resource_state_http_requires_operator_and_distinguishes_absence_from_read_failure() {
    let root = tempfile::tempdir().unwrap();
    let state = AppState::with_db_file(
        crate::templates::TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap(),
        root.path().join("state.db"),
    )
    .await
    .unwrap();
    let base = router(&state);
    let operator = base.clone().layer(Extension(LocalOperator));
    let remote = base.clone().layer(Extension(ConnectInfo(AuthenticatedHost {
        host_id: Uuid::new_v4(),
    })));
    for kind in firecrab_stack::RESOURCE_PLAN_KINDS {
        let path = format!("/api/iac/resources/{kind}/{}", Uuid::new_v4());
        assert_eq!(
            send(&base, "GET", &path, Value::Null).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            send(&remote, "GET", &path, Value::Null).await.0,
            StatusCode::FORBIDDEN
        );
    }
    assert!(!state.secrets.is_initialized());
    let response = operator
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/iac/resources/shell/{}", Uuid::new_v4()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let (code, identity) = send(&operator, "GET", "/api/iac/identity", Value::Null).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(
        identity["resourceObservationKinds"],
        identity["resourcePlanKinds"]
    );
    for kind in firecrab_stack::RESOURCE_PLAN_KINDS {
        let path = format!("/api/iac/resources/{kind}/{}", Uuid::new_v4());
        let (code, view) = send(&operator, "GET", &path, Value::Null).await;
        assert_eq!(code, StatusCode::OK, "{view}");
        assert!(view["configuration"].is_null());
        assert!(view["ownership"]["owner"].is_null());
        assert_eq!(view["ownership"]["generation"], 0);
    }
    // An unreadable Vault is an error, never a confirmed missing Secret.
    std::fs::write(
        root.path().join("state.secrets/vault.bin"),
        b"corrupt-private-storage-canary",
    )
    .unwrap();
    let (code, error) = send(
        &operator,
        "GET",
        &format!("/api/iac/resources/secret/{}", Uuid::new_v4()),
        Value::Null,
    )
    .await;
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE, "{error}");
    assert!(!error.to_string().contains("corrupt-private-storage-canary"));
    // SQL-only refresh never opens unrelated credential storage.
    let (code, _) = send(
        &operator,
        "GET",
        &format!("/api/iac/resources/shell/{}", Uuid::new_v4()),
        Value::Null,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
}

use super::*;
use crate::persistence::{PersistenceError, Store};
use firecrab_api_types::MicroServiceResponse;

fn service(state: &AppState) -> Uuid {
    let id = Uuid::new_v4();
    state.store.insert_micro_service(&MicroServiceResponse {id, spec:serde_json::from_value(json!({"name":format!("service-{}",id.simple()),"image":"ghcr.io/team/app:1","port":8080,"cpu":1,"ram":512})).unwrap()}).unwrap();
    id
}
fn slot(id: Uuid) -> ManagedResource {
    ManagedResource {
        kind: "service_otlp".into(),
        id: id.to_string(),
    }
}

#[tokio::test]
async fn iac_otlp_transfer_preserves_opt_in_boundary_and_blocks_every_direct_write() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let app = app.layer(axum::Extension(crate::ai_sandbox::access::LocalOperator));
    let id = service(&state);
    let original = "http://127.0.0.1:4318/";
    state.store.set_otlp_endpoint(id, Some(original)).unwrap();
    let before = state.store.otlp_configuration(id).unwrap();
    let claim = claim_configuration(&state, &slot(id));
    let (status, owned) = call(&app, "POST", "/api/iac/owners/transition", claim).await;
    assert_eq!(status, StatusCode::OK, "{owned}");
    for endpoint in [
        json!(original),
        json!("http://127.0.0.1:4320/"),
        Value::Null,
    ] {
        let (status, error) = call(
            &app,
            "PUT",
            &format!("/api/micro-services/{id}/observability/otlp"),
            json!({"endpoint":endpoint}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["error"]["code"], "iac_resource_managed");
    }
    assert_eq!(state.store.otlp_configuration(id).unwrap(), before);
    assert!(matches!(
        state.store.set_otlp_endpoint(id, None),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    let (status, view) = call(
        &app,
        "GET",
        &format!("/api/micro-services/{id}/observability"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["otlp"]["endpoint"], original);
    let (status, error) = call(
        &app,
        "DELETE",
        &format!("/api/micro-services/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "iac_dependent_managed");
    assert!(state.store.micro_service(id).unwrap().is_some());
    let release = json!({"operationId":Uuid::new_v4(),"expectedEndpointId":owned["endpointId"],"resource":owned["resource"],"expectedGeneration":owned["generation"],"expectedOwner":owned["owner"],"owner":null});
    assert_eq!(
        call(&app, "POST", "/api/iac/owners/transition", release)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("/api/micro-services/{id}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn iac_otlp_disabled_slot_can_be_managed_without_enabling_export() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let app = app.layer(axum::Extension(crate::ai_sandbox::access::LocalOperator));
    let id = service(&state);
    let claim = claim_configuration(&state, &slot(id));
    assert_eq!(
        call(&app, "POST", "/api/iac/owners/transition", claim)
            .await
            .0,
        StatusCode::OK
    );
    let (status, error) = call(
        &app,
        "PUT",
        &format!("/api/micro-services/{id}/observability/otlp"),
        json!({"endpoint":null}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert!(state.store.otlp_endpoint(id).unwrap().is_none());
    let unknown = claim_configuration(&state, &slot(Uuid::new_v4()));
    assert_eq!(
        call(&app, "POST", "/api/iac/owners/transition", unknown)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn iac_otlp_late_save_cannot_cross_an_ownership_release_and_reclaim() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let id = service(&state);
    let target = slot(id);
    let before = state.store.iac_resource_owner(&target).unwrap();
    state
        .store
        .set_otlp_endpoint_checked(id, Some("http://127.0.0.1:4318"), &before)
        .unwrap();
    let original = state.store.otlp_configuration(id).unwrap();
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &target)).unwrap();
    let owned = state.store.change_iac_resource_owner(&claim).unwrap();
    let second = Store::open(&root.path().join("state.db")).unwrap();
    second
        .change_iac_resource_owner(&ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: owned.endpoint_id,
            resource: target.clone(),
            expected_generation: owned.generation,
            expected_owner: owned.owner,
            owner: None,
        })
        .unwrap();
    assert!(matches!(
        second.set_otlp_endpoint_checked(id, None, &before),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert_eq!(second.otlp_configuration(id).unwrap(), original);
    let stack = state.store.register_stack("observer").unwrap();
    let current = state.store.iac_resource_owner(&target).unwrap();
    state
        .store
        .change_iac_resource_owner(&ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: current.endpoint_id,
            resource: target.clone(),
            expected_generation: current.generation,
            expected_owner: current.owner.clone(),
            owner: Some(firecrab_stack::ResourceOwner {
                manager: firecrab_stack::Manager::Stack,
                id: stack.id,
                key: "telemetry".into(),
            }),
        })
        .unwrap();
    assert!(matches!(
        second.set_otlp_endpoint_checked(id, Some("http://127.0.0.1:4320"), &current),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert_eq!(second.otlp_configuration(id).unwrap(), original);
}

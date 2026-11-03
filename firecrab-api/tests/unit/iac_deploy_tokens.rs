use super::*;
use crate::persistence::{PersistenceError, Store};
use firecrab_api_types::{DeployToken, MicroServiceResponse};
use firecrab_stack::{NativeAction, NativeOperationState};

fn credential(state: &AppState) -> DeployToken {
    let service = Uuid::new_v4();
    state.store.insert_micro_service(&MicroServiceResponse{id:service,spec:serde_json::from_value(json!({"name":format!("app-{}",service.simple()),"image":"ghcr.io/team/app:1","port":8080,"cpu":1,"ram":512})).unwrap()}).unwrap();
    let token = DeployToken {
        id: Uuid::new_v4(),
        service_id: service,
        name: "release".into(),
        repository: "ghcr.io/team/app".into(),
        created_at: 100,
        expires_at: 1000,
    };
    state.store.insert_deploy_token(&token, &[7; 32]).unwrap();
    token
}
fn resource(token: &DeployToken) -> ManagedResource {
    ManagedResource {
        kind: "service_deploy_token".into(),
        id: format!("{}:{}", token.service_id, token.id),
    }
}

#[tokio::test]
async fn iac_deploy_token_transfer_retains_verifier_expiry_and_operator_revocation_invalidates_authority()
 {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let app = app.layer(axum::Extension(crate::ai_sandbox::access::LocalOperator));
    let token = credential(&state);
    let target = resource(&token);
    let claim = claim_configuration(&state, &target);
    let (status, owned) = call(&app, "POST", "/api/iac/owners/transition", claim).await;
    assert_eq!(status, StatusCode::OK, "{owned}");
    let saved = state.store.deploy_token(token.id).unwrap().unwrap();
    assert_eq!(saved.metadata, token);
    assert_eq!(saved.hash, [7; 32]);
    let id = Uuid::new_v4();
    let path = format!(
        "/api/micro-services/{}/deploy-tokens/{}",
        token.service_id, token.id
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(&path)
                .header("x-firecrab-operation-id", id.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response.headers()["x-firecrab-operation-id"],
        id.to_string()
    );
    assert!(state.store.deploy_token(token.id).unwrap().is_none());
    let released = state.store.iac_resource_owner(&target).unwrap();
    assert!(released.owner.is_none());
    assert_eq!(
        released.generation,
        owned["generation"].as_u64().unwrap() + 1
    );
    let receipt = state.store.iac_native_operation(id).unwrap().unwrap();
    assert_eq!(receipt.action, NativeAction::RevokeDeployToken);
    assert_eq!(receipt.state, NativeOperationState::Succeeded);
    assert_eq!(receipt.ownership.resource, target);
    assert_eq!(serde_json::to_value(&receipt.ownership).unwrap(), owned);
    assert!(!serde_json::to_string(&receipt).unwrap().contains("hash"));
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/api/iac/native-operations/{id}/recover"),
            Value::Null
        )
        .await
        .1,
        serde_json::to_value(&receipt).unwrap()
    );
    assert!(matches!(
        state.store.insert_deploy_token(&token, &[7; 32]),
        Err(PersistenceError::IacOperationIdentity)
    ));
    assert_eq!(
        state
            .store
            .revoke_deploy_token_with_receipt(token.service_id, token.id, id)
            .unwrap(),
        receipt
    );
    assert_eq!(state.store.iac_resource_owner(&target).unwrap(), released);
    assert!(
        state
            .store
            .micro_service(token.service_id)
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn iac_deploy_token_scope_is_exact_and_parent_deletion_cannot_bypass_managed_credentials() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let token = credential(&state);
    let target = resource(&token);
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &target)).unwrap();
    let owned = state.store.change_iac_resource_owner(&claim).unwrap();
    let wrong = Uuid::new_v4();
    let malformed = ManagedResource {
        kind: target.kind.clone(),
        id: token.id.to_string(),
    };
    assert!(state.store.iac_resource_owner(&malformed).is_err());
    let wrong_target = ManagedResource {
        kind: target.kind.clone(),
        id: format!("{wrong}:{}", token.id),
    };
    let request: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &wrong_target)).unwrap();
    assert!(matches!(
        state.store.change_iac_resource_owner(&request),
        Err(PersistenceError::InvalidStackResource)
    ));
    state
        .store
        .revoke_deploy_token_with_receipt(wrong, token.id, Uuid::new_v4())
        .unwrap();
    assert!(state.store.deploy_token(token.id).unwrap().is_some());
    assert_eq!(state.store.iac_resource_owner(&target).unwrap(), owned);
    let (status, error) = call(
        &app,
        "DELETE",
        &format!("/api/micro-services/{}", token.service_id),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "iac_dependent_managed");
    state
        .store
        .revoke_deploy_token_with_receipt(token.service_id, token.id, Uuid::new_v4())
        .unwrap();
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("/api/micro-services/{}", token.service_id),
            Value::Null
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn iac_deploy_token_failed_receipt_rolls_back_revocation_and_preserves_credentials() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let token = credential(&state);
    let target = resource(&token);
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &target)).unwrap();
    let owned = state.store.change_iac_resource_owner(&claim).unwrap();
    let conn = rusqlite::Connection::open(root.path().join("state.db")).unwrap();
    conn.execute_batch("CREATE TRIGGER fail_token_receipt BEFORE INSERT ON iac_native_operations BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    let id = Uuid::new_v4();
    assert!(
        state
            .store
            .revoke_deploy_token_with_receipt(token.service_id, token.id, id)
            .is_err()
    );
    assert_eq!(state.store.iac_resource_owner(&target).unwrap(), owned);
    assert_eq!(
        state.store.deploy_token(token.id).unwrap().unwrap().hash,
        [7; 32]
    );
    assert!(state.store.iac_native_operation(id).unwrap().is_none());
}

#[tokio::test]
async fn iac_deploy_token_late_issuance_cannot_cross_parent_ownership_or_prior_revocation() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let token = credential(&state);
    let parent = ManagedResource {
        kind: "service".into(),
        id: token.service_id.to_string(),
    };
    let before = state.store.iac_resource_owner(&parent).unwrap();
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &parent)).unwrap();
    state.store.change_iac_resource_owner(&claim).unwrap();
    let second = Store::open(&root.path().join("state.db")).unwrap();
    let mut late = token.clone();
    late.id = Uuid::new_v4();
    assert!(matches!(
        second.insert_deploy_token_checked(&late, &[9; 32], &before),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert!(second.deploy_token(late.id).unwrap().is_none());
    let mut next = token.clone();
    next.id = Uuid::new_v4();
    second
        .revoke_deploy_token_with_receipt(next.service_id, next.id, Uuid::new_v4())
        .unwrap();
    // Revocation retires the reserved identity even if its issuance had not committed.
    let owned = state.store.iac_resource_owner(&parent).unwrap();
    state
        .store
        .change_iac_resource_owner(&ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: owned.endpoint_id,
            resource: parent.clone(),
            expected_generation: owned.generation,
            expected_owner: owned.owner,
            owner: None,
        })
        .unwrap();
    let current = state.store.iac_resource_owner(&parent).unwrap();
    assert!(matches!(
        second.insert_deploy_token_checked(&next, &[9; 32], &current),
        Err(PersistenceError::IacOperationIdentity)
    ));
}

#[tokio::test]
async fn iac_deploy_token_operator_revocation_supersedes_active_stack_work_without_granting_unverified_access()
 {
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let token = credential(&state);
    let target = resource(&token);
    let stack = state.store.register_stack("pipeline").unwrap();
    state
        .store
        .bind_stack_resource(
            &stack.name,
            &Binding {
                kind: ResourceKind::Network,
                key: "app".into(),
                resource_id: network,
            },
        )
        .unwrap();
    let mut claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &target)).unwrap();
    claim.owner = Some(firecrab_stack::ResourceOwner {
        manager: firecrab_stack::Manager::Stack,
        id: stack.id,
        key: "release-token".into(),
    });
    let owned = state.store.change_iac_resource_owner(&claim).unwrap();
    let stack = state.store.stack("pipeline").unwrap().unwrap();
    let status:firecrab_stack::ApplyStatus=serde_json::from_value(json!({"operationId":Uuid::new_v4(),"stackId":stack.id,"name":stack.name,"state":"queued","changes":[{"kind":"network","key":"app","resourceId":network,"action":"update","fields":["internetEnabled"],"reason":null,"state":"pending"}],"error":null})).unwrap();
    state
        .store
        .begin_stack_apply(&stack, "queued-network-update", false, &status, None)
        .unwrap();
    let path = format!(
        "/api/micro-services/{}/deploy-tokens/{}",
        token.service_id, token.id
    );
    assert_eq!(
        call(&app, "DELETE", &path, Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(state.store.iac_resource_owner(&target).unwrap(), owned);
    assert!(state.store.deploy_token(token.id).unwrap().is_some());
    let operator = app.layer(axum::Extension(crate::ai_sandbox::access::LocalOperator));
    assert_eq!(
        call(&operator, "DELETE", &path, Value::Null).await.0,
        StatusCode::NO_CONTENT
    );
    assert!(state.store.deploy_token(token.id).unwrap().is_none());
    assert!(
        state
            .store
            .iac_resource_owner(&target)
            .unwrap()
            .owner
            .is_none()
    );
    assert_eq!(
        state.store.stack("pipeline").unwrap().unwrap().revision,
        stack.revision + 1
    );
    assert_eq!(
        state
            .store
            .stored_stack_apply(status.operation_id)
            .unwrap()
            .unwrap()
            .status,
        status
    );
    assert!(state.store.micro_network(network).unwrap().is_some());
}

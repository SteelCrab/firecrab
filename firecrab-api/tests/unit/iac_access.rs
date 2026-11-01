use super::*;
use crate::persistence::{PersistenceError, Store};
use firecrab_api_types::SandboxAccessPolicy;
use firecrab_stack::{NativeAction, NativeOperationState};

#[tokio::test]
async fn iac_access_revocation_finishes_after_its_http_waiter_is_dropped() {
    use std::{future::Future, task::Poll};
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let principal = Uuid::new_v4();
    state
        .store
        .save_sandbox_access(principal, &policy())
        .unwrap();
    let blocked = rusqlite::Connection::open(root.path().join("state.db")).unwrap();
    blocked.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut waiter = Box::pin(crate::handlers::ai_sandbox::revoke_access(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Default::default(),
        axum::extract::Path(principal),
    ));
    assert!(
        std::future::poll_fn(|cx| Poll::Ready(matches!(waiter.as_mut().poll(cx), Poll::Pending)))
            .await
    );
    drop(waiter);
    assert!(
        state.sandbox.admission.try_lock().is_err(),
        "the admitted SQLite worker retains authority after disconnect"
    );
    blocked.execute_batch("ROLLBACK").unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if state.sandbox.admission.try_lock().is_ok() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        state
            .store
            .sandbox_access(principal)
            .unwrap()
            .grants
            .is_empty()
    );
    let operations = state
        .store
        .iac_native_operations(&resource(principal))
        .unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].state, NativeOperationState::Succeeded);
}

#[tokio::test]
async fn iac_access_delegated_host_cannot_mutate_or_revoke_an_owned_policy() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let principal = Uuid::new_v4();
    let policy = policy();
    state.store.save_sandbox_access(principal, &policy).unwrap();
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &resource(principal))).unwrap();
    state.store.change_iac_resource_owner(&claim).unwrap();
    for (method, path, body) in [
        ("GET", "/api/iac/owners".into(), Value::Null),
        (
            "PUT",
            format!("/api/ai-sandbox/access/{principal}"),
            json!({"grants":[]}),
        ),
        (
            "DELETE",
            format!("/api/ai-sandbox/access/{principal}"),
            Value::Null,
        ),
    ] {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("x-firecrab-caller", "operator")
            .body(Body::from(body.to_string()))
            .unwrap();
        request.extensions_mut().insert(axum::extract::ConnectInfo(
            crate::tls::AuthenticatedHost { host_id: principal },
        ));
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(state.store.sandbox_access(principal).unwrap(), policy);
    assert!(
        state
            .store
            .iac_native_operations(&resource(principal))
            .unwrap()
            .is_empty()
    );
}

fn policy() -> SandboxAccessPolicy {
    serde_json::from_value(json!({"grants":[{"resource":{"kind":"project","id":Uuid::new_v4()},"permissions":["read"]}]})).unwrap()
}

fn resource(principal: Uuid) -> ManagedResource {
    ManagedResource {
        kind: "sandbox_access".into(),
        id: principal.to_string(),
    }
}

#[tokio::test]
async fn iac_access_owned_policy_blocks_grants_but_operator_can_revoke_with_a_durable_receipt() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let app = app.layer(axum::Extension(crate::ai_sandbox::access::LocalOperator));
    let principal = Uuid::new_v4();
    let policy = policy();
    let path = format!("/api/ai-sandbox/access/{principal}");
    assert_eq!(
        call(&app, "PUT", &path, serde_json::to_value(&policy).unwrap())
            .await
            .0,
        StatusCode::OK
    );
    let before = state.store.sandbox_access_authority(principal).unwrap();
    let claim = claim_configuration(&state, &resource(principal));
    let (status, owned) = call(&app, "POST", "/api/iac/owners/transition", claim.clone()).await;
    assert_eq!(status, StatusCode::OK, "{owned}");
    assert_eq!(state.store.sandbox_access(principal).unwrap(), policy);
    let (status, error) = call(&app, "PUT", &path, serde_json::to_value(&policy).unwrap()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "iac_ownership_conflict");
    assert!(matches!(
        state
            .store
            .save_sandbox_access_checked(principal, &policy, &before),
        Err(PersistenceError::StaleIacOwner)
    ));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(&path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let id = Uuid::parse_str(
        response.headers()["x-firecrab-operation-id"]
            .to_str()
            .unwrap(),
    )
    .unwrap();
    let operation = state.store.iac_native_operation(id).unwrap().unwrap();
    assert_eq!(operation.action, NativeAction::RevokeSandboxAccess);
    assert_eq!(operation.state, NativeOperationState::Succeeded);
    assert_eq!(serde_json::to_value(&operation.ownership).unwrap(), owned);
    let after = state.store.sandbox_access_authority(principal).unwrap();
    assert_eq!(after.generation, 2);
    assert!(after.owner.is_none());
    assert!(
        state
            .store
            .sandbox_access(principal)
            .unwrap()
            .grants
            .is_empty()
    );
    assert!(state.store.sandbox_delegate(principal).unwrap());
    assert!(state.store.micro_network(network).unwrap().is_some());
    assert_eq!(
        call(&app, "POST", "/api/iac/owners/transition", claim)
            .await
            .1,
        owned,
        "lost acknowledgement returns its old receipt without regranting"
    );
    assert_eq!(
        state.store.sandbox_access_authority(principal).unwrap(),
        after
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/api/iac/native-operations/{id}/recover"),
            Value::Null
        )
        .await
        .1,
        serde_json::to_value(&operation).unwrap()
    );
}

#[tokio::test]
async fn iac_access_empty_put_releases_stack_authority_without_removing_its_registration() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let app = app.layer(axum::Extension(crate::ai_sandbox::access::LocalOperator));
    let principal = Uuid::new_v4();
    state
        .store
        .save_sandbox_access(principal, &policy())
        .unwrap();
    let stack = state.store.register_stack("access-policy").unwrap();
    let mut claim = claim_configuration(&state, &resource(principal));
    claim["owner"] = json!({"manager":"stack","id":stack.id,"key":"policy"});
    assert_eq!(
        call(&app, "POST", "/api/iac/owners/transition", claim)
            .await
            .0,
        StatusCode::OK
    );
    let before = state.store.stack(&stack.name).unwrap().unwrap();
    let (status, empty) = call(
        &app,
        "PUT",
        &format!("/api/ai-sandbox/access/{principal}"),
        json!({"grants":[]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{empty}");
    assert_eq!(empty, json!({"grants":[]}));
    assert_eq!(
        state.store.stack(&stack.name).unwrap().unwrap().revision,
        before.revision + 1
    );
    assert!(state.store.list_iac_resource_owners().unwrap().is_empty());
    assert_eq!(
        state
            .store
            .iac_native_operations(&resource(principal))
            .unwrap()
            .len(),
        1
    );
    state
        .store
        .delete_empty_stack(&stack.name, stack.id)
        .unwrap();
    assert!(state.store.sandbox_delegate(principal).unwrap());
}

#[tokio::test]
async fn iac_access_revocation_invalidates_late_saves_and_old_retries_preserve_new_grants_after_restart()
 {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let principal = Uuid::new_v4();
    let policy = policy();
    state.store.save_sandbox_access(principal, &policy).unwrap();
    let late = state.store.sandbox_access_authority(principal).unwrap();
    let other = Store::open(&root.path().join("state.db")).unwrap();
    let id = Uuid::new_v4();
    let revoked = other.revoke_sandbox_access(principal, id).unwrap();
    assert!(matches!(
        state
            .store
            .save_sandbox_access_checked(principal, &policy, &late),
        Err(PersistenceError::StaleIacOwner)
    ));
    let after = state.store.sandbox_access_authority(principal).unwrap();
    assert_eq!(after.generation, 1, "unowned authority is invalidated too");
    drop(other);
    let restarted = Store::open(&root.path().join("state.db")).unwrap();
    assert_eq!(
        restarted.iac_native_operation(id).unwrap(),
        Some(revoked.clone())
    );
    restarted
        .save_sandbox_access_checked(principal, &policy, &after)
        .unwrap();
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &resource(principal))).unwrap();
    let owned = restarted.change_iac_resource_owner(&claim).unwrap();
    assert_eq!(
        restarted.revoke_sandbox_access(principal, id).unwrap(),
        revoked
    );
    assert_eq!(restarted.sandbox_access(principal).unwrap(), policy);
    assert_eq!(
        restarted.sandbox_access_authority(principal).unwrap(),
        owned
    );
    assert!(matches!(
        restarted.revoke_sandbox_access(Uuid::new_v4(), id),
        Err(PersistenceError::IacOperationIdentity)
    ));
}

#[tokio::test]
async fn iac_access_failed_receipt_commit_keeps_permissions_and_owner_together() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let principal = Uuid::new_v4();
    let policy = policy();
    state.store.save_sandbox_access(principal, &policy).unwrap();
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &resource(principal))).unwrap();
    let owned = state.store.change_iac_resource_owner(&claim).unwrap();
    let fault = rusqlite::Connection::open(root.path().join("state.db")).unwrap();
    fault.execute_batch("CREATE TRIGGER fail_access_receipt BEFORE INSERT ON iac_native_operations WHEN NEW.kind='sandbox_access' BEGIN SELECT RAISE(ABORT,'test transaction failure'); END;").unwrap();
    let id = Uuid::new_v4();
    assert!(state.store.revoke_sandbox_access(principal, id).is_err());
    assert_eq!(state.store.sandbox_access(principal).unwrap(), policy);
    assert_eq!(
        state.store.sandbox_access_authority(principal).unwrap(),
        owned
    );
    assert!(state.store.iac_native_operation(id).unwrap().is_none());
    fault
        .execute_batch("DROP TRIGGER fail_access_receipt")
        .unwrap();
    state.store.revoke_sandbox_access(principal, id).unwrap();
    assert!(
        state
            .store
            .sandbox_access(principal)
            .unwrap()
            .grants
            .is_empty()
    );
}

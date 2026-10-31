use super::*;
use crate::pipe_auth::{AwsCredentials, Provider, SavedAuth};

#[tokio::test]
async fn iac_auth_provider_slots_transfer_without_exposing_or_replacing_credentials() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let stack = state.store.register_stack("auth-ownership").unwrap();
    for auth in [
        SavedAuth::Github {
            username: "fixture-user".into(),
            token: "fixture-auth-never-echo".into(),
        },
        SavedAuth::Firecrab {
            account_id: Uuid::new_v4().to_string(),
            email: "fixture@example.invalid".into(),
            expires_at: 4102444800000,
            token: "fixture-auth-never-echo".into(),
        },
        SavedAuth::Aws {
            account_id: "123456789012".into(),
            credentials: AwsCredentials {
                access_key_id: "fixture-access".into(),
                secret_access_key: "fixture-auth-never-echo".into(),
                session_token: String::new(),
                region: "us-east-1".into(),
            },
        },
    ] {
        state.store.put_pipe_auth(&auth).unwrap();
        let provider = auth.connection().provider;
        let resource = ManagedResource {
            kind: "auth".into(),
            id: provider.as_str().into(),
        };
        let mut native = claim_configuration(&state, &resource);
        native["owner"] = json!({"manager":"stack","id":stack.id,"key":provider.as_str()});
        let (status, native) = call(&app, "POST", "/api/iac/owners/transition", native).await;
        assert_eq!(status, StatusCode::OK, "{native}");
        let transfer = claim_configuration(&state, &resource);
        let (status, owned) =
            call(&app, "POST", "/api/iac/owners/transition", transfer.clone()).await;
        assert_eq!(status, StatusCode::OK, "{owned}");
        assert_eq!(owned["generation"], 2);
        assert!(!owned.to_string().contains("fixture-auth-never-echo"));
        let encoded = provider
            .as_str()
            .chars()
            .map(|character| format!("%{:02X}", character as u32))
            .collect::<String>();
        for method in ["PUT", "DELETE"] {
            let (status, error) = call(
                &app,
                method,
                &format!("/api/pipe/auth/{encoded}"),
                json!({}),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT, "{error}");
            assert_eq!(error["error"]["code"], "iac_resource_managed");
        }
        // This bypasses the HTTP gate, as an already-dispatched credential save can.
        assert!(matches!(
            state.store.put_pipe_auth(&auth),
            Err(crate::persistence::PersistenceError::IacOwnershipConflict)
        ));
        assert!(matches!(
            state.store.delete_pipe_auth(provider),
            Err(crate::persistence::PersistenceError::IacOwnershipConflict)
        ));
        let saved = state.store.pipe_auths().unwrap();
        assert!(
            saved
                .iter()
                .any(|current| serde_json::to_value(current).unwrap()
                    == serde_json::to_value(&auth).unwrap())
        );
        let (status, public) = call(&app, "GET", "/api/pipe/auth", Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!public.to_string().contains("fixture-auth-never-echo"));
        let receipt = call(
            &app,
            "GET",
            &format!(
                "/api/iac/owner-changes/{}",
                transfer["operationId"].as_str().unwrap()
            ),
            Value::Null,
        )
        .await;
        assert_eq!(receipt.1["ownership"], owned);
        let (status,released)=call(&app,"POST","/api/iac/owners/transition",json!({
            "operationId":Uuid::new_v4(),"expectedEndpointId":owned["endpointId"],"resource":resource,
            "expectedGeneration":2,"expectedOwner":owned["owner"],"owner":null
        })).await;
        assert_eq!(status, StatusCode::OK, "{released}");
        assert_eq!(released["generation"], 3);
        state.store.delete_pipe_auth(provider).unwrap();
        assert!(
            state
                .store
                .pipe_auths()
                .unwrap()
                .iter()
                .all(|record| record.connection().provider != provider)
        );
    }
    assert!(
        state
            .store
            .stack("auth-ownership")
            .unwrap()
            .unwrap()
            .bindings
            .is_empty()
    );
}

#[tokio::test]
async fn iac_auth_requires_an_existing_exact_provider_slot() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let resource = ManagedResource {
        kind: "auth".into(),
        id: Provider::Github.as_str().into(),
    };
    let change = claim_configuration(&state, &resource);
    let operation = Uuid::parse_str(change["operationId"].as_str().unwrap()).unwrap();
    let (status, error) = call(&app, "POST", "/api/iac/owners/transition", change).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert!(
        state
            .store
            .iac_ownership_operation(operation)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        call(&app, "GET", "/api/iac/owners/auth/GitHub", Value::Null)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        state
            .store
            .iac_resource_owner(&resource)
            .unwrap()
            .generation,
        0
    );
}

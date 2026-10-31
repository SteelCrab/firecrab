use super::*;
use crate::persistence::PersistenceError;
use crate::pipe_ai::{OAuthSession, SavedConnection};
use firecrab_api_types::{AiAuthentication, AiConnection, AiModelSelection, AiProvider};

fn connection(state: &AppState) -> SavedConnection {
    let saved = SavedConnection {
        connection: AiConnection {
            id: Uuid::new_v4(),
            name: "fixture-ai".into(),
            provider: AiProvider::Openai,
            authentication: AiAuthentication::Chatgpt,
            endpoint: crate::pipe_ai::ENDPOINT.into(),
            models: vec!["fixture-model".into()],
            streaming: false,
            tool_calls: false,
        },
        api_key: "fixture-ai-never-echo".into(),
        oauth: Some(OAuthSession {
            client_id: "fixture-client".into(),
            subject: "fixture-subject".into(),
            refresh_token: "fixture-refresh-never-echo".into(),
            expires_at: 4102444800,
            earliest_refresh_at: 0,
            scope: "fixture-scope".into(),
        }),
    };
    state.store.save_ai_connection(&saved).unwrap();
    saved
}

#[tokio::test]
async fn iac_ai_connection_and_project_model_writes_obey_their_exact_owners() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let saved = connection(&state);
    let project = Uuid::new_v4();
    let selection = AiModelSelection {
        connection_id: saved.connection.id,
        model: "fixture-model".into(),
    };
    state.store.save_ai_default(project, &selection).unwrap();
    for (kind, id, routes) in [
        (
            "ai_connection",
            saved.connection.id,
            vec![
                (
                    "PUT",
                    format!("/api/pipe/ai/connections/{}", saved.connection.id),
                ),
                (
                    "DELETE",
                    format!("/api/pipe/ai/connections/{}", saved.connection.id),
                ),
                (
                    "POST",
                    format!("/api/pipe/ai/connections/{}/verify", saved.connection.id),
                ),
                (
                    "POST",
                    format!("/api/pipe/ai/connections/{}/login", saved.connection.id),
                ),
            ],
        ),
        (
            "ai_model",
            project,
            vec![("PUT", format!("/api/pipe/ai/projects/{project}/model"))],
        ),
    ] {
        let resource = ManagedResource {
            kind: kind.into(),
            id: id.to_string(),
        };
        let (status, owned) = call(
            &app,
            "POST",
            "/api/iac/owners/transition",
            claim_configuration(&state, &resource),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{owned}");
        assert!(!owned.to_string().contains("never-echo"));
        for (method, path) in routes {
            let path = path.replace(&id.to_string(), &id.to_string().replace('-', "%2D"));
            let (status, error) = call(&app, method, &path, json!({})).await;
            assert_eq!(status, StatusCode::CONFLICT, "{path}: {error}");
            assert_eq!(error["error"]["code"], "iac_resource_managed");
        }
    }
    assert!(matches!(
        state.store.save_ai_connection(&saved),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    assert!(matches!(
        state.store.delete_ai_connection(saved.connection.id),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    assert!(matches!(
        state.store.save_ai_default(project, &selection),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    let (status, metadata) = call(&app, "GET", "/api/pipe/ai/connections", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!metadata.to_string().contains("never-echo"));
    assert_eq!(state.store.ai_default(project).unwrap(), Some(selection));
}

#[tokio::test]
async fn iac_ai_oauth_refresh_preserves_configuration_and_cannot_undo_rotation_or_deletion() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let previous = connection(&state);
    let resource = ManagedResource {
        kind: "ai_connection".into(),
        id: previous.connection.id.to_string(),
    };
    let (status, owned) = call(
        &app,
        "POST",
        "/api/iac/owners/transition",
        claim_configuration(&state, &resource),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let mut refreshed = previous.clone();
    refreshed.api_key = "refreshed-private-access".into();
    refreshed.oauth.as_mut().unwrap().refresh_token = "refreshed-private-refresh".into();
    state
        .store
        .refresh_ai_connection(&previous, &refreshed)
        .unwrap();
    assert_eq!(
        state
            .store
            .ai_connection(previous.connection.id)
            .unwrap()
            .unwrap()
            .api_key,
        refreshed.api_key
    );
    assert_eq!(
        state
            .store
            .iac_resource_owner(&resource)
            .unwrap()
            .generation,
        1
    );
    assert!(matches!(
        state.store.refresh_ai_connection(&previous, &refreshed),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    let mut wrong_account = refreshed.clone();
    wrong_account.oauth.as_mut().unwrap().subject = "another-subject".into();
    assert!(matches!(
        state
            .store
            .refresh_ai_connection(&refreshed, &wrong_account),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    let mut wrong_config = refreshed.clone();
    wrong_config.connection.name = "renamed".into();
    assert!(matches!(
        state.store.refresh_ai_connection(&refreshed, &wrong_config),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    let released=call(&app,"POST","/api/iac/owners/transition",json!({
        "operationId":Uuid::new_v4(),"expectedEndpointId":owned["endpointId"],"resource":resource,
        "expectedGeneration":1,"expectedOwner":owned["owner"],"owner":null
    })).await;
    assert_eq!(released.0, StatusCode::OK);
    let mut rotated = refreshed.clone();
    rotated.api_key = "operator-rotated-private-access".into();
    state.store.save_ai_connection(&rotated).unwrap();
    assert!(matches!(
        state.store.refresh_ai_connection(&refreshed, &previous),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    state
        .store
        .delete_ai_connection(previous.connection.id)
        .unwrap();
    assert!(matches!(
        state.store.refresh_ai_connection(&rotated, &refreshed),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    assert!(
        state
            .store
            .ai_connection(previous.connection.id)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn iac_ai_managed_defaults_prevent_parent_deletion_and_model_removal() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let saved = connection(&state);
    let project = Uuid::new_v4();
    let selection = AiModelSelection {
        connection_id: saved.connection.id,
        model: "fixture-model".into(),
    };
    state.store.save_ai_default(project, &selection).unwrap();
    let resource = ManagedResource {
        kind: "ai_model".into(),
        id: project.to_string(),
    };
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/iac/owners/transition",
            claim_configuration(&state, &resource)
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut removed = saved.clone();
    removed.connection.models.clear();
    assert!(matches!(
        state.store.save_ai_connection(&removed),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    assert!(matches!(
        state.store.delete_ai_connection(saved.connection.id),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    let (status, error) = call(
        &app,
        "DELETE",
        &format!("/api/pipe/ai/connections/{}", saved.connection.id),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(state.store.ai_default(project).unwrap(), Some(selection));
    assert_eq!(
        state
            .store
            .ai_connection(saved.connection.id)
            .unwrap()
            .unwrap()
            .connection,
        saved.connection
    );
}

#[tokio::test]
async fn iac_ai_pending_login_blocks_transfer_without_recording_a_receipt() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let saved = connection(&state);
    state
        .pipe_ai_logins
        .begin(
            saved.connection.id,
            firecrab_api_types::AiLoginRequest {
                name: "pending-login".into(),
                model: None,
            },
            format!("urn:uuid:{}", Uuid::new_v4()),
            5523,
            None,
        )
        .unwrap();
    let resource = ManagedResource {
        kind: "ai_connection".into(),
        id: saved.connection.id.to_string(),
    };
    let change = claim_configuration(&state, &resource);
    let id = Uuid::parse_str(change["operationId"].as_str().unwrap()).unwrap();
    let (status, error) = call(&app, "POST", "/api/iac/owners/transition", change.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "iac_resource_busy");
    assert!(state.store.iac_ownership_operation(id).unwrap().is_none());
    state.pipe_ai_logins.cancel_connection(saved.connection.id);
    let (status, owned) = call(&app, "POST", "/api/iac/owners/transition", change.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(matches!(
        state.store.save_ai_connection(&saved),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    let released=call(&app,"POST","/api/iac/owners/transition",json!({
        "operationId":Uuid::new_v4(),"expectedEndpointId":owned["endpointId"],"resource":resource,
        "expectedGeneration":1,"expectedOwner":owned["owner"],"owner":null
    })).await;
    assert_eq!(released.0, StatusCode::OK);
    state
        .pipe_ai_logins
        .begin(
            saved.connection.id,
            firecrab_api_types::AiLoginRequest {
                name: "later-login".into(),
                model: None,
            },
            format!("urn:uuid:{}", Uuid::new_v4()),
            5523,
            None,
        )
        .unwrap();
    assert_eq!(
        call(&app, "POST", "/api/iac/owners/transition", change.clone())
            .await
            .1,
        owned
    );
    let mut conflict = change;
    conflict["owner"]["key"] = json!("different-address");
    let (status, error) = call(&app, "POST", "/api/iac/owners/transition", conflict).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "iac_operation_identity_conflict");
    let current = state.store.iac_resource_owner(&resource).unwrap();
    assert_eq!(current.generation, 2);
    assert!(current.owner.is_none());
}

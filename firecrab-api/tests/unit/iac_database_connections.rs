use super::*;
use crate::persistence::PersistenceError;

#[tokio::test]
async fn iac_database_connection_update_rejects_managed_credentials_before_remote_io() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, network) = fixture(root.path()).await;
    let resource = configuration_resource(&state, root.path(), network, "database_connection");
    let id = Uuid::parse_str(&resource.id).unwrap();
    let before = state.store.pipe_database(id).unwrap().unwrap();
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &resource)).unwrap();
    state.store.change_iac_resource_owner(&claim).unwrap();
    let mut input = before.input.clone();
    input.write_enabled = true;
    let (status, error) = call(
        &app,
        "PUT",
        &format!("/api/pipe/databases/{id}"),
        serde_json::to_value(&input).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "iac_ownership_conflict");
    assert!(matches!(
        state.store.update_pipe_database(id, &input),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    assert_eq!(
        serde_json::to_value(state.store.pipe_database(id).unwrap().unwrap().input).unwrap(),
        serde_json::to_value(before.input).unwrap()
    );
}

#[tokio::test]
async fn iac_database_connection_late_credential_validation_cannot_cross_owner_history() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, network) = fixture(root.path()).await;
    let resource = configuration_resource(&state, root.path(), network, "database_connection");
    let id = Uuid::parse_str(&resource.id).unwrap();
    let original = state.store.pipe_database(id).unwrap().unwrap();
    let authority = state.store.pipe_database_write_authority(id).unwrap();
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &resource)).unwrap();
    let owned = state.store.change_iac_resource_owner(&claim).unwrap();
    state
        .store
        .change_iac_resource_owner(&ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: owned.endpoint_id,
            resource: owned.resource,
            expected_generation: owned.generation,
            expected_owner: owned.owner,
            owner: None,
        })
        .unwrap();
    let second = crate::persistence::Store::open(&root.path().join("state.db")).unwrap();
    let mut changed = original.input.clone();
    changed.write_enabled = true;
    assert!(matches!(
        second.update_pipe_database_checked(id, &changed, &authority),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert_eq!(
        serde_json::to_value(second.pipe_database(id).unwrap().unwrap().input).unwrap(),
        serde_json::to_value(original.input).unwrap()
    );
}

use super::*;
use crate::persistence::PersistenceError;
use firecrab_api_types::{AutoDeployPolicy, MicroServiceResponse};

fn service(state: &AppState, enabled: bool) -> MicroServiceResponse {
    let id = Uuid::new_v4();
    let mut record: MicroServiceResponse = serde_json::from_value(json!({"id":id,"name":format!("app-{}",id.simple()),"image":"ghcr.io/team/app:1","port":8080,"cpu":1,"ram":512})).unwrap();
    if enabled {
        record.spec.auto_deploy = Some(AutoDeployPolicy {
            reference: "ghcr.io/team/app:stable".into(),
            interval_seconds: 60,
        });
    }
    state.store.insert_micro_service(&record).unwrap();
    record
}
fn slot(id: Uuid) -> ManagedResource {
    ManagedResource {
        kind: "service_auto_deploy".into(),
        id: id.to_string(),
    }
}
fn release(state: &AppState, owned: firecrab_stack::ResourceOwnership) {
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
}

#[tokio::test]
async fn iac_auto_deploy_transfer_retains_policy_and_blocks_all_public_controls() {
    let root = tempfile::tempdir().unwrap();
    let (state, app, _) = fixture(root.path()).await;
    let record = service(&state, true);
    let before = state.store.tag_watch(record.id).unwrap().unwrap();
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &slot(record.id))).unwrap();
    let owned = state.store.change_iac_resource_owner(&claim).unwrap();
    for (method, path, body) in [
        (
            "PUT",
            format!("/api/micro-services/{}/auto-deploy", record.id),
            Value::Null,
        ),
        (
            "PUT",
            format!("/api/micro-services/{}/auto-deploy", record.id),
            json!(record.spec.auto_deploy),
        ),
        (
            "POST",
            format!("/api/micro-services/{}/auto-deploy/pause", record.id),
            Value::Null,
        ),
        (
            "POST",
            format!("/api/micro-services/{}/auto-deploy/resume", record.id),
            Value::Null,
        ),
    ] {
        let (status, error) = call(&app, method, &path, body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{path}: {error}");
        assert_eq!(error["error"]["code"], "iac_ownership_conflict");
    }
    let after = state.store.tag_watch(record.id).unwrap().unwrap();
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.status, before.status);
    let (status, view) = call(
        &app,
        "GET",
        &format!("/api/micro-services/{}/auto-deploy", record.id),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert!(view["error"].as_str().unwrap().contains("suspended"));
    assert_eq!(view["policy"], json!(record.spec.auto_deploy));
    assert!(matches!(
        state.store.delete_micro_service(record.id),
        Err(PersistenceError::IacManagedDependent)
    ));
    let mut changed = record.clone();
    changed.spec.auto_deploy = None;
    assert!(matches!(
        state.store.update_micro_service(&changed),
        Err(PersistenceError::IacOwnershipConflict)
    ));
    let mut other = record.clone();
    other.spec.ram = 1024;
    assert!(state.store.update_micro_service(&other).unwrap());
    assert_eq!(
        state
            .store
            .tag_watch(record.id)
            .unwrap()
            .unwrap()
            .generation,
        before.generation
    );
    release(&state, owned);
    assert!(state.store.update_micro_service(&changed).unwrap());
    assert!(state.store.tag_watch(record.id).unwrap().is_none());
}

#[tokio::test]
async fn iac_auto_deploy_disabled_slot_and_exact_service_identity() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let record = service(&state, false);
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &slot(record.id))).unwrap();
    state.store.change_iac_resource_owner(&claim).unwrap();
    assert!(state.store.tag_watch(record.id).unwrap().is_none());
    let missing: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &slot(Uuid::new_v4()))).unwrap();
    assert!(matches!(
        state.store.change_iac_resource_owner(&missing),
        Err(PersistenceError::InvalidStackResource)
    ));
    for id in ["app".to_owned(), Uuid::nil().to_string()] {
        let mut resource = slot(record.id);
        resource.id = id;
        assert!(resource.normalize().is_err());
    }
}

#[tokio::test]
async fn iac_auto_deploy_late_observation_and_policy_save_cannot_cross_release_reclaim() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let record = service(&state, true);
    let authority = state.store.auto_deploy_authority(record.id).unwrap();
    let watch = state.store.tag_watch(record.id).unwrap().unwrap();
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &slot(record.id))).unwrap();
    let owned = state.store.change_iac_resource_owner(&claim).unwrap();
    release(&state, owned);
    let mut late = watch.clone();
    late.status.next_check_at = 999;
    assert!(matches!(
        state
            .store
            .save_tag_watch_checked(record.id, &late, watch.generation, false),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert!(matches!(
        state.store.set_tag_policy(
            &record,
            None,
            Some(&watch),
            &authority,
            Some(watch.generation)
        ),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert!(matches!(
        state
            .store
            .update_micro_service_checked(&record, &authority),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert_eq!(
        state.store.tag_watch(record.id).unwrap().unwrap().status,
        watch.status
    );
    let reopened = crate::persistence::Store::open(&root.path().join("state.db")).unwrap();
    let saved = reopened.tag_watch(record.id).unwrap().unwrap();
    assert!(!reopened.automatic_watch_allowed(record.id, &saved).unwrap());
    let renewed = reopened.auto_deploy_authority(record.id).unwrap();
    let mut next = saved.clone();
    next.authority = Some(renewed);
    next.generation = Uuid::new_v4();
    reopened
        .save_tag_watch_checked(record.id, &next, saved.generation, true)
        .unwrap();
    assert!(reopened.automatic_watch_allowed(record.id, &next).unwrap());
    assert!(matches!(
        reopened.save_tag_watch_checked(record.id, &late, watch.generation, false),
        Err(PersistenceError::StaleIacOwner)
    ));
}

#[tokio::test]
async fn iac_auto_deploy_legacy_watch_without_authority_cannot_gain_a_new_owners_image_writer() {
    let root = tempfile::tempdir().unwrap();
    let (state, _, _) = fixture(root.path()).await;
    let record = service(&state, true);
    let watch = state.store.tag_watch(record.id).unwrap().unwrap();
    let mut legacy = serde_json::to_value(&watch).unwrap();
    legacy.as_object_mut().unwrap().remove("authority");
    let conn = rusqlite::Connection::open(root.path().join("state.db")).unwrap();
    conn.execute(
        "UPDATE service_tag_watches SET state=?2 WHERE service_id=?1",
        rusqlite::params![record.id.to_string(), legacy.to_string()],
    )
    .unwrap();
    let claim: ChangeResourceOwner =
        serde_json::from_value(claim_configuration(&state, &slot(record.id))).unwrap();
    state.store.change_iac_resource_owner(&claim).unwrap();
    let saved = state.store.tag_watch(record.id).unwrap().unwrap();
    assert!(saved.authority.is_none());
    assert!(
        !state
            .store
            .automatic_watch_allowed(record.id, &saved)
            .unwrap()
    );
    state.store.save_tag_watch(record.id, &saved).unwrap();
    let saved = state.store.tag_watch(record.id).unwrap().unwrap();
    assert!(saved.authority.is_none());
    assert!(
        !state
            .store
            .automatic_watch_allowed(record.id, &saved)
            .unwrap()
    );
}

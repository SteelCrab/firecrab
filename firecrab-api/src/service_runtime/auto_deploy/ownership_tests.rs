use super::*;
use crate::persistence::{PersistenceError, iac_auto_deploy::DeploymentAuthority};
use firecrab_stack::{ChangeResourceOwner, ManagedResource, Manager, ResourceOwner};

fn transfer(
    state: &AppState,
    id: Uuid,
    kind: &str,
) -> Result<firecrab_stack::ResourceOwnership, PersistenceError> {
    let resource = ManagedResource {
        kind: kind.into(),
        id: id.to_string(),
    };
    let old = state.store.iac_resource_owner(&resource)?;
    state.store.change_iac_resource_owner(&ChangeResourceOwner {
        operation_id: Uuid::new_v4(),
        expected_endpoint_id: old.endpoint_id,
        resource,
        expected_generation: old.generation,
        expected_owner: old.owner,
        owner: Some(ResourceOwner {
            manager: Manager::Terraform,
            id: Uuid::new_v4(),
            key: "firecrab_service.test".into(),
        }),
    })
}

#[tokio::test]
async fn iac_auto_deploy_registry_result_and_private_queue_claim_are_fenced_after_transfer() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let current = running();
    insert(&state, &current);
    let original = state.store.tag_watch(current.service_id).unwrap().unwrap();
    let owned = transfer(&state, current.service_id, "service_auto_deploy").unwrap();
    let late = finish_observation(
        &state,
        &current,
        &original,
        Ok(("stable".into(), digest('b'))),
        1,
    )
    .await;
    assert!(late.is_err());
    let saved = state.store.tag_watch(current.service_id).unwrap().unwrap();
    assert_eq!(saved.status, original.status);
    assert!(saved.pending.is_none());
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
    let mut claim = original.clone();
    observe(&mut claim, &current, Ok(("stable".into(), digest('b'))), 1);
    let mut queued = current.clone();
    queued.id = claim.pending.as_ref().unwrap().operation;
    queued.phase = ServicePhase::Queued;
    queued.spec.image = format!("ghcr.io/team/app@{}", digest('b'));
    assert!(matches!(
        state
            .store
            .save_authorized_deployment(&queued, &DeploymentAuthority::Automatic(Box::new(claim))),
        Err(PersistenceError::StaleIacOwner)
    ));
    assert!(state.store.service_attempt(queued.id).unwrap().is_none());
    assert_eq!(
        state
            .store
            .service_deployment(current.service_id)
            .unwrap()
            .unwrap()
            .id,
        current.id
    );
}

#[tokio::test]
async fn iac_auto_deploy_pending_identity_blocks_parent_and_policy_handover() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let current = running();
    insert(&state, &current);
    let mut watch = state.store.tag_watch(current.service_id).unwrap().unwrap();
    observe(&mut watch, &current, Ok(("stable".into(), digest('b'))), 1);
    state
        .store
        .save_tag_watch(current.service_id, &watch)
        .unwrap();
    for kind in ["service", "service_auto_deploy"] {
        assert!(matches!(
            transfer(&state, current.service_id, kind),
            Err(PersistenceError::IacResourceBusy)
        ));
    }
    let pending = watch.pending.as_ref().unwrap().operation;
    assert_eq!(
        state
            .store
            .tag_watch(current.service_id)
            .unwrap()
            .unwrap()
            .pending
            .unwrap()
            .operation,
        pending
    );
}

#[tokio::test]
async fn iac_auto_deploy_new_parent_owner_does_not_inherit_an_implicit_image_delegation() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let current = running();
    insert(&state, &current);
    let original = state.store.tag_watch(current.service_id).unwrap().unwrap();
    transfer(&state, current.service_id, "service").unwrap();
    assert!(
        !state
            .store
            .automatic_watch_allowed(current.service_id, &original)
            .unwrap()
    );
    check(&state, current.clone()).await.unwrap();
    let Json(status) = get(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(current.service_id.to_string()),
    )
    .await
    .unwrap();
    assert_eq!(status.policy, current.spec.auto_deploy);
    assert!(status.error.unwrap().contains("suspended"));
    assert!(
        state
            .store
            .tag_watch(current.service_id)
            .unwrap()
            .unwrap()
            .pending
            .is_none()
    );
}

#[tokio::test]
async fn iac_auto_deploy_late_health_snapshot_preserves_current_policy_and_release_history() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let current = running();
    insert(&state, &current);
    let mut policy = policy();
    policy.interval_seconds = 600;
    let _ = configure(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(current.service_id.to_string()),
        ValidatedJson(Some(policy.clone())),
    )
    .await
    .unwrap();
    let watcher = state.store.tag_watch(current.service_id).unwrap().unwrap();
    transfer(&state, current.service_id, "service_auto_deploy").unwrap();
    let mut late = current.clone();
    late.log.push("late health observation".into());
    state.store.save_service_deployment(&late).unwrap();
    let saved = state
        .store
        .service_deployment(current.service_id)
        .unwrap()
        .unwrap();
    assert_eq!(saved.spec.auto_deploy, Some(policy));
    assert!(saved.log.contains(&"late health observation".into()));
    let after = state.store.tag_watch(current.service_id).unwrap().unwrap();
    assert_eq!(after.generation, watcher.generation);
    assert_eq!(after.status, watcher.status);
    assert_eq!(
        state.store.service_releases(current.service_id).unwrap()[0]
            .spec
            .auto_deploy,
        current.spec.auto_deploy
    );
    let mut failed = current.clone();
    failed.id = Uuid::new_v4();
    failed.phase = ServicePhase::Failed;
    state
        .store
        .restore_service_release(&failed, &current)
        .unwrap();
    assert_eq!(
        state
            .store
            .micro_service(current.service_id)
            .unwrap()
            .unwrap()
            .spec
            .auto_deploy,
        saved.spec.auto_deploy
    );
    assert_eq!(
        state
            .store
            .service_deployment(current.service_id)
            .unwrap()
            .unwrap()
            .spec
            .auto_deploy,
        saved.spec.auto_deploy
    );
}

#[tokio::test]
async fn iac_auto_deploy_explicit_stack_policy_retains_its_private_image_writer() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let current = running();
    insert(&state, &current);
    let stack = state.store.register_stack("automation").unwrap();
    let target = ManagedResource {
        kind: "service".into(),
        id: current.service_id.to_string(),
    };
    let before = state.store.iac_resource_owner(&target).unwrap();
    state
        .store
        .change_iac_resource_owner(&ChangeResourceOwner {
            operation_id: Uuid::new_v4(),
            expected_endpoint_id: before.endpoint_id,
            resource: target,
            expected_generation: before.generation,
            expected_owner: before.owner,
            owner: Some(ResourceOwner {
                manager: Manager::Stack,
                id: stack.id,
                key: "web".into(),
            }),
        })
        .unwrap();
    let _ = configure(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(current.service_id.to_string()),
        ValidatedJson(current.spec.auto_deploy.clone()),
    )
    .await
    .unwrap();
    let saved = state.store.tag_watch(current.service_id).unwrap().unwrap();
    assert!(
        state
            .store
            .automatic_watch_allowed(current.service_id, &saved)
            .unwrap()
    );
}

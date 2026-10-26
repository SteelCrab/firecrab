use super::*;

#[test]
fn stable_rules_choose_versions_without_prereleases_or_wrong_prefixes() {
    let tags = [
        "latest",
        "v1.2.0",
        "v1.20.1",
        "v2.0.0",
        "v1.99.0-rc.1",
        "1.21.0",
    ]
    .map(str::to_owned);
    assert_eq!(rules::select("v1.*", &tags).as_deref(), Some("v1.20.1"));
    assert_eq!(rules::select("1.*", &tags).as_deref(), Some("1.21.0"));
    assert_eq!(rules::select("v1.2.*", &tags).as_deref(), Some("v1.2.0"));
    assert_eq!(rules::select("*", &tags).as_deref(), Some("v2.0.0"));
}

use crate::service_runtime::tests::{deployment, state};
use axum::response::IntoResponse;
use firecrab_api_types::{DeployStrategy, MicroServiceResponse, ServiceUpdate};

fn policy() -> AutoDeployPolicy {
    AutoDeployPolicy {
        reference: "ghcr.io/team/app:stable".into(),
        interval_seconds: 60,
    }
}
fn digest(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}
fn running() -> MicroServiceDeployment {
    let mut record = deployment(Uuid::new_v4());
    record.phase = ServicePhase::Running;
    record.spec.domain = Some("web.example.test".into());
    record.spec.auto_deploy = Some(policy());
    record.digest = Some(digest('a'));
    record.domain_route_owned = true;
    record.instances.push(serde_json::from_value(serde_json::json!({"vmId":Uuid::new_v4(),"ipv4":"172.30.0.2","hostPort":0,"state":"running","healthy":true})).unwrap());
    record
}
fn insert(state: &AppState, record: &MicroServiceDeployment) {
    state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id: record.service_id,
            spec: record.spec.clone(),
        })
        .unwrap();
    state.store.save_service_deployment(record).unwrap();
}

#[test]
fn observations_baseline_the_live_digest_and_do_not_revert_manual_updates() {
    let mut current = running();
    let mut watch = Watch::new(policy());
    observe(&mut watch, &current, Ok(("stable".into(), digest('a'))), 1);
    assert!(watch.pending.is_none());
    assert_eq!(watch.handled, current.digest);
    current.digest = Some(digest('c'));
    observe(&mut watch, &current, Ok(("stable".into(), digest('a'))), 2);
    assert!(watch.pending.is_none());
    observe(&mut watch, &current, Ok(("stable".into(), digest('b'))), 3);
    let pending = watch.pending.unwrap();
    assert_eq!(pending.previous, current.id);
    assert_eq!(pending.digest, digest('b'));
    assert_eq!(watch.status.operation_id, Some(pending.operation));
}

#[test]
fn rollback_failure_and_manual_pauses_have_distinct_resume_rules() {
    let current = running();
    for reason in [AutoDeployPause::Rollback, AutoDeployPause::Failed] {
        let mut watch = Watch::new(policy());
        watch.pause(reason, Some(digest('b')));
        observe(&mut watch, &current, Ok(("stable".into(), digest('b'))), 1);
        assert!(watch.pending.is_none());
        assert_eq!(watch.status.pause, Some(reason));
        observe(&mut watch, &current, Ok(("stable".into(), digest('c'))), 2);
        assert!(watch.status.pause.is_none());
        assert_eq!(watch.pending.unwrap().digest, digest('c'));
    }
    let mut watch = Watch::new(policy());
    watch.pause(AutoDeployPause::Manual, None);
    // Manual pause is filtered before registry access; observers never override it.
    assert_eq!(watch.status.pause, Some(AutoDeployPause::Manual));
}

#[test]
fn metadata_errors_back_off_without_disclosing_remote_content() {
    let current = running();
    let mut watch = Watch::new(policy());
    for n in 1..=10 {
        observe(
            &mut watch,
            &current,
            Err("Registry digest check failed"),
            100,
        );
        assert_eq!(watch.status.failures, n.min(6));
        assert!((120..=3600).contains(&(watch.status.next_check_at - 100)));
        assert!(watch.pending.is_none());
    }
    observe(
        &mut watch,
        &current,
        Ok(("stable".into(), digest('a'))),
        200,
    );
    assert_eq!(watch.status.failures, 0);
    assert!(watch.status.error.is_none());
}

#[tokio::test]
async fn watch_journal_survives_restart_and_tracks_exact_success_failure_and_rollback() {
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
    let op = watch.pending.as_ref().unwrap().operation;
    let restarted = crate::persistence::Store::open(&root.path().join("state.db")).unwrap();
    assert_eq!(
        restarted
            .tag_watch(current.service_id)
            .unwrap()
            .unwrap()
            .pending
            .unwrap()
            .operation,
        op
    );
    let mut updated = current.clone();
    updated.id = op;
    updated.digest = Some(digest('b'));
    state.store.save_service_deployment(&updated).unwrap();
    let watch = state.store.tag_watch(current.service_id).unwrap().unwrap();
    assert!(watch.pending.is_none());
    assert_eq!(watch.handled, Some(digest('b')));
    assert_eq!(watch.status.operation_id, Some(op));
    let mut rollback = updated.clone();
    rollback.id = Uuid::new_v4();
    rollback.phase = ServicePhase::Queued;
    rollback.update = Some(Box::new(ServiceUpdate {
        failure: None,
        cancelled: false,
        previous: Box::new(updated.clone()),
        requested_spec: current.spec.clone(),
        strategy: DeployStrategy::Rolling,
        rollback_of: Some(current.id),
        restoring: false,
        routed_new: false,
        transition: None,
        retiring: vec![],
        completed: false,
    }));
    state.store.save_service_deployment(&rollback).unwrap();
    let watch = state.store.tag_watch(current.service_id).unwrap().unwrap();
    assert_eq!(watch.status.pause, Some(AutoDeployPause::Rollback));
    assert_eq!(watch.blocked, Some(digest('b')));
    let mut manual = watch.clone();
    manual.pause(AutoDeployPause::Manual, None);
    state
        .store
        .save_tag_watch(current.service_id, &manual)
        .unwrap();
    // API restart can journal the same rollback as queued again. It cannot override Manual pause.
    state.store.save_service_deployment(&rollback).unwrap();
    assert_eq!(
        state
            .store
            .tag_watch(current.service_id)
            .unwrap()
            .unwrap()
            .status
            .pause,
        Some(AutoDeployPause::Manual)
    );
    state
        .store
        .save_tag_watch(current.service_id, &watch)
        .unwrap();
    rollback.phase = ServicePhase::Failed;
    rollback.update.as_mut().unwrap().completed = true;
    state
        .store
        .restore_service_release(&rollback, &updated)
        .unwrap();
    assert_eq!(
        state
            .store
            .tag_watch(current.service_id)
            .unwrap()
            .unwrap()
            .status
            .pause,
        Some(AutoDeployPause::Rollback)
    );
    let mut watch = Watch::new(policy());
    observe(&mut watch, &updated, Ok(("stable".into(), digest('c'))), 3);
    let failed = watch.pending.as_ref().unwrap().operation;
    state
        .store
        .save_tag_watch(current.service_id, &watch)
        .unwrap();
    rollback.id = failed;
    rollback.update.as_mut().unwrap().rollback_of = None;
    state.store.save_service_deployment(&rollback).unwrap();
    state
        .store
        .restore_service_release(&rollback, &updated)
        .unwrap();
    let watch = state.store.tag_watch(current.service_id).unwrap().unwrap();
    assert_eq!(watch.status.pause, Some(AutoDeployPause::Failed));
    assert_eq!(watch.blocked, Some(digest('c')));
    assert!(watch.pending.is_none());
    state
        .store
        .delete_micro_service(current.service_id)
        .unwrap();
    assert!(state.store.tag_watch(current.service_id).unwrap().is_none());
}

#[tokio::test]
async fn policy_controls_preserve_vms_release_history_and_pause_across_restart() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut current = running();
    current.spec.auto_deploy = None;
    insert(&state, &current);
    let id = current.service_id.to_string();
    let request = RequestId(Uuid::new_v4());
    let Json(disabled) = get(State(state.clone()), Extension(request), Path(id.clone()))
        .await
        .unwrap();
    assert!(disabled.policy.is_none());
    let _ = configure(
        State(state.clone()),
        Extension(request),
        Path(id.clone()),
        ValidatedJson(Some(policy())),
    )
    .await
    .unwrap();
    let updated = state
        .store
        .service_deployment(current.service_id)
        .unwrap()
        .unwrap();
    assert_eq!(updated.id, current.id);
    assert_eq!(updated.instances.len(), current.instances.len());
    assert_eq!(
        state.store.service_releases(current.service_id).unwrap()[0]
            .spec
            .auto_deploy,
        None
    );
    let Json(paused) = pause(State(state.clone()), Extension(request), Path(id.clone()))
        .await
        .unwrap();
    assert_eq!(paused.pause, Some(AutoDeployPause::Manual));
    let Json(unchanged) = configure(
        State(state.clone()),
        Extension(request),
        Path(id.clone()),
        ValidatedJson(Some(policy())),
    )
    .await
    .unwrap();
    assert_eq!(unchanged.pause, paused.pause);
    let Json(resumed) = resume(State(state.clone()), Extension(request), Path(id.clone()))
        .await
        .unwrap();
    assert!(resumed.pause.is_none());
    assert_eq!(resumed.next_check_at, 0);
    let Json(disabled) = configure(
        State(state.clone()),
        Extension(request),
        Path(id.clone()),
        ValidatedJson(None),
    )
    .await
    .unwrap();
    assert!(disabled.policy.is_none());
    assert!(
        pause(State(state.clone()), Extension(request), Path(id.clone()))
            .await
            .is_err()
    );
    assert!(
        get(
            State(state.clone()),
            Extension(request),
            Path(Uuid::new_v4().to_string())
        )
        .await
        .is_err()
    );
    current.phase = ServicePhase::Queued;
    state.store.save_service_deployment(&current).unwrap();
    let error = configure(
        State(state.clone()),
        Extension(request),
        Path(id.clone()),
        ValidatedJson(Some(policy())),
    )
    .await
    .unwrap_err();
    assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn private_claim_rejects_stale_policy_manual_pause_other_service_and_config_edits() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let current = running();
    insert(&state, &current);
    let mut watch = Watch::new(policy());
    observe(&mut watch, &current, Ok(("stable".into(), digest('b'))), 1);
    state
        .store
        .save_tag_watch(current.service_id, &watch)
        .unwrap();
    let op = watch.pending.as_ref().unwrap().operation;
    let mut spec = current.spec.clone();
    spec.image = format!("ghcr.io/team/app@{}", digest('b'));
    assert!(authorize(&state, &current, &spec, &watch, op).await.is_ok());
    spec.cpu = 2;
    assert!(
        authorize(&state, &current, &spec, &watch, op)
            .await
            .is_err()
    );
    spec.cpu = current.spec.cpu;
    assert!(
        authorize(&state, &current, &spec, &watch, Uuid::new_v4())
            .await
            .is_err()
    );
    let mut stale = watch.clone();
    stale.generation = Uuid::new_v4();
    assert!(
        authorize(&state, &current, &spec, &stale, op)
            .await
            .is_err()
    );
    watch.pause(AutoDeployPause::Manual, None);
    state
        .store
        .save_tag_watch(current.service_id, &watch)
        .unwrap();
    assert!(
        authorize(&state, &current, &spec, &watch, op)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn registry_metadata_verifies_authenticated_manifest_and_bounds_version_discovery() {
    for bearer in [false, true] {
        let (registry, task) = crate::oci::registries::tests::fixture(bearer).await;
        let selected = rules::metadata(registry.reference(), "v1", registry.credential())
            .await
            .unwrap();
        assert_eq!(selected.0, "v1");
        assert_eq!(selected.1, {
            use sha2::Digest;
            format!("sha256:{:x}", sha2::Sha256::digest(&registry.manifest))
        });
        // Both authenticated tag pages contain only non-semver names; never silently select latest.
        assert!(
            rules::metadata(registry.reference(), "v*", registry.credential())
                .await
                .unwrap_err()
                .contains("No stable")
        );
        assert!(
            rules::metadata(registry.reference(), "v1", None)
                .await
                .is_err()
        );
        task.abort();
    }
}

#[tokio::test]
async fn delayed_observations_cannot_override_pause_policy_release_or_deleted_service() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let current = running();
    insert(&state, &current);
    let watch = state.store.tag_watch(current.service_id).unwrap().unwrap();
    let mut paused = watch.clone();
    paused.pause(AutoDeployPause::Manual, None);
    state
        .store
        .save_tag_watch(current.service_id, &paused)
        .unwrap();
    finish_observation(
        &state,
        &current,
        &watch,
        Ok(("stable".into(), digest('b'))),
        1,
    )
    .await
    .unwrap();
    assert_eq!(
        state
            .store
            .tag_watch(current.service_id)
            .unwrap()
            .unwrap()
            .status
            .pause,
        Some(AutoDeployPause::Manual)
    );
    state
        .store
        .save_tag_watch(current.service_id, &watch)
        .unwrap();
    let mut replaced = current.clone();
    replaced.id = Uuid::new_v4();
    state.store.save_service_deployment(&replaced).unwrap();
    finish_observation(
        &state,
        &current,
        &watch,
        Ok(("stable".into(), digest('b'))),
        1,
    )
    .await
    .unwrap();
    assert!(
        state
            .store
            .tag_watch(current.service_id)
            .unwrap()
            .unwrap()
            .pending
            .is_none()
    );
    finish_observation(
        &state,
        &replaced,
        &watch,
        Err("Registry metadata check timed out"),
        1,
    )
    .await
    .unwrap();
    assert_eq!(
        state
            .store
            .tag_watch(current.service_id)
            .unwrap()
            .unwrap()
            .status
            .failures,
        1
    );
    state
        .store
        .delete_micro_service(current.service_id)
        .unwrap();
    finish_observation(
        &state,
        &current,
        &watch,
        Ok(("stable".into(), digest('b'))),
        1,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn disabled_paused_not_due_and_non_running_services_do_not_contact_a_registry() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut current = running();
    insert(&state, &current);
    let mut watch = Watch::new(policy());
    watch.status.next_check_at = i64::MAX;
    state
        .store
        .save_tag_watch(current.service_id, &watch)
        .unwrap();
    reconcile(&state).await.unwrap();
    assert!(
        state
            .store
            .tag_watch(current.service_id)
            .unwrap()
            .unwrap()
            .status
            .observed_digest
            .is_none()
    );
    watch.pause(AutoDeployPause::Manual, None);
    state
        .store
        .save_tag_watch(current.service_id, &watch)
        .unwrap();
    check(&state, current.clone()).await.unwrap();
    current.phase = ServicePhase::Stopped;
    check(&state, current.clone()).await.unwrap();
    current.phase = ServicePhase::Running;
    current.spec.auto_deploy = None;
    check(&state, current).await.unwrap();
}

#[tokio::test]
async fn staged_digest_enters_the_existing_queue_once_and_survives_lost_acknowledgement() {
    let root = crate::firecracker::test_support::short_tempdir();
    let mut state = state(root.path()).await;
    let journal = crate::service_runtime::deploy::tests::journal();
    let helper = crate::service_runtime::deploy::tests::helper(root.path(), &journal).await;
    state.network = helper.client.clone();
    let mut previous = (*journal.update.as_ref().unwrap().previous).clone();
    previous.spec.auto_deploy = Some(policy());
    previous.request.host_port = 0;
    insert(&state, &previous);
    let permit = state.service_deploy_permits.acquire().await.unwrap();
    let watch = state.store.tag_watch(previous.service_id).unwrap().unwrap();
    finish_observation(
        &state,
        &previous,
        &watch,
        Ok(("stable".into(), digest('b'))),
        1,
    )
    .await
    .unwrap();
    let queued = state
        .store
        .service_deployment(previous.service_id)
        .unwrap()
        .unwrap();
    assert_eq!(queued.phase, ServicePhase::Queued);
    assert_eq!(
        queued.spec.image,
        format!("ghcr.io/team/app@{}", digest('b'))
    );
    assert!(queued.instances.is_empty());
    assert_eq!(queued.update.as_ref().unwrap().previous.id, previous.id);
    let watch = state.store.tag_watch(previous.service_id).unwrap().unwrap();
    dispatch(&state, &previous, watch).await.unwrap();
    assert_eq!(
        state
            .store
            .service_deployment(previous.service_id)
            .unwrap()
            .unwrap()
            .id,
        queued.id
    );
    assert!(state.store.service_deploy_waiting().unwrap());
    // Pause changes future observations, not an operation already accepted by the queue.
    let _ = pause(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(previous.service_id.to_string()),
    )
    .await
    .unwrap();
    assert_eq!(
        state
            .store
            .tag_watch(previous.service_id)
            .unwrap()
            .unwrap()
            .pending
            .unwrap()
            .operation,
        queued.id
    );
    state
        .store
        .delete_micro_service(previous.service_id)
        .unwrap();
    drop(permit);
}

#[tokio::test]
async fn a_manual_release_change_discards_unaccepted_intent_without_pausing_the_policy() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let current = running();
    insert(&state, &current);
    let mut watch = Watch::new(policy());
    observe(&mut watch, &current, Ok(("stable".into(), digest('b'))), 1);
    state
        .store
        .save_tag_watch(current.service_id, &watch)
        .unwrap();
    let mut replacement = current.clone();
    replacement.id = Uuid::new_v4();
    replacement.digest = Some(digest('c'));
    state.store.save_service_deployment(&replacement).unwrap();
    dispatch(&state, &current, watch).await.unwrap();
    let saved = state.store.tag_watch(current.service_id).unwrap().unwrap();
    assert!(saved.pending.is_none());
    assert!(saved.status.pause.is_none());
    assert!(saved.status.operation_id.is_none());
    assert_eq!(
        state
            .store
            .service_deployment(current.service_id)
            .unwrap()
            .unwrap()
            .id,
        replacement.id
    );
}

#[tokio::test]
async fn manual_rollback_preserves_a_policy_even_if_its_source_auth_is_unavailable() {
    let root = crate::firecracker::test_support::short_tempdir();
    let mut state = state(root.path()).await;
    let mut journal = crate::service_runtime::deploy::tests::journal();
    let original = (*journal.update.as_ref().unwrap().previous).clone();
    let mut active = original.clone();
    active.id = Uuid::new_v4();
    active.spec.auto_deploy = Some(AutoDeployPolicy {
        reference: "ghcr.io/disconnected/app:stable".into(),
        interval_seconds: 60,
    });
    active.spec.image = format!("ghcr.io/team/app@{}", digest('b'));
    active.digest = Some(digest('b'));
    active.request.host_port = 0;
    journal.update.as_mut().unwrap().previous = Box::new(active.clone());
    let helper = crate::service_runtime::deploy::tests::helper(root.path(), &journal).await;
    state.network = helper.client.clone();
    state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id: active.service_id,
            spec: active.spec.clone(),
        })
        .unwrap();
    state.store.save_service_deployment(&original).unwrap();
    state.store.save_service_deployment(&active).unwrap();
    let permit = state.service_deploy_permits.acquire().await.unwrap();
    let (_, Json(rollback)) = deploy::rollback(
        State(state.clone()),
        Extension(RequestId(Uuid::new_v4())),
        Path(active.service_id.to_string()),
        ValidatedJson(firecrab_api_types::RollbackServiceRequest {
            expected_release_id: Some(active.id),
            operation_id: Uuid::new_v4(),
            release_id: original.id,
            strategy: DeployStrategy::Rolling,
        }),
    )
    .await
    .unwrap();
    assert_eq!(rollback.spec.auto_deploy, active.spec.auto_deploy);
    assert_eq!(
        rollback.spec.image,
        format!("ghcr.io/team/app@{}", digest('a'))
    );
    assert_eq!(
        state
            .store
            .tag_watch(active.service_id)
            .unwrap()
            .unwrap()
            .status
            .pause,
        Some(AutoDeployPause::Rollback)
    );
    state.store.delete_micro_service(active.service_id).unwrap();
    drop(permit);
}

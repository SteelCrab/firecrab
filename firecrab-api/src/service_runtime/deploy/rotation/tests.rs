use super::*;
use crate::secrets::{Secret, SecretSummary};
use crate::service_runtime::tests::{deployment, state};
use firecrab_api_types::MicroServiceResponse;

const OLD: &str = "rotation-private-old-value-6b18f329";
const NEW: &str = "rotation-private-new-value-72d9421a";

#[tokio::test]
async fn controller_credentials_never_fall_back_or_rotate_to_host_local_values() {
    use firecrab_api_types::{
        ClusterServiceCredential, ClusterServiceDesired, ServiceWorkerIntent,
    };
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let local = secret(&state, "host-local-secret-with-the-same-uuid");
    let mut record = deployment(Uuid::new_v4());
    record.spec.secrets.insert("PASSWORD".into(), local);
    state
        .store
        .cluster_service_worker_admit(
            record.service_id,
            Uuid::new_v4(),
            ServiceWorkerIntent {
                generation: 1,
                release_generation: 1,
                desired: ClusterServiceDesired::Running,
                deployment_id: record.id,
                spec: record.spec.clone(),
                request: record.request.clone(),
                state: Default::default(),
                credentials: BTreeMap::from([(
                    "PASSWORD".into(),
                    ClusterServiceCredential {
                        secret_id: local,
                        version: 1,
                    },
                )]),
            },
        )
        .unwrap();
    assert!(
        secrets::preflight_reserved(&state, &record.spec, record.id, Uuid::new_v4())
            .await
            .is_err()
    );
    assert!(secrets::capture(&state, &record).await.is_err());
    state
        .secrets
        .write(|contents| {
            contents
                .service_environments
                .insert(record.id, BTreeMap::from([("PASSWORD".into(), OLD.into())]));
            Ok(())
        })
        .unwrap();
    secrets::preflight_reserved(&state, &record.spec, record.id, Uuid::new_v4())
        .await
        .unwrap();
    secrets::capture(&state, &record).await.unwrap();
    record.phase = ServicePhase::Running;
    record.instances.push(ServiceInstance {
        vm_id: Uuid::new_v4(),
        ipv4: "172.30.0.2".into(),
        host_port: 30100,
        state: VmState::Running,
        healthy: true,
    });
    state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id: record.service_id,
            spec: record.spec.clone(),
        })
        .unwrap();
    save(&state, &record).await.unwrap();
    edit(&state, local, Some(NEW));
    assert_eq!(reconcile(&state, 100).await.unwrap(), 0);
    assert_eq!(
        state
            .store
            .service_deployment(record.service_id)
            .unwrap()
            .unwrap(),
        record
    );
    state
        .secrets
        .read(|contents| {
            assert_eq!(contents.service_environments[&record.id]["PASSWORD"], OLD);
            assert!(contents.service_rotations.is_empty());
            Ok(())
        })
        .unwrap();
}

fn secret(state: &AppState, value: &str) -> Uuid {
    let id = Uuid::new_v4();
    state
        .secrets
        .write(|contents| {
            contents.secrets.push(Secret {
                summary: SecretSummary {
                    id,
                    name: id.simple().to_string(),
                    description: String::new(),
                    version: 1,
                    created_at: 0,
                    updated_at: 0,
                    source: None,
                },
                value: value.into(),
            });
            Ok(())
        })
        .unwrap();
    id
}

fn edit(state: &AppState, id: Uuid, value: Option<&str>) {
    state
        .secrets
        .write(|contents| {
            let secret = contents
                .secrets
                .iter_mut()
                .find(|secret| secret.summary.id == id)
                .unwrap();
            secret.summary.version += 1;
            secret.summary.description.push_str("metadata edit");
            if let Some(value) = value {
                secret.value = value.into();
            }
            Ok(())
        })
        .unwrap();
}

async fn running(state: &AppState, name: &str, secret: Uuid) -> MicroServiceDeployment {
    let mut record = deployment(Uuid::new_v4());
    record.spec.name = name.into();
    record.spec.domain = Some(format!("{name}.example.test"));
    record.spec.secrets.insert("PASSWORD".into(), secret);
    record.phase = ServicePhase::Running;
    record.digest = Some(format!("sha256:{}", "a".repeat(64)));
    record.domain_route_owned = true;
    record.instances.push(ServiceInstance {
        vm_id: Uuid::new_v4(),
        ipv4: "172.30.0.2".into(),
        host_port: 0,
        state: VmState::Running,
        healthy: true,
    });
    state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id: record.service_id,
            spec: record.spec.clone(),
        })
        .unwrap();
    save(state, &record).await.unwrap();
    secrets::capture(state, &record).await.unwrap();
    record
}

fn disconnect(state: &AppState) {
    state
        .store
        .delete_pipe_auth(crate::pipe_auth::Provider::Github)
        .unwrap();
}

fn tracking(state: &AppState, service: Uuid) -> ServiceRotation {
    state
        .secrets
        .read(|contents| Ok(contents.service_rotations[&service].clone()))
        .unwrap()
}

#[tokio::test]
async fn idle_and_metadata_edits_do_not_create_work_or_an_unused_vault() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    assert_eq!(reconcile(&state, 0).await.unwrap(), 0);
    assert!(!state.secrets.is_initialized());
    let secret = secret(&state, OLD);
    let active = running(&state, "unchanged", secret).await;
    edit(&state, secret, None);
    disconnect(&state);
    assert_eq!(reconcile(&state, 0).await.unwrap(), 0);
    state
        .secrets
        .read(|contents| {
            assert!(contents.service_rotations.is_empty());
            Ok(())
        })
        .unwrap();
    assert!(
        state
            .store
            .service_deployment(active.service_id)
            .unwrap()
            .unwrap()
            .error
            .is_none()
    );
}

#[tokio::test]
async fn shared_changes_track_only_running_references_without_exposing_values() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let shared = secret(&state, OLD);
    let unrelated = secret(&state, "unrelated-private-value");
    let a = running(&state, "first", shared).await;
    let b = running(&state, "second", shared).await;
    let other = running(&state, "other", unrelated).await;
    let mut stopped = running(&state, "stopped", shared).await;
    stopped.phase = ServicePhase::Stopped;
    stopped.instances.clear();
    save(&state, &stopped).await.unwrap();
    disconnect(&state);
    edit(&state, shared, Some(NEW));
    assert_eq!(reconcile(&state, 100).await.unwrap(), 0);
    state
        .secrets
        .read(|contents| {
            assert_eq!(contents.service_rotations.len(), 2);
            assert_eq!(contents.service_rotations[&a.service_id].attempts, 1);
            assert_eq!(contents.service_rotations[&b.service_id].attempts, 1);
            assert_eq!(contents.service_environments[&a.id]["PASSWORD"], OLD);
            Ok(())
        })
        .unwrap();
    for active in [&a, &b] {
        let current = state
            .store
            .service_deployment(active.service_id)
            .unwrap()
            .unwrap();
        assert_eq!(current.id, active.id);
        assert!(
            current
                .error
                .as_deref()
                .unwrap()
                .starts_with("Secret rotation")
        );
        let public = serde_json::to_string(&current).unwrap();
        assert!(!public.contains(OLD) && !public.contains(NEW));
    }
    assert!(
        state
            .store
            .service_deployment(other.service_id)
            .unwrap()
            .unwrap()
            .error
            .is_none()
    );
    for path in [
        root.path().join("state.db"),
        root.path().join("state.secrets/vault.bin"),
    ] {
        let bytes = std::fs::read(path).unwrap();
        assert!(
            !bytes
                .windows(OLD.len())
                .any(|window| window == OLD.as_bytes())
        );
        assert!(
            !bytes
                .windows(NEW.len())
                .any(|window| window == NEW.as_bytes())
        );
    }
}

#[tokio::test]
async fn admission_retries_survive_restart_pause_at_five_and_reset_only_for_changed_values() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let secret = secret(&state, OLD);
    let active = running(&state, "retry", secret).await;
    disconnect(&state);
    edit(&state, secret, Some(NEW));
    reconcile(&state, 0).await.unwrap();
    let first = tracking(&state, active.service_id);
    assert_eq!(first.attempts, 1);
    reconcile(&state, 4_999).await.unwrap();
    assert_eq!(tracking(&state, active.service_id).attempts, 1);
    let restarted = crate::service_runtime::tests::state(root.path()).await;
    disconnect(&restarted);
    for (now, attempts) in [(5_000, 2), (15_000, 3), (35_000, 4), (75_000, 5)] {
        reconcile(&restarted, now).await.unwrap();
        let record = tracking(&restarted, active.service_id);
        assert_eq!(record.attempts, attempts);
        assert_eq!(
            record.operation_id, first.operation_id,
            "lost admission ACK must reuse its identity"
        );
    }
    reconcile(&restarted, 75_001).await.unwrap();
    assert!(tracking(&restarted, active.service_id).paused);
    assert_eq!(
        restarted
            .store
            .service_deployment(active.service_id)
            .unwrap()
            .unwrap()
            .error
            .as_deref(),
        Some(PAUSED)
    );
    edit(&restarted, secret, None);
    reconcile(&restarted, 1_000_000).await.unwrap();
    assert_eq!(tracking(&restarted, active.service_id).attempts, 5);
    edit(&restarted, secret, Some("another-private-value"));
    reconcile(&restarted, 1_000_001).await.unwrap();
    let next = tracking(&restarted, active.service_id);
    assert_eq!(next.attempts, 1);
    assert_ne!(next.operation_id, first.operation_id);
}

#[tokio::test]
async fn busy_host_does_not_consume_retry_budget_and_stop_clears_tracking() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let secret = secret(&state, OLD);
    let mut active = running(&state, "busy", secret).await;
    disconnect(&state);
    edit(&state, secret, Some(NEW));
    let permit = state.service_deploy_permits.acquire().await.unwrap();
    reconcile(&state, 0).await.unwrap();
    state
        .secrets
        .read(|contents| {
            assert!(contents.service_rotations.is_empty());
            Ok(())
        })
        .unwrap();
    drop(permit);
    reconcile(&state, 0).await.unwrap();
    assert_eq!(tracking(&state, active.service_id).attempts, 1);
    active.phase = ServicePhase::Stopped;
    active.instances.clear();
    save(&state, &active).await.unwrap();
    reconcile(&state, 5_000).await.unwrap();
    state
        .secrets
        .read(|contents| {
            assert!(contents.service_rotations.is_empty());
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn rotation_keeps_the_current_digest_and_tag_while_waiting_for_the_shared_permit() {
    let root = crate::firecracker::test_support::short_tempdir();
    let mut state = state(root.path()).await;
    let secret = secret(&state, OLD);
    let active = running(&state, "pinned", secret).await;
    let mut journal = super::super::tests::journal();
    *journal.update.as_mut().unwrap().previous = active.clone();
    let helper = super::super::tests::helper(root.path(), &journal).await;
    state.network = helper.client.clone();
    edit(&state, secret, Some(NEW));
    let permit = state.service_deploy_permits.acquire().await.unwrap();
    let operation = Uuid::new_v4();
    let queued = rotate(state.clone(), &active, operation).await.unwrap();
    assert_eq!(queued.phase, ServicePhase::Queued);
    assert_eq!(queued.digest, active.digest);
    assert_eq!(queued.spec, active.spec);
    let replay = rotate(state.clone(), &active, operation).await.unwrap();
    assert_eq!(replay.id, queued.id);
    state.service_deploy_permits.close();
    drop(permit);
}

#[tokio::test]
async fn git_managed_services_keep_their_stack_writer_and_running_secret_values() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let secret = secret(&state, OLD);
    let active = running(&state, "managed", secret).await;
    state.store.register_stack("rotation").unwrap();
    state
        .store
        .bind_stack_resource(
            "rotation",
            &firecrab_stack::Binding {
                kind: firecrab_stack::ResourceKind::Service,
                key: "web".into(),
                resource_id: active.service_id,
            },
        )
        .unwrap();
    let source = serde_json::from_value(serde_json::json!({
        "expectedGeneration": 0,
        "source": {"repository":"https://github.com/example/infra", "ref":"main", "manifestPath":"stack.yaml", "paused":true, "pollIntervalSeconds":60}
    })).unwrap();
    state
        .store
        .configure_stack_source("rotation", &source, 0)
        .unwrap();
    edit(&state, secret, Some(NEW));
    let operation = Uuid::new_v4();
    let error = rotate(state.clone(), &active, operation).await.unwrap_err();
    assert_eq!(error.code(), "stack_source_managed");
    assert!(state.store.service_attempt(operation).unwrap().is_none());
    assert_eq!(reconcile(&state, 0).await.unwrap(), 0);
    let current = state
        .store
        .service_deployment(active.service_id)
        .unwrap()
        .unwrap();
    assert_eq!(current.id, active.id);
    assert_eq!(current.phase, ServicePhase::Running);
    assert!(current.error.unwrap().contains("stack ownership"));
    state
        .secrets
        .read(|contents| {
            assert_eq!(contents.service_environments[&active.id]["PASSWORD"], OLD);
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn invalid_inputs_fail_closed_and_repeated_status_reads_do_not_grow_the_log() {
    for invalid in ["missing", "oversized", "snapshot", "domain"] {
        let root = tempfile::tempdir().unwrap();
        let state = state(root.path()).await;
        let secret = secret(&state, OLD);
        let mut active = running(&state, invalid, secret).await;
        edit(&state, secret, Some(NEW));
        state
            .secrets
            .write(|contents| {
                match invalid {
                    "missing" => contents.secrets.clear(),
                    "oversized" => contents.secrets[0].value = "private-value".repeat(1000),
                    "snapshot" => {
                        contents.service_environments.remove(&active.id);
                    }
                    _ => {}
                }
                Ok(())
            })
            .unwrap();
        if invalid == "domain" {
            active.spec.domain = None;
            save(&state, &active).await.unwrap();
        }
        reconcile(&state, 0).await.unwrap();
        let first = state
            .store
            .service_deployment(active.service_id)
            .unwrap()
            .unwrap();
        assert_eq!(first.id, active.id);
        assert!(
            first
                .error
                .as_deref()
                .unwrap()
                .starts_with("Secret rotation requires")
        );
        reconcile(&state, 5_000).await.unwrap();
        let second = state
            .store
            .service_deployment(active.service_id)
            .unwrap()
            .unwrap();
        assert_eq!(first.log, second.log);
        state
            .secrets
            .read(|contents| {
                assert!(contents.service_rotations.is_empty());
                Ok(())
            })
            .unwrap();
    }
}

#[tokio::test]
async fn stale_reservations_and_manual_release_changes_cannot_overwrite_newer_work() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let secret = secret(&state, OLD);
    let active = running(&state, "fenced", secret).await;
    edit(&state, secret, Some(NEW));
    let observation = Observation {
        deployment: active.clone(),
        rotation: None,
        change: Change::Changed(
            fingerprint(&BTreeMap::from([("PASSWORD".into(), NEW.into())])).unwrap(),
        ),
    };
    assert!(matches!(
        reserve(&state, &observation, 0).await.unwrap(),
        Some(Action::Submit(_))
    ));
    assert!(reserve(&state, &observation, 0).await.unwrap().is_none());
    let tracked = tracking(&state, active.service_id);
    let mut newer = active.clone();
    newer.id = Uuid::new_v4();
    save(&state, &newer).await.unwrap();
    notice(&state, &active, Some(PAUSED)).await.unwrap();
    assert!(
        state
            .store
            .service_deployment(active.service_id)
            .unwrap()
            .unwrap()
            .error
            .is_none()
    );
    let result = rotate(state.clone(), &active, tracked.operation_id)
        .await
        .unwrap_err();
    assert_eq!(result.code(), "release_changed");
    assert!(
        state
            .store
            .service_attempt(tracked.operation_id)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_value_change_between_observation_and_reservation_waits_for_the_next_scan() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let secret = secret(&state, OLD);
    let active = running(&state, "newer-value", secret).await;
    let observation = Observation {
        deployment: active,
        rotation: None,
        change: Change::Changed(
            fingerprint(&BTreeMap::from([("PASSWORD".into(), NEW.into())])).unwrap(),
        ),
    };
    edit(&state, secret, Some("newer-private-value"));
    assert!(reserve(&state, &observation, 0).await.unwrap().is_none());
    state
        .secrets
        .read(|contents| {
            assert!(contents.service_rotations.is_empty());
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn failed_completed_attempts_get_a_new_identity_and_reverted_values_clear_pending_work() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let secret = secret(&state, OLD);
    let active = running(&state, "failed", secret).await;
    disconnect(&state);
    edit(&state, secret, Some(NEW));
    reconcile(&state, 0).await.unwrap();
    let first = tracking(&state, active.service_id);
    let mut failed = super::super::tests::journal();
    failed.id = first.operation_id;
    failed.service_id = active.service_id;
    failed.spec = active.spec.clone();
    failed.instances.clear();
    failed.phase = ServicePhase::Failed;
    failed.finished_at = Some(6);
    let update = failed.update.as_mut().unwrap();
    *update.previous = active.clone();
    update.completed = true;
    update.failure = Some("App health check failed".into());
    save(&state, &failed).await.unwrap();
    state
        .store
        .restore_service_release(&failed, &active)
        .unwrap();
    reconcile(&state, 5_000).await.unwrap();
    assert_eq!(tracking(&state, active.service_id).attempts, 1);
    reconcile(&state, 11_000).await.unwrap();
    let second = tracking(&state, active.service_id);
    assert_ne!(second.operation_id, first.operation_id);
    assert_eq!(second.attempts, 2);
    edit(&state, secret, Some(OLD));
    reconcile(&state, 11_001).await.unwrap();
    state
        .secrets
        .read(|contents| {
            assert!(contents.service_rotations.is_empty());
            Ok(())
        })
        .unwrap();
    assert!(
        state
            .store
            .service_deployment(active.service_id)
            .unwrap()
            .unwrap()
            .error
            .is_none()
    );
}

#[tokio::test]
async fn corrupted_vault_does_not_change_the_running_release() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let secret = secret(&state, OLD);
    let active = running(&state, "corrupt", secret).await;
    std::fs::write(
        root.path().join("state.secrets/vault.bin"),
        b"invalid ciphertext",
    )
    .unwrap();
    assert!(reconcile(&state, 0).await.is_err());
    assert_eq!(
        state
            .store
            .service_deployment(active.service_id)
            .unwrap()
            .unwrap(),
        active
    );
}

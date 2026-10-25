use super::*;
use crate::service_runtime::tests::{deployment, state};
use firecrab_api_types::{MicroServiceResponse, ReplicaHealth};

fn running() -> MicroServiceDeployment {
    let mut record = deployment(Uuid::new_v4());
    record.phase = ServicePhase::Running;
    record.request.micro_network_id = Some(Uuid::new_v4());
    record.digest = Some(format!("sha256:{}", "a".repeat(64)));
    record.instances.push(ServiceInstance {
        vm_id: Uuid::new_v4(),
        ipv4: "127.0.0.1".into(),
        host_port: 30100,
        state: VmState::Running,
        healthy: true,
    });
    record
}

#[test]
fn failure_threshold_hysteresis_and_stable_health_bound_restart_loops() {
    let mut record = running();
    let id = record.instances[0].vm_id;
    for now in [1, 6] {
        observe(&mut record, &[VmState::Running], &[false], now);
        assert!(record.instances[0].healthy);
        assert!(!admit(&mut record, now));
    }
    observe(&mut record, &[VmState::Stopped], &[false], 11);
    assert_eq!(record.phase, ServicePhase::Degraded);
    assert!(admit(&mut record, 11));
    assert_eq!(record.recovery.as_ref().unwrap().replicas[&id].attempts, 1);
    record.recovery.as_mut().unwrap().repair = None;
    observe(&mut record, &[VmState::Running], &[true], 20);
    assert!(!record.instances[0].healthy);
    observe(&mut record, &[VmState::Running], &[true], 25);
    assert!(record.instances[0].healthy);
    assert_eq!(record.recovery.as_ref().unwrap().replicas[&id].attempts, 1);
    observe(&mut record, &[VmState::Running], &[true], 80);
    assert_eq!(record.recovery.as_ref().unwrap().replicas[&id].attempts, 0);
}

#[test]
fn retry_budget_and_delay_survive_serialization_without_new_intent() {
    let mut record = running();
    let id = record.instances[0].vm_id;
    record.instances[0].healthy = false;
    record.recovery = Some(Box::new(ServiceRecovery {
        replicas: BTreeMap::from([(
            id,
            ReplicaHealth {
                failures: 3,
                ..Default::default()
            },
        )]),
        repair: None,
    }));
    assert!(admit(&mut record, 0));
    let intent = record.recovery.as_ref().unwrap().repair.clone();
    for (attempt, now) in [(2, 5), (3, 15), (4, 35), (5, 75)] {
        record = serde_json::from_str(&serde_json::to_string(&record).unwrap()).unwrap();
        assert!(!admit(&mut record, now - 1));
        assert!(admit(&mut record, now));
        assert_eq!(
            record.recovery.as_ref().unwrap().replicas[&id].attempts,
            attempt
        );
        assert_eq!(record.recovery.as_ref().unwrap().repair, intent);
    }
    assert!(!admit(&mut record, 10_000));
}

#[test]
fn missing_slots_are_reserved_without_losing_healthy_replica_identity() {
    let mut record = running();
    let healthy_id = record.instances[0].vm_id;
    record.spec.replicas = 2;
    observe(&mut record, &[VmState::Running], &[true], 1);
    assert_eq!(record.instances.len(), 2);
    assert_eq!(record.instances[0].vm_id, healthy_id);
    let missing = record.instances[1].vm_id;
    assert_eq!(record.instances[1].host_port, 30101);
    assert!(admit(&mut record, 1));
    assert_eq!(record.recovery.unwrap().repair.unwrap().vm_id, missing);
}

#[test]
fn internal_only_missing_replicas_do_not_gain_host_ports() {
    let mut record = running();
    record.spec.domain = None;
    record.spec.replicas = 3;
    record.request.host_port = 0;
    record.instances[0].host_port = 0;
    observe(&mut record, &[VmState::Running], &[true], 1);
    assert_eq!(record.instances.len(), 3);
    assert!(
        record
            .instances
            .iter()
            .all(|instance| instance.host_port == 0)
    );
    assert!(admit(&mut record, 1));
}

#[test]
fn missing_pin_cannot_turn_recovery_into_a_mutable_tag_pull() {
    let mut record = running();
    record.digest = None;
    assert!(!admit(&mut record, 0));
    assert!(
        record
            .error
            .as_deref()
            .unwrap()
            .contains("pinned image digest")
    );
    assert!(record.recovery.is_none());
}

#[test]
fn exhausted_restart_budget_still_withdraws_a_newly_unhealthy_endpoint() {
    let mut record = running();
    let id = record.instances[0].vm_id;
    record.recovery = Some(Box::new(ServiceRecovery {
        replicas: BTreeMap::from([(
            id,
            ReplicaHealth {
                failures: 3,
                attempts: 5,
                retry_at: 1_000,
                ..Default::default()
            },
        )]),
        repair: None,
    }));
    assert!(admit(&mut record, 100));
    let health = record.recovery.as_ref().unwrap();
    assert_eq!(health.replicas[&id].attempts, 5);
    assert_eq!(
        health.repair.as_ref().unwrap().stage,
        ReplicaRepairStage::Paused
    );
    assert!(!admit(&mut record, 10_000));
}

#[tokio::test]
async fn health_observations_cannot_overwrite_a_new_desired_configuration() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut record = crate::service_runtime::deploy::tests::journal();
    record.update.as_mut().unwrap().completed = true;
    record.phase = ServicePhase::Running;
    state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id: record.service_id,
            spec: record.spec.clone(),
        })
        .unwrap();
    state.store.save_service_deployment(&record).unwrap();
    let mut desired = record.spec.clone();
    desired.env.insert("DESIRED".into(), "next".into());
    state
        .store
        .update_micro_service(&MicroServiceResponse {
            id: record.service_id,
            spec: desired.clone(),
        })
        .unwrap();
    observe(&mut record, &[VmState::Running], &[true], 1);
    save(&state, &record).await.unwrap();
    assert_eq!(
        state
            .store
            .micro_service(record.service_id)
            .unwrap()
            .unwrap()
            .spec,
        desired
    );
}

#[test]
fn health_admission_preserves_completed_deploy_retry_identity() {
    let mut record = crate::service_runtime::deploy::tests::journal();
    record.update.as_mut().unwrap().completed = true;
    record.phase = ServicePhase::Running;
    record.request.micro_network_id = Some(Uuid::new_v4());
    let journal = record.update.clone();
    for now in [0, 5, 10] {
        observe(&mut record, &[VmState::Stopped], &[false], now);
    }
    assert!(admit(&mut record, 10));
    assert_eq!(record.update, journal);
}

#[tokio::test]
async fn background_probes_persist_degradation_without_consuming_a_busy_deploy_queue() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let record = running();
    state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id: record.service_id,
            spec: record.spec.clone(),
        })
        .unwrap();
    state.store.save_service_deployment(&record).unwrap();
    let _permit = state
        .service_deploy_permits
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    for now in [0, 5, 10] {
        reconcile(&state, now).await.unwrap();
    }
    let saved = state
        .store
        .service_deployment(record.service_id)
        .unwrap()
        .unwrap();
    assert_eq!(saved.phase, ServicePhase::Degraded);
    assert!(saved.recovery.as_ref().unwrap().repair.is_none());
    assert_eq!(
        saved.recovery.as_ref().unwrap().replicas[&record.instances[0].vm_id].attempts,
        0
    );
    assert_eq!(saved.instances[0].vm_id, record.instances[0].vm_id);
    assert!(state.store.load_all().unwrap().is_empty());
    assert_eq!(
        state.store.service_releases(record.service_id).unwrap()[0].recovery,
        None
    );
}

#[tokio::test]
async fn stopped_busy_and_controller_owned_services_are_never_admitted() {
    use firecrab_api_types::{ClusterServiceDesired, ServiceWorkerIntent};
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut record = running();
    state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id: record.service_id,
            spec: record.spec.clone(),
        })
        .unwrap();
    for phase in [
        ServicePhase::Stopped,
        ServicePhase::Stopping,
        ServicePhase::Queued,
        ServicePhase::Failed,
    ] {
        record.phase = phase;
        state.store.save_service_deployment(&record).unwrap();
        reconcile(&state, 0).await.unwrap();
        assert_eq!(
            state
                .store
                .service_deployment(record.service_id)
                .unwrap()
                .unwrap(),
            record
        );
    }
    record.phase = ServicePhase::Running;
    state.store.delete_micro_service(record.service_id).unwrap();
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
                credentials: Default::default(),
            },
        )
        .unwrap();
    state
        .store
        .insert_micro_service(&MicroServiceResponse {
            id: record.service_id,
            spec: record.spec.clone(),
        })
        .unwrap();
    state.store.save_service_deployment(&record).unwrap();
    for now in [0, 5, 10] {
        reconcile(&state, now).await.unwrap();
    }
    assert_eq!(
        state
            .store
            .service_deployment(record.service_id)
            .unwrap()
            .unwrap(),
        record
    );
}

#[tokio::test]
async fn repair_keeps_the_release_and_encrypted_environment_after_database_reopen() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut record = running();
    let secret = Uuid::new_v4();
    record.spec.secrets.insert("PASSWORD".into(), secret);
    state
        .secrets
        .write(|contents| {
            contents.service_environments.insert(
                record.id,
                BTreeMap::from([("PASSWORD".into(), "recovery-private-frozen-value".into())]),
            );
            Ok(())
        })
        .unwrap();
    observe(&mut record, &[VmState::Stopped], &[false], 1);
    observe(&mut record, &[VmState::Stopped], &[false], 6);
    observe(&mut record, &[VmState::Stopped], &[false], 11);
    assert!(admit(&mut record, 11));
    state.store.save_service_deployment(&record).unwrap();
    let reopened = crate::persistence::Store::open(&root.path().join("state.db")).unwrap();
    assert_eq!(
        reopened
            .service_deployment(record.service_id)
            .unwrap()
            .unwrap(),
        record
    );
    let serialized = serde_json::to_string(&record).unwrap();
    assert!(!serialized.contains("recovery-private-frozen-value"));
    assert_eq!(
        state
            .secrets
            .read(|contents| Ok(contents.service_environments[&record.id]["PASSWORD"].clone()))
            .unwrap(),
        "recovery-private-frozen-value"
    );
}

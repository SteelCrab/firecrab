use super::*;
use crate::service_runtime::tests::{deployment, state};

#[test]
fn retries_start_after_failure_and_preserve_the_exact_side_effect_intent() {
    let mut record = deployment(Uuid::new_v4());
    let id = Uuid::new_v4();
    record.recovery = Some(Box::new(ServiceRecovery {
        replicas: BTreeMap::from([(
            id,
            firecrab_api_types::ReplicaHealth {
                attempts: 3,
                ..Default::default()
            },
        )]),
        repair: Some(ReplicaRepair {
            id: Uuid::new_v4(),
            vm_id: id,
            replacement_id: Some(Uuid::new_v4()),
            stage: ReplicaRepairStage::Replace,
            transition: None,
        }),
    }));
    let intent = record.recovery.as_ref().unwrap().repair.clone();
    failed(&mut record, 1_000);
    assert_eq!(record.phase, ServicePhase::Degraded);
    assert_eq!(
        record.recovery.as_ref().unwrap().replicas[&id].retry_at,
        1_020
    );
    assert_eq!(record.recovery.as_ref().unwrap().repair, intent);
    assert!(record.error.as_deref().unwrap().contains("five attempts"));
}

#[tokio::test]
async fn queued_worker_cannot_overwrite_stop_or_a_new_release() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let mut old = deployment(Uuid::new_v4());
    old.recovery = Some(Box::new(ServiceRecovery {
        replicas: Default::default(),
        repair: Some(ReplicaRepair {
            id: Uuid::new_v4(),
            vm_id: Uuid::new_v4(),
            replacement_id: None,
            stage: ReplicaRepairStage::Withdraw,
            transition: None,
        }),
    }));
    let permit = state
        .service_deploy_permits
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    resume(state.clone(), old.clone());
    let mut stopped = old.clone();
    stopped.phase = ServicePhase::Stopped;
    state.store.save_service_deployment(&stopped).unwrap();
    drop(permit);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        state
            .store
            .service_deployment(old.service_id)
            .unwrap()
            .unwrap(),
        stopped
    );
    let mut newer = old.clone();
    newer.id = Uuid::new_v4();
    state.store.save_service_deployment(&newer).unwrap();
    resume(state.clone(), old);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        state
            .store
            .service_deployment(newer.service_id)
            .unwrap()
            .unwrap(),
        newer
    );
}

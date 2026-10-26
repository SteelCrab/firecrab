use super::*;
use crate::service_runtime::{
    deploy::tests::{helper, journal},
    tests::state,
};
use std::sync::atomic::Ordering;

fn active(template: &MicroServiceDeployment) -> MicroServiceDeployment {
    let mut record = *template.update.as_ref().unwrap().previous.clone();
    let vm_id = record.instances[0].vm_id;
    record.recovery = Some(Box::new(ServiceRecovery {
        replicas: Default::default(),
        repair: Some(ReplicaRepair {
            id: Uuid::new_v4(),
            vm_id,
            replacement_id: None,
            stage: ReplicaRepairStage::Withdraw,
            transition: None,
        }),
    }));
    record
}

#[tokio::test]
async fn all_unready_endpoints_keep_the_owned_hostname_and_return_503_until_ready() {
    let root = tempfile::tempdir().unwrap();
    let template = journal();
    let helper = helper(root.path(), &template).await;
    helper.auto_drain.store(true, Ordering::SeqCst);
    let mut state = state(root.path()).await;
    state.network = helper.client.clone();
    let mut record = active(&template);
    switch(&state, &mut record, vec![]).await.unwrap();
    let suspended = helper.config.lock().unwrap().clone();
    assert_eq!(suspended.routes.len(), 1);
    assert_eq!(suspended.routes[0].owner, Some(owner(&record)));
    assert!(suspended.routes[0].upstreams.is_empty());
    assert!(
        suspended
            .nginx()
            .contains("location / { root /dev/null; try_files /unready =503; }")
    );
    assert!(!suspended.nginx().contains("upstream fc_route_0"));
    assert!(record.domain_route_owned);
    assert!(before_cleanup(&state, &record).await.is_err());
    before_stop(&state, &record, record.instances[0].vm_id)
        .await
        .unwrap();
    let next = ready(&record, None).unwrap();
    switch(&state, &mut record, next.clone()).await.unwrap();
    assert_eq!(helper.config.lock().unwrap().routes[0].upstreams, next);
    before_cleanup(&state, &record).await.unwrap();
    assert!(
        before_stop(&state, &record, record.instances[0].vm_id)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn drain_timeout_and_lost_ack_replay_retain_the_original_vm_identity() {
    let root = tempfile::tempdir().unwrap();
    let template = journal();
    let helper = helper(root.path(), &template).await;
    let mut state = state(root.path()).await;
    state.network = helper.client.clone();
    let mut record = active(&template);
    let instances = record.instances.clone();
    record.spec.drain_timeout = 0;
    assert!(switch(&state, &mut record, vec![]).await.is_err());
    assert_eq!(record.instances, instances);
    assert!(
        record
            .recovery
            .as_ref()
            .unwrap()
            .repair
            .as_ref()
            .unwrap()
            .transition
            .as_ref()
            .unwrap()
            .confirmed
    );
    let saved = state
        .store
        .service_deployment(record.service_id)
        .unwrap()
        .unwrap();
    let mut replay = saved;
    // The helper committed, but its acknowledgement or the confirmation save was lost.
    replay
        .recovery
        .as_mut()
        .unwrap()
        .repair
        .as_mut()
        .unwrap()
        .transition
        .as_mut()
        .unwrap()
        .confirmed = false;
    helper
        .workers
        .lock()
        .unwrap()
        .iter_mut()
        .for_each(|worker| worker.start_time += 1);
    replay.spec.drain_timeout = 1;
    switch(&state, &mut replay, vec![]).await.unwrap();
    assert_eq!(helper.changes.load(Ordering::SeqCst), 1);
    assert_eq!(replay.instances, instances);
}

#[tokio::test]
async fn foreign_route_and_unexpected_upstream_never_allow_destructive_recovery() {
    let root = tempfile::tempdir().unwrap();
    let template = journal();
    let helper = helper(root.path(), &template).await;
    helper.auto_drain.store(true, Ordering::SeqCst);
    let mut state = state(root.path()).await;
    state.network = helper.client.clone();
    let mut record = active(&template);
    helper.config.lock().unwrap().routes[0].upstreams = vec!["172.30.0.99:8080".parse().unwrap()];
    assert!(switch(&state, &mut record, vec![]).await.is_err());
    helper.config.lock().unwrap().routes[0]
        .owner
        .as_mut()
        .unwrap()
        .deployment_id = Uuid::new_v4();
    assert!(switch(&state, &mut record, vec![]).await.is_err());
    assert_eq!(helper.changes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn private_only_and_shared_names_switch_together_and_keep_internal_dns() {
    for public in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let template = journal();
        let helper = helper(root.path(), &template).await;
        helper.auto_drain.store(true, Ordering::SeqCst);
        let mut state = state(root.path()).await;
        state.network = helper.client.clone();
        let mut record = active(&template);
        record.internal_route_owned = true;
        let mut private = helper.config.lock().unwrap().routes[0].clone();
        private.host = "web.internal".into();
        private.service_port = Some(record.spec.port);
        private.network = Some(
            serde_json::from_value(serde_json::json!({
                "micro_network_id": Uuid::new_v4(), "gateway":"172.30.0.1", "prefix":24,
            }))
            .unwrap(),
        );
        if !public {
            record.spec.domain = None;
            record.domain_route_owned = false;
            helper.config.lock().unwrap().routes.clear();
        }
        helper.config.lock().unwrap().routes.push(private);
        switch(&state, &mut record, vec![]).await.unwrap();
        let saved = helper.config.lock().unwrap().clone();
        assert_eq!(saved.routes.len(), if public { 2 } else { 1 });
        assert!(saved.routes.iter().all(|route| route.upstreams.is_empty()));
        assert_eq!(saved.internal_dns_hosts(), "172.30.0.1 web.internal\n");
        assert_eq!(record.domain_route_owned, public);
        before_stop(&state, &record, record.instances[0].vm_id)
            .await
            .unwrap();
        let next = ready(&record, None).unwrap();
        switch(&state, &mut record, next.clone()).await.unwrap();
        assert!(
            helper
                .config
                .lock()
                .unwrap()
                .routes
                .iter()
                .all(|route| route.upstreams == next)
        );
        before_cleanup(&state, &record).await.unwrap();
        {
            let mut config = helper.config.lock().unwrap();
            config
                .routes
                .iter_mut()
                .find(|route| route.network.is_some())
                .unwrap()
                .owner
                .as_mut()
                .unwrap()
                .service_id = Uuid::new_v4();
        }
        assert!(before_cleanup(&state, &record).await.is_err());
    }
}

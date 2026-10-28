//! Periodic health observations and bounded repairs through the shared Deploy queue.

use super::*;
use firecrab_api_types::{ReplicaRepair, ReplicaRepairStage, ServiceRecovery};
use std::collections::BTreeSet;

mod routes;
mod worker;
pub(super) use worker::resume;

const MAX_ATTEMPTS: u8 = 5;
const FAILURE: &str = "Replica recovery failed; owned VMs are retained. Automatic retries are limited to five attempts; use Deploy or Stop after fixing the cause.";

pub(super) fn pending(record: &MicroServiceDeployment) -> bool {
    record
        .recovery
        .as_ref()
        .is_some_and(|health| health.repair.is_some())
}

pub(crate) fn start(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if reconcile(&state, chrono::Utc::now().timestamp())
                .await
                .is_err()
            {
                tracing::warn!("Could not reconcile service replica health");
            }
        }
    });
}

/// A controller owns recovery of its worker services; this loop never changes their intent.
async fn local_services(state: &AppState) -> Result<Vec<MicroServiceDeployment>, String> {
    let store = state.store.clone();
    let workers = crate::cluster::stored(move || store.cluster_service_workers())
        .await
        .map_err(|_| "Could not read controller service ownership")?
        .into_iter()
        .map(|receipt| receipt.id)
        .collect::<BTreeSet<_>>();
    let store = state.store.clone();
    database(Uuid::new_v4(), move || {
        let mut records = Vec::new();
        for service in store.list_micro_services()? {
            if !workers.contains(&service.id)
                && let Some(record) = store.service_deployment(service.id)?
                && matches!(record.phase, ServicePhase::Running | ServicePhase::Degraded)
                && record.update.as_ref().is_none_or(|update| update.completed)
            {
                records.push(record);
            }
        }
        Ok(records)
    })
    .await
    .map_err(|_| "Could not read service health ownership".into())
}

async fn reconcile(state: &AppState, now: i64) -> Result<(), String> {
    for mut observed in local_services(state).await? {
        // Probes run outside the mutation lock. Admission rechecks the exact saved owner.
        let client = health_client();
        let states = {
            let vms = state.vms.lock().unwrap_or_else(|p| p.into_inner());
            observed
                .instances
                .iter()
                .map(|instance| {
                    vms.get(&instance.vm_id)
                        .map_or(VmState::Stopped, |vm| vm.state)
                })
                .collect::<Vec<_>>()
        };
        let probes = join_all(observed.instances.iter().zip(&states).map(
            |(instance, state)| async {
                *state == VmState::Running && healthy(&client, &observed.spec, instance).await
            },
        ))
        .await;
        let _claim = state.service_mutations.lock().await;
        let store = state.store.clone();
        let id = observed.service_id;
        let Some(current) = database(observed.id, move || store.service_deployment(id))
            .await
            .map_err(|_| "Could not read service health")?
        else {
            continue;
        };
        if current != observed {
            continue;
        }
        let before = observed.clone();
        if !pending(&observed) {
            observe(&mut observed, &states, &probes, now);
        }
        let store = state.store.clone();
        let waiting = database(observed.id, move || store.service_deploy_waiting())
            .await
            .map_err(|_| "Could not inspect Deploy queue")?;
        let launch = state.service_deploy_permits.available_permits() > 0
            && !waiting
            && admit(&mut observed, now);
        if observed != before {
            save(state, &observed).await?;
        }
        if launch {
            resume(state.clone(), observed);
        }
    }
    Ok(())
}

fn observe(record: &mut MicroServiceDeployment, states: &[VmState], probes: &[bool], now: i64) {
    let health = record
        .recovery
        .get_or_insert_with(|| Box::new(ServiceRecovery::default()));
    let ids = record
        .instances
        .iter()
        .map(|instance| instance.vm_id)
        .collect::<BTreeSet<_>>();
    health.replicas.retain(|id, _| ids.contains(id));
    for ((instance, state), ready) in record.instances.iter_mut().zip(states).zip(probes) {
        instance.state = *state;
        let status = health.replicas.entry(instance.vm_id).or_default();
        if *ready {
            status.failures = 0;
            status.successes = status.successes.saturating_add(1).min(2);
            let since = *status.healthy_since.get_or_insert(now);
            if now.saturating_sub(since) >= 60 {
                status.attempts = 0;
                status.retry_at = 0;
            }
            if status.successes >= 2 {
                instance.healthy = true;
            }
        } else {
            status.successes = 0;
            status.healthy_since = None;
            status.failures = status.failures.saturating_add(1).min(3);
            if status.failures >= 3 {
                instance.healthy = false;
            }
        }
    }
    while record.instances.len() < usize::from(record.spec.replicas) {
        let index = record.instances.len() as u16;
        let vm_id = Uuid::new_v4();
        record.instances.push(ServiceInstance {
            vm_id,
            ipv4: String::new(),
            host_port: if record.spec.domain.is_some() || record.request.host_port == 0 {
                0
            } else {
                record.request.host_port + index
            },
            state: VmState::Created,
            healthy: false,
        });
        health.replicas.insert(
            vm_id,
            firecrab_api_types::ReplicaHealth {
                failures: 3,
                ..Default::default()
            },
        );
    }
    record.phase = if record.instances.len() == usize::from(record.spec.replicas)
        && record.instances.iter().all(|instance| instance.healthy)
    {
        if record.error.as_deref() == Some(FAILURE) {
            record.error = None;
        }
        ServicePhase::Running
    } else {
        ServicePhase::Degraded
    };
}

fn admit(record: &mut MicroServiceDeployment, now: i64) -> bool {
    if record
        .digest
        .as_ref()
        .is_none_or(|digest| Sha256Digest::parse(digest).is_err())
        || record.request.micro_network_id.is_none()
    {
        record.error = Some("Replica recovery requires a pinned image digest and saved MicroNetwork; use Deploy to restore them.".into());
        return false;
    }
    let health = record
        .recovery
        .get_or_insert_with(|| Box::new(ServiceRecovery::default()));
    let target = health
        .repair
        .as_ref()
        .map(|repair| repair.vm_id)
        .or_else(|| {
            record
                .instances
                .iter()
                .find(|instance| {
                    health.replicas.get(&instance.vm_id).is_some_and(|status| {
                        status.failures >= 3
                            && (status.attempts >= MAX_ATTEMPTS || now >= status.retry_at)
                    })
                })
                .map(|instance| instance.vm_id)
        });
    let Some(target) = target else { return false };
    let status = health.replicas.entry(target).or_default();
    let paused = status.attempts >= MAX_ATTEMPTS;
    if paused && health.repair.is_some() || !paused && now < status.retry_at {
        return false;
    }
    if !paused {
        status.attempts += 1;
        status.retry_at = now.saturating_add(5_i64 << (status.attempts - 1));
    }
    if health.repair.is_none() {
        health.repair = Some(ReplicaRepair {
            id: Uuid::new_v4(),
            vm_id: target,
            replacement_id: None,
            stage: if paused {
                ReplicaRepairStage::Paused
            } else {
                ReplicaRepairStage::Withdraw
            },
            transition: None,
        });
    }
    record.phase = ServicePhase::Queued;
    record.error = None;
    true
}

#[cfg(test)]
mod tests;

//! One repair holds the normal Deploy permit and keeps every VM durably owned.

use super::*;

pub(in crate::service_runtime) fn resume(state: AppState, record: MicroServiceDeployment) {
    let Some(repair) = record
        .recovery
        .as_ref()
        .and_then(|health| health.repair.as_ref())
    else {
        return;
    };
    let operation = repair.id;
    let reserved = state
        .service_deploy_permits
        .clone()
        .try_acquire_owned()
        .ok();
    tokio::spawn(async move {
        let _permit = match reserved {
            Some(permit) => permit,
            None => match state.service_deploy_permits.clone().acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => return,
            },
        };
        let store = state.store.clone();
        let id = record.service_id;
        let Ok(Some(mut current)) = database(operation, move || store.service_deployment(id)).await
        else {
            return;
        };
        if current.id != record.id
            || !current.phase.is_busy()
            || current
                .update
                .as_ref()
                .is_some_and(|update| !update.completed)
            || current
                .recovery
                .as_ref()
                .and_then(|health| health.repair.as_ref())
                .is_none_or(|repair| repair.id != operation)
            || !matches!(state.store.cluster_service_worker(id), Ok(None))
        {
            return;
        }
        if execute(&state, &mut current).await.is_err() {
            failed(&mut current, chrono::Utc::now().timestamp());
            if save(&state, &current).await.is_err() {
                tracing::error!(service_id = %current.service_id, "Could not persist replica recovery failure");
            }
        }
    });
}

fn failed(record: &mut MicroServiceDeployment, now: i64) {
    record.phase = ServicePhase::Degraded;
    record.error = Some(FAILURE.into());
    let health = record.recovery.as_mut().expect("health");
    let target = health.repair.as_ref().expect("repair").vm_id;
    let status = health.replicas.entry(target).or_default();
    status.retry_at = now.saturating_add(5_i64 << status.attempts.saturating_sub(1).min(6));
}

async fn advance(
    state: &AppState,
    record: &mut MicroServiceDeployment,
    stage: ReplicaRepairStage,
) -> Result<(), String> {
    record
        .recovery
        .as_mut()
        .expect("health")
        .repair
        .as_mut()
        .expect("repair")
        .stage = stage;
    record.phase = ServicePhase::Starting;
    save(state, record).await
}

fn vm(state: &AppState, id: Uuid) -> Option<crate::model::VmRecord> {
    state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&id)
        .cloned()
}

fn verify(state: &AppState, record: &MicroServiceDeployment, id: Uuid) -> Result<(), String> {
    let Some(vm) = vm(state, id) else {
        return Ok(());
    };
    if Some(&vm.template_version) != record.digest.as_ref()
        || Some(vm.micro_network_id) != record.request.micro_network_id
        || vm.ram != record.spec.ram
        || vm.cpu != record.spec.cpu
        || vm.env != record.spec.env
        || vm.security_group_id != record.spec.security_group_id
    {
        return Err("Recovery VM no longer matches its saved release; VM retained".into());
    }
    Ok(())
}

async fn stop(state: &AppState, record: &MicroServiceDeployment, id: Uuid) -> Result<(), String> {
    verify(state, record, id)?;
    if let Some(vm) = vm(state, id) {
        if matches!(vm.state, VmState::Starting | VmState::Stopping) {
            return Err("Replica lifecycle is still changing".into());
        }
        if matches!(vm.state, VmState::Running | VmState::Paused) {
            let _ = vms::stop_vm(
                State(state.clone()),
                Extension(RequestId(record.id)),
                Path(id.to_string()),
            )
            .await
            .map_err(|_| "Could not stop failed replica")?;
        }
    }
    Ok(())
}

async fn ports(
    state: &AppState,
    record: &MicroServiceDeployment,
    id: Uuid,
    publish: bool,
) -> Result<(), String> {
    if record.spec.domain.is_some() || vm(state, id).is_none() {
        return Ok(());
    }
    let instance = record
        .instances
        .iter()
        .find(|instance| instance.vm_id == id)
        .ok_or("Recovery replica ownership is missing")?;
    if instance.host_port == 0 {
        return Ok(());
    }
    let port_forwards = if publish {
        vec![PortForward {
            host_port: instance.host_port,
            guest_port: record.spec.port,
            protocol: PortProtocol::Tcp,
        }]
    } else {
        vec![]
    };
    let _ = vms::update_vm_port_forwards(
        State(state.clone()),
        Extension(RequestId(record.id)),
        Path(id.to_string()),
        ValidatedJson(firecrab_api_types::UpdateVmPortForwardsRequest { port_forwards }),
    )
    .await
    .map_err(|_| "Could not change recovery host ports")?;
    Ok(())
}

async fn start(
    state: &AppState,
    record: &mut MicroServiceDeployment,
    id: Uuid,
) -> Result<(), String> {
    verify(state, record, id)?;
    deploy::replicas::prepare_template(state, record, false).await?;
    if vm(state, id).is_some_and(|vm| vm.state != VmState::Running) {
        deploy::replicas::check_surge(state, &record.spec, 1).await?;
    }
    let index = record
        .instances
        .iter()
        .position(|instance| instance.vm_id == id)
        .ok_or("Recovery replica ownership is missing")?;
    deploy::replicas::start_replica(state, record, false, index).await
}

async fn refresh(state: &AppState, record: &mut MicroServiceDeployment) {
    let client = health_client();
    let checks = join_all(record.instances.iter().map(|instance| {
        let running = vm(state, instance.vm_id).is_some_and(|vm| vm.state == VmState::Running);
        let client = &client;
        let spec = &record.spec;
        async move { running && healthy(client, spec, instance).await }
    }))
    .await;
    for (instance, ready) in record.instances.iter_mut().zip(checks) {
        instance.healthy = ready;
    }
}

async fn execute(state: &AppState, record: &mut MicroServiceDeployment) -> Result<(), String> {
    loop {
        let repair = record
            .recovery
            .as_ref()
            .expect("health")
            .repair
            .as_ref()
            .expect("repair")
            .clone();
        match repair.stage {
            ReplicaRepairStage::Paused => {
                routes::switch(state, record, routes::ready(record, Some(repair.vm_id))?).await?;
                ports(state, record, repair.vm_id, false).await?;
                failed(record, chrono::Utc::now().timestamp());
                return save(state, record).await;
            }
            ReplicaRepairStage::Withdraw => {
                verify(state, record, repair.vm_id)?;
                routes::switch(state, record, routes::ready(record, Some(repair.vm_id))?).await?;
                ports(state, record, repair.vm_id, false).await?;
                if record.spec.domain.is_none()
                    && record
                        .instances
                        .iter()
                        .any(|instance| instance.vm_id == repair.vm_id && instance.host_port != 0)
                {
                    tokio::time::sleep(Duration::from_secs(u64::from(record.spec.drain_timeout)))
                        .await;
                }
                advance(state, record, ReplicaRepairStage::Stop).await?;
            }
            ReplicaRepairStage::Stop => {
                routes::before_stop(state, record, repair.vm_id).await?;
                stop(state, record, repair.vm_id).await?;
                advance(
                    state,
                    record,
                    if vm(state, repair.vm_id).is_some() {
                        ReplicaRepairStage::Start
                    } else {
                        ReplicaRepairStage::Replace
                    },
                )
                .await?;
            }
            ReplicaRepairStage::Start => {
                routes::before_stop(state, record, repair.vm_id).await?;
                if start(state, record, repair.vm_id).await.is_ok() {
                    advance(state, record, ReplicaRepairStage::Publish).await?;
                } else {
                    advance(state, record, ReplicaRepairStage::Replace).await?;
                }
            }
            ReplicaRepairStage::Replace => {
                routes::before_stop(state, record, repair.vm_id).await?;
                stop(state, record, repair.vm_id).await?;
                // Retry the exact reserved candidate after a crash; never create an untracked VM.
                let id = if let Some(id) = repair.replacement_id {
                    id
                } else {
                    let id = Uuid::new_v4();
                    let old = record
                        .instances
                        .iter()
                        .find(|instance| instance.vm_id == repair.vm_id)
                        .ok_or("Failed replica ownership is missing")?;
                    record.instances.push(ServiceInstance {
                        vm_id: id,
                        ipv4: String::new(),
                        host_port: old.host_port,
                        state: VmState::Created,
                        healthy: false,
                    });
                    record
                        .recovery
                        .as_mut()
                        .unwrap()
                        .repair
                        .as_mut()
                        .unwrap()
                        .replacement_id = Some(id);
                    save(state, record).await?;
                    id
                };
                start(state, record, id).await?;
                advance(state, record, ReplicaRepairStage::Publish).await?;
            }
            ReplicaRepairStage::Publish => {
                refresh(state, record).await;
                let id = repair.replacement_id.unwrap_or(repair.vm_id);
                if !record
                    .instances
                    .iter()
                    .any(|instance| instance.vm_id == id && instance.healthy)
                {
                    return Err("Repaired replica lost readiness".into());
                }
                routes::switch(state, record, routes::ready(record, None)?).await?;
                ports(state, record, id, true).await?;
                advance(state, record, ReplicaRepairStage::Cleanup).await?;
            }
            ReplicaRepairStage::Cleanup => {
                routes::before_cleanup(state, record).await?;
                if repair.replacement_id.is_some() {
                    stop(state, record, repair.vm_id).await?;
                    if vm(state, repair.vm_id).is_some() {
                        vms::delete_vm(
                            State(state.clone()),
                            Extension(RequestId(record.id)),
                            Path(repair.vm_id.to_string()),
                        )
                        .await
                        .map_err(|_| "Could not delete retired recovery replica")?;
                    }
                    record
                        .instances
                        .retain(|instance| instance.vm_id != repair.vm_id);
                }
                let health = record.recovery.as_mut().unwrap();
                let mut status = health.replicas.remove(&repair.vm_id).unwrap_or_default();
                status.failures = 0;
                status.successes = 2;
                status.healthy_since = Some(chrono::Utc::now().timestamp());
                health
                    .replicas
                    .insert(repair.replacement_id.unwrap_or(repair.vm_id), status);
                health.repair = None;
                record.error = None;
                let phase = if record.instances.len() == usize::from(record.spec.replicas)
                    && record.instances.iter().all(|instance| instance.healthy)
                {
                    ServicePhase::Running
                } else {
                    ServicePhase::Degraded
                };
                return super::super::phase(
                    state,
                    record,
                    phase,
                    "Replica recovery complete; healthy instances retained",
                )
                .await;
            }
        }
    }
}

#[cfg(test)]
mod tests;

//! Owned Nginx transitions and verified connection drain.

use super::*;

pub(super) fn endpoints(deployment: &MicroServiceDeployment) -> Result<Vec<SocketAddr>, String> {
    endpoints_skipping(deployment, &[])
}
pub(super) fn endpoints_skipping(
    deployment: &MicroServiceDeployment,
    skipped: &[Uuid],
) -> Result<Vec<SocketAddr>, String> {
    deployment
        .instances
        .iter()
        .filter(|instance| instance.healthy && !skipped.contains(&instance.vm_id))
        .map(|instance| {
            format!("{}:{}", instance.ipv4, deployment.spec.port)
                .parse()
                .map_err(|_| "Invalid Deploy upstream".into())
        })
        .collect()
}
pub(super) async fn inspect(state: &AppState) -> Result<Snapshot, String> {
    state
        .network
        .pipe(Operation::Inspect)
        .await
        .map_err(|_| "Cannot inspect Nginx; owned VMs are retained".into())
}
pub(super) fn current_route<'a>(
    snapshot: &'a Snapshot,
    deployment: &MicroServiceDeployment,
) -> Result<&'a firecrab_helper_protocol::pipe::Route, String> {
    let host = deployment
        .spec
        .domain
        .as_ref()
        .ok_or("Deploy requires a domain")?;
    let route = snapshot
        .config
        .as_ref()
        .and_then(|config| config.routes.iter().find(|route| &route.host == host))
        .ok_or("Deploy route is missing; inspect Nginx before retrying")?;
    let previous = &deployment.update.as_ref().expect("journal").previous;
    if route.owner.as_ref().is_none_or(|owner| {
        owner.service_id != deployment.service_id
            || (owner.deployment_id != deployment.id && owner.deployment_id != previous.id)
    }) {
        return Err("Deploy route belongs to a different operation; VMs retained".into());
    }
    let mut owned = endpoints(deployment)?;
    owned.extend(endpoints(previous)?);
    if route
        .upstreams
        .iter()
        .any(|address| !owned.contains(address))
    {
        return Err("Nginx has unexpected Deploy upstreams; VMs retained".into());
    }
    Ok(route)
}
pub(super) fn worker_key(worker: &IngressWorker) -> String {
    format!("{}:{}:{}", worker.boot_id, worker.pid, worker.start_time)
}

pub(super) async fn begin_transition(
    state: &AppState,
    deployment: &mut MicroServiceDeployment,
    next: Vec<SocketAddr>,
    retiring: Vec<Uuid>,
    restoring: bool,
) -> Result<(), String> {
    let snapshot = inspect(state).await?;
    let route = current_route(&snapshot, deployment)?;
    let workers = snapshot
        .ingress_workers
        .as_ref()
        .filter(|workers| !workers.is_empty())
        .ok_or("Cannot verify Nginx workers; VMs retained")?;
    let update = deployment.update.as_mut().expect("journal");
    update.transition = Some(ServiceRouteTransition {
        expected_deployment_id: route.owner.as_ref().expect("validated owner").deployment_id,
        expected_upstreams: route.upstreams.iter().map(ToString::to_string).collect(),
        next_deployment_id: if restoring {
            update.previous.id
        } else {
            deployment.id
        },
        next_upstreams: next.iter().map(ToString::to_string).collect(),
        workers: workers.iter().map(worker_key).collect(),
        confirmed: false,
    });
    update.retiring = retiring;
    deployment.domain_route_owned = true;
    phase(
        state,
        deployment,
        ServicePhase::SwitchingTraffic,
        "Validating and gracefully reloading Nginx",
    )
    .await
}

pub(super) async fn finish_transition(
    state: &AppState,
    deployment: &mut MicroServiceDeployment,
) -> Result<(), String> {
    let transition = deployment
        .update
        .as_ref()
        .expect("journal")
        .transition
        .clone()
        .ok_or("Missing Nginx transition journal")?;
    if !transition.confirmed {
        let parse = |addresses: &[String]| {
            addresses
                .iter()
                .map(|address| {
                    address
                        .parse::<SocketAddr>()
                        .map_err(|_| "Invalid recorded Nginx upstream".to_owned())
                })
                .collect::<Result<Vec<_>, _>>()
        };
        let request = ServiceRouteRequest::Replace {
            expected_owner: ServiceRouteOwner {
                service_id: deployment.service_id,
                deployment_id: transition.expected_deployment_id,
            },
            expected_upstreams: parse(&transition.expected_upstreams)?,
            owner: ServiceRouteOwner {
                service_id: deployment.service_id,
                deployment_id: transition.next_deployment_id,
            },
            host: deployment
                .spec
                .domain
                .clone()
                .ok_or("Missing Deploy domain")?,
            upstreams: parse(&transition.next_upstreams)?,
        };
        state
            .network
            .pipe(Operation::ManageServiceRoute { request })
            .await
            .map_err(|_| "Nginx did not confirm the Deploy transition; VMs retained")?;
        let update = deployment.update.as_mut().expect("journal");
        update.routed_new = transition.next_deployment_id == deployment.id;
        update.previous.domain_route_owned = !update.routed_new;
        deployment.domain_route_owned = update.routed_new;
        update.transition.as_mut().expect("transition").confirmed = true;
        save(state, deployment).await?;
    }
    phase(
        state,
        deployment,
        ServicePhase::Draining,
        "Nginx switched; waiting for previous workers to drain",
    )
    .await?;
    wait_workers(state, &transition.workers, deployment.spec.drain_timeout).await?;
    let snapshot = inspect(state).await?;
    let route = current_route(&snapshot, deployment)?;
    if route.owner.as_ref().map(|owner| owner.deployment_id) != Some(transition.next_deployment_id)
        || route
            .upstreams
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            != transition.next_upstreams
        || !snapshot
            .units
            .iter()
            .any(|unit| unit.unit == "firecrab-ingress.service" && unit.active_state == "active")
    {
        return Err("Nginx serving generation changed during drain; VMs retained".into());
    }
    let restoring = deployment.update.as_ref().expect("journal").restoring;
    let retiring = deployment
        .update
        .as_ref()
        .expect("journal")
        .retiring
        .clone();
    for id in retiring {
        remove_replica(state, deployment, !restoring, id).await?;
    }
    let update = deployment.update.as_mut().expect("journal");
    update.transition = None;
    update.retiring.clear();
    save(state, deployment).await
}

pub(super) async fn remove_replica(
    state: &AppState,
    deployment: &mut MicroServiceDeployment,
    previous: bool,
    id: Uuid,
) -> Result<(), String> {
    if !target(deployment, previous)
        .instances
        .iter()
        .any(|instance| instance.vm_id == id)
    {
        return Err("Deploy cleanup rejected an unowned VM".into());
    }
    let vm = state
        .vms
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&id)
        .cloned();
    if let Some(vm) = vm {
        if matches!(vm.state, VmState::Running | VmState::Paused) {
            let _ = vms::stop_vm(
                State(state.clone()),
                Extension(RequestId(deployment.id)),
                Path(id.to_string()),
            )
            .await
            .map_err(|_| "Could not stop retired Deploy VM; retry Deploy")?;
        }
        vms::delete_vm(
            State(state.clone()),
            Extension(RequestId(deployment.id)),
            Path(id.to_string()),
        )
        .await
        .map_err(|_| "Could not delete retired Deploy VM; retry Deploy")?;
    }
    target_mut(deployment, previous)
        .instances
        .retain(|instance| instance.vm_id != id);
    deployment
        .update
        .as_mut()
        .expect("journal")
        .retiring
        .retain(|retired| *retired != id);
    save(state, deployment).await
}

/// Stop an interrupted update without abandoning either generation's ownership.
pub(super) async fn finish_stop(
    state: &AppState,
    deployment: &mut MicroServiceDeployment,
) -> Result<(), String> {
    if deployment
        .update
        .as_ref()
        .expect("journal")
        .transition
        .is_none()
    {
        let snapshot = inspect(state).await?;
        let workers = snapshot
            .ingress_workers
            .as_ref()
            .ok_or("Cannot verify Nginx drain before Stop")?;
        let route = snapshot.config.as_ref().and_then(|config| {
            config
                .routes
                .iter()
                .find(|route| route.host == deployment.spec.domain.clone().unwrap_or_default())
        });
        let owner = if let Some(route) = route {
            current_route(&snapshot, deployment)?;
            route.owner.as_ref().expect("validated owner").deployment_id
        } else if deployment.update.as_ref().expect("journal").routed_new {
            deployment.id
        } else {
            deployment.update.as_ref().expect("journal").previous.id
        };
        deployment.update.as_mut().expect("journal").transition = Some(ServiceRouteTransition {
            expected_deployment_id: owner,
            expected_upstreams: vec![],
            next_deployment_id: owner,
            next_upstreams: vec![],
            workers: workers.iter().map(worker_key).collect(),
            confirmed: false,
        });
        save(state, deployment).await?;
    }
    let transition = deployment
        .update
        .as_ref()
        .expect("journal")
        .transition
        .clone()
        .expect("stop transition");
    if !transition.confirmed {
        let request = ServiceRouteRequest::Remove {
            owner: ServiceRouteOwner {
                service_id: deployment.service_id,
                deployment_id: transition.expected_deployment_id,
            },
            host: deployment
                .spec
                .domain
                .clone()
                .ok_or("Deploy Stop has no domain")?,
        };
        state
            .network
            .pipe(Operation::ManageServiceRoute { request })
            .await
            .map_err(|_| "Could not withdraw Deploy route; VMs retained")?;
        deployment.domain_route_owned = false;
        let update = deployment.update.as_mut().expect("journal");
        update.previous.domain_route_owned = false;
        update.transition.as_mut().expect("transition").confirmed = true;
        save(state, deployment).await?;
    }
    wait_workers(state, &transition.workers, deployment.spec.drain_timeout).await?;
    for previous in [false, true] {
        while let Some(instance) = target(deployment, previous).instances.last() {
            let id = instance.vm_id;
            remove_replica(state, deployment, previous, id).await?;
        }
        secrets::remove(state, target(deployment, previous)).await?;
    }
    deployment.update.as_mut().expect("journal").completed = true;
    deployment.update.as_mut().expect("journal").transition = None;
    deployment.error = None;
    phase(
        state,
        deployment,
        ServicePhase::Stopped,
        "Deploy stopped; both generations drained and removed",
    )
    .await
}

pub(super) async fn wait_workers(
    state: &AppState,
    previous: &[String],
    timeout: u32,
) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(u64::from(timeout)), async {
        loop {
            let snapshot = inspect(state).await?;
            if !snapshot.ingress_graceful_drain { return Err("Nginx drain configuration changed; VMs retained".into()); }
            let workers = snapshot.ingress_workers.ok_or("Nginx drain status is unavailable")?;
            if !workers.iter().any(|worker| worker.draining || previous.contains(&worker_key(worker))) { return Ok::<_, String>(()); }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }).await.map_err(|_| "Nginx drainTimeout expired; existing connections and VMs are retained. Retry Deploy after they finish".to_owned())?
}

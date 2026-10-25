//! Recovery routes retain one deployment owner and journal empty-route withdrawal too.

use super::*;
use firecrab_api_types::ServiceRouteTransition;
use firecrab_helper_protocol::pipe::{Operation, ServiceRouteOwner, ServiceRouteRequest, Snapshot};
use std::net::SocketAddr;

fn owned_route<'a>(
    snapshot: &'a Snapshot,
    record: &MicroServiceDeployment,
) -> Result<Option<&'a firecrab_helper_protocol::pipe::Route>, String> {
    let route = snapshot.config.as_ref().and_then(|config| {
        config
            .routes
            .iter()
            .find(|route| Some(&route.host) == record.spec.domain.as_ref())
    });
    if let Some(route) = route {
        let addresses = record
            .instances
            .iter()
            .filter_map(|instance| {
                format!("{}:{}", instance.ipv4, record.spec.port)
                    .parse::<SocketAddr>()
                    .ok()
            })
            .collect::<BTreeSet<_>>();
        if route.owner.as_ref() != Some(&owner(record))
            || route
                .upstreams
                .iter()
                .any(|address| !addresses.contains(address))
        {
            return Err("Replica recovery found a foreign route; VMs retained".into());
        }
    }
    Ok(route)
}

fn owner(record: &MicroServiceDeployment) -> ServiceRouteOwner {
    ServiceRouteOwner {
        service_id: record.service_id,
        deployment_id: record.id,
    }
}

async fn inspect(state: &AppState) -> Result<Snapshot, String> {
    let snapshot = state
        .network
        .pipe(Operation::Inspect)
        .await
        .map_err(|_| "Cannot inspect recovery ingress")?;
    if !snapshot.ingress_graceful_drain
        || snapshot.ingress_workers.as_ref().is_none_or(Vec::is_empty)
        || !snapshot
            .units
            .iter()
            .any(|unit| unit.unit == "firecrab-ingress.service" && unit.active_state == "active")
    {
        return Err("Replica recovery requires verifiable Nginx drain".into());
    }
    Ok(snapshot)
}

pub(super) async fn switch(
    state: &AppState,
    record: &mut MicroServiceDeployment,
    next: Vec<SocketAddr>,
) -> Result<(), String> {
    if record.spec.domain.is_none() {
        return Ok(());
    }
    if record
        .recovery
        .as_ref()
        .and_then(|health| health.repair.as_ref())
        .and_then(|repair| repair.transition.as_ref())
        .is_none()
    {
        let snapshot = inspect(state).await?;
        let route = owned_route(&snapshot, record)?;
        let current = route.map_or_else(Vec::new, |route| route.upstreams.clone());
        if current == next {
            if snapshot
                .ingress_workers
                .as_ref()
                .is_some_and(|workers| workers.iter().any(|worker| worker.draining))
            {
                return Err("An earlier Nginx generation is still draining".into());
            }
            record.domain_route_owned = route.is_some();
            return save(state, record).await;
        }
        let workers = snapshot.ingress_workers.as_ref().expect("verified workers");
        record
            .recovery
            .as_mut()
            .expect("health")
            .repair
            .as_mut()
            .expect("repair")
            .transition = Some(ServiceRouteTransition {
            expected_deployment_id: record.id,
            expected_upstreams: current.iter().map(ToString::to_string).collect(),
            next_deployment_id: record.id,
            next_upstreams: next.iter().map(ToString::to_string).collect(),
            workers: workers.iter().map(deploy::routes::worker_key).collect(),
            confirmed: false,
        });
        save(state, record).await?;
    }
    let transition = record
        .recovery
        .as_ref()
        .expect("health")
        .repair
        .as_ref()
        .expect("repair")
        .transition
        .clone()
        .expect("transition");
    let parse = |values: &[String]| {
        values
            .iter()
            .map(|value| {
                value
                    .parse::<SocketAddr>()
                    .map_err(|_| "Invalid recovery upstream".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()
    };
    let expected = parse(&transition.expected_upstreams)?;
    let next = parse(&transition.next_upstreams)?;
    let snapshot = inspect(state).await?;
    let current = owned_route(&snapshot, record)?;
    let addresses = current.map_or_else(Vec::new, |route| route.upstreams.clone());
    if addresses != next {
        if addresses != expected {
            return Err("Recovery route generation changed; VMs retained".into());
        }
        let request = if current.is_none() {
            if next.is_empty() {
                return Err("Recovery route is missing; VMs retained".into());
            }
            ServiceRouteRequest::Publish {
                owner: owner(record),
                host: record.spec.domain.clone().unwrap(),
                upstreams: next.clone(),
            }
        } else {
            ServiceRouteRequest::Replace {
                expected_owner: owner(record),
                expected_upstreams: expected,
                owner: owner(record),
                host: record.spec.domain.clone().unwrap(),
                upstreams: next.clone(),
            }
        };
        state
            .network
            .pipe(Operation::ManageServiceRoute { request })
            .await
            .map_err(|_| "Nginx did not confirm replica recovery; VMs retained")?;
    }
    record.domain_route_owned = true;
    record
        .recovery
        .as_mut()
        .unwrap()
        .repair
        .as_mut()
        .unwrap()
        .transition
        .as_mut()
        .unwrap()
        .confirmed = true;
    save(state, record).await?;
    deploy::routes::wait_workers(state, &transition.workers, record.spec.drain_timeout).await?;
    let snapshot = inspect(state).await?;
    let addresses =
        owned_route(&snapshot, record)?.map_or_else(Vec::new, |route| route.upstreams.clone());
    if addresses != next {
        return Err("Recovery route changed during drain; VMs retained".into());
    }
    record
        .recovery
        .as_mut()
        .unwrap()
        .repair
        .as_mut()
        .unwrap()
        .transition = None;
    save(state, record).await
}

pub(super) fn ready(
    record: &MicroServiceDeployment,
    excluded: Option<Uuid>,
) -> Result<Vec<SocketAddr>, String> {
    record
        .instances
        .iter()
        .filter(|instance| instance.healthy && Some(instance.vm_id) != excluded)
        .map(|instance| {
            format!("{}:{}", instance.ipv4, record.spec.port)
                .parse()
                .map_err(|_| "Invalid ready replica address".into())
        })
        .collect()
}

#[cfg(test)]
mod tests;

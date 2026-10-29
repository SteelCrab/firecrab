//! Recovery routes retain one deployment owner and journal empty-route withdrawal too.

use super::*;
use firecrab_api_types::ServiceRouteTransition;
use firecrab_helper_protocol::pipe::{Operation, ServiceRouteOwner, ServiceRouteRequest, Snapshot};
use std::net::SocketAddr;

fn uses_ingress(record: &MicroServiceDeployment) -> bool {
    record.spec.domain.is_some() || record.internal_route_owned
}

fn hostname(record: &MicroServiceDeployment) -> String {
    record
        .spec
        .domain
        .clone()
        .unwrap_or_else(|| format!("{}.internal", record.spec.name))
}

fn owned_route<'a>(
    snapshot: &'a Snapshot,
    record: &MicroServiceDeployment,
) -> Result<Option<&'a firecrab_helper_protocol::pipe::Route>, String> {
    let route = snapshot.config.as_ref().and_then(|config| {
        config
            .routes
            .iter()
            .find(|route| route.host == hostname(record))
    });
    if let Some(route) = route {
        if record.internal_route_owned
            && snapshot
                .config
                .as_ref()
                .unwrap()
                .routes
                .iter()
                .find(|other| other.host == format!("{}.internal", record.spec.name))
                .is_none_or(|other| {
                    other.owner.as_ref() != Some(&owner(record))
                        || other.upstreams != route.upstreams
                })
        {
            return Err("Private recovery route changed; VMs retained".into());
        }
        let addresses = record
            .instances
            .iter()
            .filter_map(|instance| {
                format!("{}:{}", instance.ipv4, record.spec.port)
                    .parse::<SocketAddr>()
                    .ok()
            })
            .collect::<BTreeSet<_>>();
        if snapshot
            .config
            .as_ref()
            .unwrap()
            .routes
            .iter()
            .filter(|other| {
                other
                    .owner
                    .as_ref()
                    .is_some_and(|owner| owner.service_id == record.service_id)
            })
            .any(|other| {
                other.owner.as_ref() != Some(&owner(record))
                    || other.upstreams != route.upstreams
                    || other
                        .upstreams
                        .iter()
                        .any(|address| !addresses.contains(address))
            })
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
    if !uses_ingress(record) {
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
            record.domain_route_owned = record.spec.domain.is_some() && route.is_some();
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
                host: hostname(record),
                upstreams: next.clone(),
            }
        } else {
            ServiceRouteRequest::Replace {
                expected_owner: owner(record),
                expected_upstreams: expected,
                owner: owner(record),
                host: hostname(record),
                upstreams: next.clone(),
            }
        };
        state
            .network
            .pipe(Operation::ManageServiceRoute { request })
            .await
            .map_err(|_| "Nginx did not confirm replica recovery; VMs retained")?;
    }
    record.domain_route_owned = record.spec.domain.is_some();
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

/// A restart can resume long after drain was first confirmed. Recheck ownership.
pub(super) async fn before_stop(
    state: &AppState,
    record: &MicroServiceDeployment,
    id: Uuid,
) -> Result<(), String> {
    if !uses_ingress(record) {
        return Ok(());
    }
    let snapshot = inspect(state).await?;
    let route = owned_route(&snapshot, record)?.ok_or("Recovery route disappeared; VM retained")?;
    let target = record
        .instances
        .iter()
        .find(|instance| instance.vm_id == id)
        .and_then(|instance| {
            format!("{}:{}", instance.ipv4, record.spec.port)
                .parse::<SocketAddr>()
                .ok()
        });
    if target.is_some_and(|address| route.upstreams.contains(&address))
        || snapshot
            .ingress_workers
            .as_ref()
            .unwrap()
            .iter()
            .any(|worker| worker.draining)
    {
        return Err("Recovery endpoint is still routed or draining; VM retained".into());
    }
    Ok(())
}

pub(super) async fn before_cleanup(
    state: &AppState,
    record: &MicroServiceDeployment,
) -> Result<(), String> {
    if !uses_ingress(record) {
        return Ok(());
    }
    let snapshot = inspect(state).await?;
    let route = owned_route(&snapshot, record)?.ok_or("Recovery route disappeared; VM retained")?;
    if route.upstreams != ready(record, None)? {
        return Err("Recovery serving generation changed before cleanup; VM retained".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;

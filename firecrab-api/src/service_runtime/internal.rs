//! Private DNS and ingress share the deployment's durable ownership lifetime.

use super::*;
use firecrab_helper_protocol::pipe::{Operation, ServiceRouteOwner, ServiceRouteRequest};

pub(super) async fn preflight(
    state: &AppState,
    service_id: Uuid,
    spec: &MicroServiceSpec,
    request_id: Uuid,
) -> Result<(), AppError> {
    domain::preflight_route(
        state,
        service_id,
        &format!("{}.internal", spec.name),
        "name",
        request_id,
    )
    .await
}

fn owner(deployment: &MicroServiceDeployment) -> ServiceRouteOwner {
    ServiceRouteOwner {
        service_id: deployment.service_id,
        deployment_id: deployment.id,
    }
}

pub(super) async fn publish(
    state: &AppState,
    deployment: &mut MicroServiceDeployment,
) -> Result<(), String> {
    let network_id = deployment
        .request
        .micro_network_id
        .ok_or("Internal routing needs the deployment MicroNetwork")?;
    let network = state
        .store
        .micro_network(network_id)
        .map_err(|_| "Could not read service MicroNetwork")?
        .ok_or("Service MicroNetwork no longer exists")?;
    let subnet = crate::ipam::SubnetSpec::parse(network.id, &network.subnet_cidr)
        .ok_or("Service MicroNetwork has an invalid subnet")?;
    let upstreams = deployment
        .instances
        .iter()
        .map(|instance| {
            if !instance.healthy {
                return Err("Only healthy replicas can receive internal traffic".to_owned());
            }
            format!("{}:{}", instance.ipv4, deployment.spec.port)
                .parse()
                .map_err(|_| "Replica has no valid application address".to_owned())
        })
        .collect::<Result<Vec<_>, String>>()?;
    let request = ServiceRouteRequest::PublishInternal {
        security_group: crate::network_policy::resolve(
            &state.store,
            deployment.spec.security_group_id,
            Some(network_id),
        )?,
        owner: owner(deployment),
        name: deployment.spec.name.clone(),
        network: subnet.helper_spec(network.internet_enabled, network.uplink, network.dns_mode),
        upstreams,
    };
    deployment.internal_route_owned = true;
    phase(
        state,
        deployment,
        ServicePhase::ConnectingDomain,
        format!(
            "Connecting {}.internal to healthy replicas",
            deployment.spec.name
        ),
    )
    .await?;
    domain::apply(state, request, deployment.service_id).await
}

pub(super) async fn withdraw(
    state: &AppState,
    deployment: &mut MicroServiceDeployment,
) -> Result<(), String> {
    if !deployment.internal_route_owned {
        return Ok(());
    }
    state.network.pipe(Operation::ManageServiceRoute { request: ServiceRouteRequest::Remove {
        owner: owner(deployment), host: format!("{}.internal", deployment.spec.name),
    }}).await.map_err(|error| {
        tracing::warn!(%error, service_id = %deployment.service_id, "could not withdraw internal service route");
        "Could not withdraw internal DNS and routing; retry Stop. Instances are retained until cleanup succeeds.".to_owned()
    })?;
    deployment.internal_route_owned = false;
    deployment
        .log
        .push("Withdrew internal DNS and service route".into());
    save(state, deployment).await
}

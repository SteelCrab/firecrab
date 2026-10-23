//! Domain routes have the same durable ownership and cleanup lifetime as service replicas.

use super::*;
use crate::network::NetworkError;
use firecrab_helper_protocol::network::HelperFailure;
use firecrab_helper_protocol::pipe::{Operation, ServiceRouteOwner, ServiceRouteRequest};

/// Fail before importing an image if the engine is unavailable or the domain is occupied.
pub(super) async fn preflight(
    state: &AppState,
    service_id: Uuid,
    spec: &MicroServiceSpec,
    request_id: Uuid,
) -> Result<(), AppError> {
    let Some(host) = &spec.domain else {
        return Ok(());
    };
    let snapshot = state
        .network
        .pipe(Operation::Inspect)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "could not inspect service ingress");
            AppError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "pipe_unavailable",
                "Cannot check domain routing. Check the Firecrab helper and ingress engine.",
                request_id,
            )
        })?;
    let message = if !snapshot.nginx_available {
        Some(
            "Install the ingress engine before deploying a domain. Reinstall the Firecrab host bundle.",
        )
    } else if snapshot.config.as_ref().is_some_and(|config| {
        config.routes.iter().any(|route| {
            route.host == *host
                && route
                    .owner
                    .as_ref()
                    .is_none_or(|owner| owner.service_id != service_id)
        })
    }) {
        Some("This domain is already used by another Pipe route or service. Choose another domain.")
    } else {
        None
    };
    if let Some(message) = message {
        return Err(AppError::validation(
            BTreeMap::from([("domain".into(), message.into())]),
            request_id,
        ));
    }
    Ok(())
}

fn owner(deployment: &MicroServiceDeployment) -> ServiceRouteOwner {
    ServiceRouteOwner {
        service_id: deployment.service_id,
        deployment_id: deployment.id,
    }
}

/// Persist ownership before publication so a lost response or API restart cannot orphan a route.
pub(super) async fn publish(
    state: &AppState,
    deployment: &mut MicroServiceDeployment,
) -> Result<(), String> {
    let Some(host) = deployment.spec.domain.clone() else {
        return Ok(());
    };
    let upstreams = deployment
        .instances
        .iter()
        .map(|instance| {
            if !instance.healthy {
                return Err("Only healthy replicas can receive domain traffic".to_owned());
            }
            format!("{}:{}", instance.ipv4, deployment.spec.port)
                .parse()
                .map_err(|_| "Replica has no valid application address".to_owned())
        })
        .collect::<Result<Vec<std::net::SocketAddr>, String>>()?;
    deployment.domain_route_owned = true;
    phase(
        state,
        deployment,
        ServicePhase::ConnectingDomain,
        format!("Connecting {host} to healthy replicas"),
    )
    .await?;
    let request = ServiceRouteRequest::Publish {
        owner: owner(deployment),
        host,
        upstreams,
    };
    let mut retries = 0;
    loop {
        match state
            .network
            .pipe(Operation::ManageServiceRoute {
                request: request.clone(),
            })
            .await
        {
            Ok(_) => return Ok(()),
            Err(error)
                if retries < 2
                    && matches!(
                        &error,
                        NetworkError::Helper(HelperFailure::Internal { detail })
                            if detail.trim_end() == "[ERROR] another pipe change is running"
                    ) =>
            {
                retries += 1;
                tokio::time::sleep(Duration::from_secs(retries)).await;
            }
            Err(error) => {
                tracing::warn!(%error, service_id = %deployment.service_id, "could not publish service domain");
                return Err(
                    "Could not confirm domain routing. Check Pipe ingress and domain conflicts."
                        .to_owned(),
                );
            }
        }
    }
}

/// Withdraw traffic before draining or deleting VMs. Keep ownership on uncertain results.
pub(super) async fn withdraw(
    state: &AppState,
    deployment: &mut MicroServiceDeployment,
) -> Result<(), String> {
    if !deployment.domain_route_owned {
        return Ok(());
    }
    let host = deployment
        .spec
        .domain
        .clone()
        .ok_or("Owned domain route has no hostname")?;
    let request = ServiceRouteRequest::Remove {
        owner: owner(deployment),
        host,
    };
    state.network.pipe(Operation::ManageServiceRoute { request }).await.map_err(|error| {
        tracing::warn!(%error, service_id = %deployment.service_id, "could not withdraw service domain");
        "Could not withdraw the service domain. Check Pipe and remove Quick Tunnels referencing this domain, then retry Stop. Instances are retained until route cleanup succeeds.".to_owned()
    })?;
    deployment.domain_route_owned = false;
    deployment.log.push("Withdrew service domain".into());
    save(state, deployment).await
}

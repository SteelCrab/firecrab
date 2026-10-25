//! Controller-owned services: one release identity, durable fan-out and fenced observations.
mod credentials;
mod handlers;
mod worker;
pub(crate) use handlers::router;

use super::{endpoint_url, now_ms, stored};
use crate::{persistence::ClusterError, state::AppState};
use firecrab_api_types::*;
use std::{collections::BTreeSet, time::Duration};
use uuid::Uuid;

pub(crate) fn validate_spec(spec: &mut ClusterServiceSpec) -> Result<(), ClusterError> {
    crate::handlers::micro_services::validate(&spec.service, Uuid::nil())
        .map_err(|_| ClusterError::Invalid("invalid MicroService configuration".into()))?;
    let image = crate::oci::ImageReference::parse(&spec.service.image)
        .map_err(|_| ClusterError::Invalid("invalid OCI image reference".into()))?;
    if !matches!(image.version, crate::oci::ImageVersion::Digest(_)) {
        return Err(ClusterError::Invalid(
            "controller service image must be pinned with @sha256:<digest>".into(),
        ));
    }
    if spec.service.domain.is_some()
        || !spec.service.secrets.is_empty()
        || spec.service.storage_connection_id.is_some()
        || spec.service.security_group_id.is_some()
        || !spec.service.queues.is_empty()
    {
        return Err(ClusterError::Invalid("host-local bindings and cross-host domain routing are not supported; use explicit networks and host ports".into()));
    }
    if spec
        .credentials
        .values()
        .any(|r| r.secret_id.is_nil() || r.version == 0)
    {
        return Err(ClusterError::Invalid(
            "credentials require a controller PipeSecret UUID and a positive version".into(),
        ));
    }
    let mut local = spec.service.clone();
    local.secrets = spec
        .credentials
        .iter()
        .map(|(key, r)| (key.clone(), r.secret_id))
        .collect();
    crate::handlers::micro_services::validate(&local, Uuid::nil()).map_err(|_| {
        ClusterError::Invalid("invalid or conflicting credential environment names".into())
    })?;
    if !spec.state.requests_duplicate_safe
        || spec.state.background_work == ClusterServiceBackgroundWork::Exclusive
    {
        return Err(ClusterError::Invalid("replicated requests and background work must be duplicate-safe; exclusive work requires ownership and fencing (#462)".into()));
    }
    let external = [
        &spec.state.sessions,
        &spec.state.uploads,
        &spec.state.database,
    ];
    let has_external = external.iter().any(|key| key.is_some());
    if (spec.state.mode == ClusterServiceStateMode::ExternalState) != has_external {
        return Err(ClusterError::Invalid("external_state requires sessions, uploads or database; stateless_http cannot declare durable application state".into()));
    }
    for key in external.into_iter().flatten() {
        if !spec.service.env.contains_key(key) && !spec.credentials.contains_key(key) {
            return Err(ClusterError::Invalid(
                "external state must reference a configured environment name or scoped credential"
                    .into(),
            ));
        }
    }
    credentials::validate_external(&spec.state, &spec.service.env)?;
    if spec.targets.is_empty() || spec.targets.len() > 100 {
        return Err(ClusterError::Invalid("select 1-100 workload hosts".into()));
    }
    spec.targets.sort_by_key(|t| t.host_id);
    let mut hosts = BTreeSet::new();
    let mut replicas = 0u32;
    for target in &spec.targets {
        let mut local = spec.service.clone();
        local.replicas = target.replicas;
        if target.host_id.is_nil()
            || !hosts.insert(target.host_id)
            || !(1..=100).contains(&target.replicas)
            || target.request.micro_network_id.is_none_or(|id| id.is_nil())
            || !crate::service_runtime::deployment_fields(&local, &target.request, 5523).is_empty()
        {
            return Err(ClusterError::Invalid(
                "each target needs a distinct host, local network, replicas and valid host ports"
                    .into(),
            ));
        }
        replicas += u32::from(target.replicas);
    }
    if replicas != u32::from(spec.service.replicas) {
        return Err(ClusterError::Invalid(
            "target replica counts must sum to service.replicas".into(),
        ));
    }
    Ok(())
}

/// A live observation is complete only for this exact desired generation and owner.
fn host_satisfied(host: &ClusterServiceHost, id: Uuid) -> bool {
    if host.observed_generation != Some(host.intent.generation) || host.error.is_some() {
        return false;
    }
    match host.intent.desired {
        ClusterServiceDesired::Stopped => host.deployment.as_ref().is_none_or(|d| {
            d.service_id == id
                && d.phase == ServicePhase::Stopped
                && d.instances.is_empty()
                && !d.domain_route_owned
        }),
        ClusterServiceDesired::Running => host.deployment.as_ref().is_some_and(|d| {
            d.service_id == id
                && d.id == host.intent.deployment_id
                && d.spec == host.intent.spec
                && d.request == host.intent.request
                && d.phase == ServicePhase::Running
                && d.instances.len() == usize::from(host.intent.spec.replicas)
                && d.instances
                    .iter()
                    .all(|i| i.healthy && i.state == VmState::Running)
                && d.digest.as_deref()
                    == host
                        .intent
                        .spec
                        .image
                        .rsplit_once('@')
                        .map(|(_, digest)| digest)
        }),
    }
}

fn status(service: &mut ClusterService) {
    service.pending = service.hosts.iter().any(|h| !host_satisfied(h, service.id));
    service.degraded = service.hosts.iter().any(|h| {
        h.error.is_some()
            || h.deployment
                .as_ref()
                .is_some_and(|d| matches!(d.phase, ServicePhase::Failed | ServicePhase::Degraded))
    });
    service.observed = if service.degraded {
        ClusterServiceObserved::Degraded
    } else if service.pending {
        ClusterServiceObserved::Pending
    } else if service.desired == ClusterServiceDesired::Stopped {
        ClusterServiceObserved::Stopped
    } else {
        ClusterServiceObserved::Running
    };
}

async fn read_receipt(
    mut response: reqwest::Response,
) -> Result<ServiceWorkerReceipt, ClusterError> {
    if !response.status().is_success() {
        return Err(ClusterError::Task);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ClusterError::Task)? {
        if bytes.len().saturating_add(chunk.len()) > 1024 * 1024 {
            return Err(ClusterError::Task);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

async fn observe_host(
    state: &AppState,
    id: Uuid,
    host: &mut ClusterServiceHost,
) -> Result<(), ClusterError> {
    let store = state.store.clone();
    let owner = host.host_id;
    let endpoint = stored(move || store.cluster_endpoint(owner)).await?;
    let endpoint = endpoint_url(&endpoint)?;
    let client = state
        .cluster
        .client(owner)
        .map_err(|_| ClusterError::Task)?;
    let url = format!("{endpoint}/api/cluster/worker/services/{id}");
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|_| ClusterError::Task)?;
    let receipt = if response.status() == reqwest::StatusCode::NOT_FOUND {
        None
    } else {
        Some(read_receipt(response).await?)
    };
    if let Some(receipt) = receipt {
        accept_receipt(state, id, host, &receipt)?;
        if host_satisfied(host, id) {
            return Ok(());
        }
    }
    let credential_values = if host.intent.desired == ClusterServiceDesired::Running {
        credentials::values(state, &host.intent.credentials).await?
    } else {
        Default::default()
    };
    let receipt = read_receipt(
        client
            .put(&url)
            .json(&credentials::WorkerRequest {
                intent: host.intent.clone(),
                credential_values,
            })
            .send()
            .await
            .map_err(|_| ClusterError::Task)?,
    )
    .await?;
    accept_receipt(state, id, host, &receipt)
}

fn accept_receipt(
    state: &AppState,
    id: Uuid,
    host: &mut ClusterServiceHost,
    receipt: &ServiceWorkerReceipt,
) -> Result<(), ClusterError> {
    if receipt.id != id
        || Some(receipt.controller_id) != state.cluster.identity
        || receipt.intent.generation > host.intent.generation
        || (receipt.intent.generation == host.intent.generation && receipt.intent != host.intent)
        || receipt
            .deployment
            .as_ref()
            .is_some_and(|d| d.service_id != id || !receipt.deployment_ids.contains(&d.id))
    {
        return Err(ClusterError::Task);
    }
    host.observed_generation = Some(receipt.intent.generation);
    host.deployment = receipt.deployment.clone();
    host.error = receipt.error.clone();
    Ok(())
}

pub(crate) async fn reconcile_one(
    state: AppState,
    id: Uuid,
    force: bool,
) -> Result<(), ClusterError> {
    let store = state.store.clone();
    let Some((token, mut service)) =
        stored(move || store.cluster_service_claim(id, now_ms(), force)).await?
    else {
        return Ok(());
    };
    // Fan out concurrently; one unreachable host cannot starve another host's cleanup.
    futures_util::future::join_all(service.hosts.iter_mut().map(|host| async {
        if observe_host(&state, id, host).await.is_err() {
            host.error = Some("host result is unresolved; deployment ownership is retained".into());
            host.observed_generation = None;
        }
    }))
    .await;
    for host in &mut service.hosts {
        if host_satisfied(host, id) {
            host.reservation = if host.intent.desired == ClusterServiceDesired::Stopped {
                HostCapacity::default()
            } else {
                HostCapacity {
                    cpu: u32::from(host.intent.spec.cpu) * u32::from(host.intent.spec.replicas),
                    ram_mib: u64::from(host.intent.spec.ram) * u64::from(host.intent.spec.replicas),
                }
            };
        }
    }
    status(&mut service);
    let store = state.store.clone();
    stored(move || store.cluster_service_finish(&service, token, now_ms())).await
}

pub(crate) fn wake(state: AppState, id: Uuid) {
    tokio::spawn(async move {
        let _ = reconcile_one(state, id, true).await;
    });
}

pub(super) fn spawn(state: AppState) {
    tokio::spawn(async move {
        let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        loop {
            interval.tick().await;
            let available = permits.available_permits();
            if available == 0 {
                continue;
            }
            let store = state.store.clone();
            let Ok(ids) = stored(move || store.cluster_service_due(now_ms(), available)).await
            else {
                continue;
            };
            for id in ids {
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    break;
                };
                let state = state.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let _ = reconcile_one(state, id, false).await;
                });
            }
        }
    });
}

//! Single-controller host registration, durable placement and owner-only reconciliation.
pub(crate) mod services;
mod worker;
pub(crate) use worker::{check_capacity, protect_vm};
pub(crate) mod handlers;

use crate::{persistence::ClusterError, state::AppState};
use firecrab_api_types::*;
use firecrab_pki::TlsPaths;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use uuid::Uuid;

#[derive(Debug, Clone)]
/// Enrollment settings for one controller-authorized workload host.
pub(crate) struct WorkerConfig {
    /// Certificate UUID authorized to send worker intents.
    pub controller_id: Uuid,
    /// HTTPS controller origin for authenticated inventory reports.
    pub controller_url: String,
    /// HTTPS origin the controller uses to reach this host.
    pub endpoint: String,
    /// Operator-visible host name.
    pub name: String,
    /// Inventory labels used by exact-match placement constraints.
    pub labels: BTreeMap<String, String>,
    /// Explicit allocatable quotas include existing and controller-owned VM disks.
    pub disk_limits: BTreeMap<String, u64>,
}

#[derive(Debug, Default)]
/// Process-local transport and locks; durable ownership remains in SQLite.
pub(crate) struct Runtime {
    /// Local certificate UUID; absent for legacy loopback HTTP.
    pub identity: Option<Uuid>,
    /// Private-CA transport material for both RPC directions.
    pub paths: Option<TlsPaths>,
    /// Enrollment configuration; absent on a controller-only process.
    pub worker: Option<WorkerConfig>,
    /// Serializes managed receipts and local VM capacity mutations.
    pub admission: Arc<AsyncMutex<()>>,
    /// Per-placement worker serialization without retaining unused locks.
    locks: Mutex<HashMap<Uuid, Weak<AsyncMutex<()>>>>,
    /// Connection pools keyed by the expected certificate UUID.
    clients: Mutex<HashMap<Uuid, reqwest::Client>>,
    /// Bounds detached worker tasks that outlive their HTTP requests.
    worker_permits: OnceLock<Arc<Semaphore>>,
    #[cfg(test)]
    /// Fixture hardware inventory; local VM usage is still calculated live.
    pub report_override: Option<HostReport>,
}

impl Runtime {
    /// Validate all worker settings before loading or recovering host state.
    pub fn from_env(paths: Option<TlsPaths>) -> Result<Self, firecrab_pki::Error> {
        let identity = paths
            .as_ref()
            .map(|p| firecrab_pki::certificates(&p.cert).and_then(|c| firecrab_pki::host_id(&c[0])))
            .transpose()?;
        let mut runtime = Self {
            identity,
            paths,
            ..Self::default()
        };
        let values = [
            "FIRECRAB_CONTROLLER_ID",
            "FIRECRAB_CONTROLLER_URL",
            "FIRECRAB_HOST_ENDPOINT",
        ]
        .map(|key| std::env::var(key).ok());
        if values.iter().all(Option::is_none) {
            return Ok(runtime);
        }
        let [Some(id), Some(controller), Some(endpoint)] = values else {
            return Err(firecrab_pki::Error("set FIRECRAB_CONTROLLER_ID, FIRECRAB_CONTROLLER_URL and FIRECRAB_HOST_ENDPOINT together".into()));
        };
        let id = Uuid::parse_str(&id)
            .map_err(|_| firecrab_pki::Error("invalid controller UUID".into()))?;
        let controller_url =
            endpoint_url(&controller).map_err(|e| firecrab_pki::Error(e.to_string()))?;
        let endpoint = endpoint_url(&endpoint).map_err(|e| firecrab_pki::Error(e.to_string()))?;
        let local = identity.ok_or_else(|| {
            firecrab_pki::Error("worker enrollment requires mutual TLS configuration".into())
        })?;
        let labels = std::env::var("FIRECRAB_HOST_LABELS")
            .ok()
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(|_| {
                firecrab_pki::Error("FIRECRAB_HOST_LABELS must be a JSON string map".into())
            })?
            .unwrap_or_default();
        let name = std::env::var("FIRECRAB_HOST_NAME").unwrap_or_else(|_| local.to_string());
        if name.is_empty() || name.len() > 64 || !valid_labels(&labels) {
            return Err(firecrab_pki::Error(
                "invalid FIRECRAB_HOST_NAME or FIRECRAB_HOST_LABELS bounds".into(),
            ));
        }
        let disk_limits: BTreeMap<String, u64> = std::env::var("FIRECRAB_HOST_DISK_LIMITS")
            .ok()
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(|_| {
                firecrab_pki::Error(
                    "FIRECRAB_HOST_DISK_LIMITS must map storage IDs to GiB quotas".into(),
                )
            })?
            .unwrap_or_default();
        if disk_limits
            .iter()
            .any(|(id, limit)| id.is_empty() || id.len() > 128 || *limit > 1 << 40)
        {
            return Err(firecrab_pki::Error("invalid host disk quota bounds".into()));
        }
        runtime.worker = Some(WorkerConfig {
            controller_id: id,
            controller_url,
            endpoint,
            name,
            labels,
            disk_limits,
        });
        runtime.client(id)?;
        Ok(runtime)
    }

    /// Reuse a client that verifies the requested owner in addition to normal TLS checks.
    pub fn client(&self, id: Uuid) -> Result<reqwest::Client, firecrab_pki::Error> {
        let mut clients = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(client) = clients.get(&id) {
            return Ok(client.clone());
        }
        let paths = self
            .paths
            .as_ref()
            .ok_or_else(|| firecrab_pki::Error("cluster operations require mutual TLS".into()))?;
        let client = reqwest::Client::builder()
            .use_preconfigured_tls(paths.client_config_for_host(id)?)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(8))
            .pool_max_idle_per_host(2)
            .build()
            .map_err(|e| firecrab_pki::Error(e.to_string()))?;
        clients.insert(id, client.clone());
        Ok(client)
    }

    /// Serialize generations of one worker placement through its final acknowledgement.
    async fn lock_worker(&self, id: Uuid) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
            locks.retain(|_, v| v.strong_count() > 0);
            match locks.get(&id).and_then(Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(AsyncMutex::new(()));
                    locks.insert(id, Arc::downgrade(&lock));
                    lock
                }
            }
        };
        lock.lock_owned().await
    }
}

/// Controller receipt clock for heartbeat freshness and retry claims.
pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Run synchronous SQLite work away from the async executor.
pub(crate) async fn stored<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ClusterError> + Send + 'static,
) -> Result<T, ClusterError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ClusterError::Task)?
}

/// Validate and normalize a cluster HTTPS origin before enrollment or dispatch.
pub(crate) fn endpoint_url(value: &str) -> Result<String, ClusterError> {
    let url = reqwest::Url::parse(value)
        .map_err(|_| ClusterError::Invalid("endpoint must be an HTTPS origin".into()))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(ClusterError::Invalid(
            "endpoint must be an HTTPS origin without credentials, path, query or fragment".into(),
        ));
    }
    Ok(url.as_str().trim_end_matches('/').into())
}

/// Bound operator labels before persisting or comparing inventory.
fn valid_labels(labels: &BTreeMap<String, String>) -> bool {
    labels.len() <= 64
        && labels
            .iter()
            .all(|(k, v)| !k.is_empty() && k.len() <= 128 && v.len() <= 128)
}

/// Validate authenticated inventory and reject ambiguous local resource IDs.
pub(crate) fn validate_report(report: &mut HostReport) -> Result<(), ClusterError> {
    report.endpoint = endpoint_url(&report.endpoint)?;
    if report.id.is_nil()
        || report.name.is_empty()
        || report.name.len() > 64
        || report.architecture.len() > 32
        || !(1..=4096).contains(&report.capacity.cpu)
        || report.capacity.ram_mib == 0
        || report.capacity.ram_mib > 1 << 40
        || report.networks.len() > 1024
        || report.storages.len() > 128
        || report.images.len() > 1024
        || !valid_labels(&report.labels)
    {
        return Err(ClusterError::Invalid(
            "invalid host identity, capacity or inventory bounds".into(),
        ));
    }
    let mut networks = BTreeSet::new();
    let mut storages = BTreeSet::new();
    if report
        .networks
        .iter()
        .any(|n| n.id.is_nil() || !networks.insert(n.id))
        || report.storages.iter().any(|s| {
            s.id.is_empty()
                || s.id.len() > 128
                || s.capacity_gib > 1 << 40
                || !storages.insert(&s.id)
        })
    {
        return Err(ClusterError::Invalid(
            "host inventory has invalid or repeated resource IDs".into(),
        ));
    }
    Ok(())
}

/// Canonicalize candidates and enforce bounded VM and boot-image requirements.
pub(crate) fn validate_spec(spec: &mut PlacementSpec) -> Result<(), ClusterError> {
    spec.targets.sort_by_key(|t| t.network.host_id);
    let mut ids = BTreeSet::new();
    let image = &spec.image;
    if spec.name.is_empty()
        || !spec.name.as_bytes()[0].is_ascii_alphanumeric()
        || spec.name.len() > 64
        || !spec
            .name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || !(1..=64).contains(&spec.cpu)
        || !(128..=32_768).contains(&spec.ram)
        || !spec.ram.is_power_of_two()
        || spec.disk_gb == 0
        || spec.targets.is_empty()
        || spec.targets.len() > 64
        || !valid_labels(&spec.required_labels)
        || image.alias.is_empty()
        || image.version.is_empty()
        || [
            &image.kernel_sha256,
            &image.rootfs_sha256,
            &image.boot_args_sha256,
        ]
        .into_iter()
        .chain(image.initrd_sha256.as_ref())
        .any(|s| {
            s.len() != 64
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        || spec.targets.iter().any(|t| {
            t.network.host_id.is_nil()
                || t.network.host_id != t.storage.host_id
                || !ids.insert(t.network.host_id)
                || Uuid::parse_str(&t.network.resource_id).is_err()
                || t.storage.resource_id.is_empty()
        })
    {
        return Err(ClusterError::Invalid(
            "invalid placement limits, image fingerprint or host-local resource references".into(),
        ));
    }
    Ok(())
}

pub(crate) fn image(version: &crate::templates::TemplateVersion) -> HostImage {
    HostImage {
        alias: version.name.clone(),
        version: version.version.clone(),
        kernel_sha256: version.kernel.sha256().into(),
        initrd_sha256: version.initrd.as_ref().map(|v| v.sha256().to_owned()),
        rootfs_sha256: version.rootfs.sha256().into(),
        boot_args_sha256: version.boot_args_sha256(),
    }
}

/// Inventory excludes controller-owned VMs from unmanaged usage to avoid double charging.
pub(crate) async fn inventory(state: &AppState) -> Result<HostReport, ClusterError> {
    #[cfg(test)]
    if let Some(report) = &state.cluster.report_override {
        let mut report = report.clone();
        fill_unmanaged(state, &mut report).await?;
        return Ok(report);
    }
    let config = state
        .cluster
        .worker
        .as_ref()
        .ok_or_else(|| ClusterError::Invalid("host is not enrolled as a worker".into()))?;
    let id = state
        .cluster
        .identity
        .ok_or_else(|| ClusterError::Invalid("host identity is missing".into()))?;
    let axum::Json(status) =
        crate::handlers::network::get_host_status(axum::extract::State(state.clone())).await;
    let store = state.store.clone();
    let networks = stored(move || {
        let networks = store
            .list_micro_networks()?
            .into_iter()
            .map(|n| HostNetwork {
                id: n.id,
                name: n.name,
            })
            .collect();
        Ok(networks)
    })
    .await?;
    let mut storages = Vec::new();
    for root in state.storage.list_responses() {
        let store = state.store.clone();
        let name = root.id.clone();
        let explicit = config.disk_limits.get(&root.id).copied();
        let capacity = stored(move || {
            if let Some(limit) = explicit {
                store.cluster_set_disk_budget(&name, limit)?;
            }
            store.cluster_disk_budget(&name, root.available_gib)
        })
        .await?;
        storages.push(HostStorage {
            unmanaged_gib: 0,
            id: root.id,
            capacity_gib: capacity,
        });
    }
    let mut report = HostReport {
        id,
        name: config.name.clone(),
        endpoint: config.endpoint.clone(),
        architecture: std::env::consts::ARCH.into(),
        capacity: HostCapacity {
            cpu: std::thread::available_parallelism()
                .map(|c| c.get().min(4096) as u32)
                .unwrap_or(1),
            ram_mib: status.memory_total_mib.saturating_sub(512),
        },
        unmanaged: HostCapacity::default(),
        images: state
            .templates
            .list_aliases()
            .iter()
            .map(|v| image(v))
            .collect(),
        networks,
        storages,
        labels: config.labels.clone(),
    };
    fill_unmanaged(state, &mut report).await?;
    Ok(report)
}

/// Local usage is always live, including when tests override hardware capacity.
async fn fill_unmanaged(state: &AppState, report: &mut HostReport) -> Result<(), ClusterError> {
    let store = state.store.clone();
    let managed: BTreeSet<_> = stored(move || store.cluster_workers())
        .await?
        .into_iter()
        .map(|r| r.id)
        .collect();
    let store = state.store.clone();
    let service_vms = stored(move || store.cluster_service_vm_ids()).await?;
    let vms = state.vms.lock().unwrap_or_else(|e| e.into_inner());
    let mut disks = BTreeMap::<&str, u64>::new();
    report.unmanaged = HostCapacity::default();
    for vm in vms.values().filter(|vm| !managed.contains(&vm.id)) {
        if !service_vms.contains(&vm.id) {
            report.unmanaged.cpu = report.unmanaged.cpu.saturating_add(u32::from(vm.cpu));
            report.unmanaged.ram_mib = report.unmanaged.ram_mib.saturating_add(u64::from(vm.ram));
        }
        *disks.entry(&vm.storage_root).or_default() += u64::from(vm.disk_gb);
    }
    for root in &mut report.storages {
        root.unmanaged_gib = *disks.get(root.id.as_str()).unwrap_or(&0);
    }
    Ok(())
}

/// Only the original, certificate-pinned owner may acknowledge or release a reservation.
pub(crate) async fn reconcile_one(
    state: AppState,
    id: Uuid,
    force: bool,
) -> Result<(), ClusterError> {
    let store = state.store.clone();
    let Some((token, mut p)) = stored(move || store.cluster_claim(id, now_ms(), force)).await?
    else {
        return Ok(());
    };
    let owner = p.vm.host_id;
    let result = async {
        let store = state.store.clone();
        let endpoint = stored(move || store.cluster_endpoint(owner)).await?;
        let endpoint = endpoint_url(&endpoint)?;
        let client = state
            .cluster
            .client(owner)
            .map_err(|_| ClusterError::Task)?;
        let url = format!("{}/api/cluster/worker/placements/{id}", endpoint);
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|_| ClusterError::Task)?;
        let intent = WorkerIntent {
            generation: p.generation,
            action: p.action,
            spec: p.spec.clone(),
            target: p.target.clone(),
        };
        let receipt = if response.status().is_success() {
            let r = read_receipt(response).await?;
            if r.intent == intent
                && matches!(
                    r.phase,
                    PlacementPhase::Confirmed | PlacementPhase::Deleted | PlacementPhase::Rejected
                )
            {
                r
            } else {
                send_intent(&client, &url, &intent).await?
            }
        } else if response.status() == reqwest::StatusCode::NOT_FOUND {
            send_intent(&client, &url, &intent).await?
        } else {
            return Err(ClusterError::Task);
        };
        if receipt.id != id
            || receipt.controller_id != state.cluster.identity.ok_or(ClusterError::Task)?
            || receipt.intent != intent
            || receipt.observed.as_ref().is_some_and(|vm| vm.id != id)
            || (receipt.phase == PlacementPhase::Deleted && p.action != PlacementAction::Delete)
            || (receipt.phase == PlacementPhase::Rejected && p.action != PlacementAction::Create)
            || (matches!(
                receipt.phase,
                PlacementPhase::Rejected | PlacementPhase::Deleted
            ) && receipt.observed.is_some())
            || (p.action == PlacementAction::Delete && receipt.phase == PlacementPhase::Confirmed)
            || (receipt.phase == PlacementPhase::Confirmed && receipt.observed.is_none())
        {
            return Err(ClusterError::Task);
        }
        Ok::<_, ClusterError>(receipt)
    }
    .await;
    match result {
        Ok(receipt) => {
            p.phase = receipt.phase;
            p.observed = receipt.observed;
            p.error = receipt.error;
        }
        Err(_) => {
            p.phase = PlacementPhase::Uncertain;
            p.error =
                Some("owner result is unresolved; ownership and capacity are retained".into());
        }
    }
    let store = state.store.clone();
    stored(move || store.cluster_finish(&p, token, now_ms())).await
}

/// Bound authenticated peers' replies as well as incoming HTTP request bodies.
async fn read_receipt(mut response: reqwest::Response) -> Result<WorkerReceipt, ClusterError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ClusterError::Task)? {
        if body.len().saturating_add(chunk.len()) > 1024 * 1024 {
            return Err(ClusterError::Task);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(Into::into)
}

async fn send_intent(
    client: &reqwest::Client,
    url: &str,
    intent: &WorkerIntent,
) -> Result<WorkerReceipt, ClusterError> {
    let response = client
        .put(url)
        .json(intent)
        .send()
        .await
        .map_err(|_| ClusterError::Task)?
        .error_for_status()
        .map_err(|_| ClusterError::Task)?;
    read_receipt(response).await
}

/// Independent tasks make request cancellation and controller restart safe.
pub(crate) fn spawn(state: AppState) {
    if state.cluster.paths.is_none() {
        return;
    }
    services::spawn(state.clone());
    let reconciler = state.clone();
    tokio::spawn(async move {
        let permits = Arc::new(Semaphore::new(8));
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        loop {
            interval.tick().await;
            let available = permits.available_permits();
            if available == 0 {
                continue;
            }
            let store = reconciler.store.clone();
            let Ok(placements) = stored(move || store.cluster_due(now_ms(), available)).await
            else {
                continue;
            };
            for id in placements {
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    break;
                };
                let state = reconciler.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let _ = reconcile_one(state, id, false).await;
                });
            }
        }
    });
    if state.cluster.worker.is_some() {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));
            loop {
                interval.tick().await;
                let Some(config) = &state.cluster.worker else {
                    break;
                };
                let result = async {
                    let mut report = inventory(&state).await?;
                    validate_report(&mut report)?;
                    let client = state
                        .cluster
                        .client(config.controller_id)
                        .map_err(|_| ClusterError::Task)?;
                    client
                        .post(format!(
                            "{}/api/cluster/hosts/register",
                            config.controller_url
                        ))
                        .json(&report)
                        .send()
                        .await
                        .map_err(|_| ClusterError::Task)?
                        .error_for_status()
                        .map_err(|_| ClusterError::Task)?;
                    Ok::<_, ClusterError>(())
                }
                .await;
                if let Err(error) = result {
                    tracing::warn!(%error,
                        "controller heartbeat failed; local VMs and ownership are retained"
                    );
                }
            }
        });
    }
}

#[cfg(test)]
mod tests;

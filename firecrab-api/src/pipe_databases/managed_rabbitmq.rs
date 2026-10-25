//! Durable, isolated RabbitMQ MicroVMs. Stop/start preserves each broker's data disk.
use super::{events::RabbitMQInput, write::Error};
use crate::{
    extract::ValidatedJson,
    handlers::{oci as oci_handler, vms},
    image_install::Architecture,
    oci::{self, ImageReference, ImageVersion, Sha256Digest},
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use firecrab_api_types::{CreateVmRequest, EgressPolicy, ImageInstallStatus, VmState};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Duration};
use uuid::Uuid;

const BROKER_RAM_MIB: u32 = 1024;

#[derive(Clone, Deserialize, Serialize)]
/// Owned broker VM and its private generated configuration.
pub struct Broker {
    /// Stable UUID for this owned resource.
    pub id: Uuid,
    /// Broker address advertised to its peers and the host relay.
    pub ipv4: String,
    /// Private broker settings, including generated TLS keys.
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    /// Generated broker configuration and TLS material, never public VM metadata.
    pub files: BTreeMap<String, String>,
}
#[derive(Clone, Deserialize, Serialize)]
/// Private managed RabbitMQ lifecycle and broker ownership.
pub struct Cluster {
    /// Stable UUID for this owned resource.
    pub id: Uuid,
    #[serde(default = "default_enabled")]
    /// Whether this host should supervise the managed cluster.
    pub enabled: bool,
    #[serde(default = "default_image")]
    /// Official RabbitMQ image reference, pinned before import.
    pub image: String,
    /// Explicit single-broker profile instead of three replicas.
    pub development: bool,
    /// Persisted setup or health state shown to the operator.
    pub phase: String,
    /// Redacted operation failure, never raw driver diagnostics.
    pub error: Option<String>,
    /// Owned broker VMs whose disks must survive restart.
    pub brokers: Vec<Broker>,
    /// Private client TLS configuration for the owned cluster.
    pub config: Option<RabbitMQInput>,
}
/// Resume managed cluster supervision unless explicitly disabled.
fn default_enabled() -> bool {
    true
}
/// Official RabbitMQ image whose digest is resolved before import.
pub fn default_image() -> String {
    "docker.io/library/rabbitmq:4.3.0".into()
}
impl std::fmt::Debug for Cluster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RabbitMQCluster")
            .field("id", &self.id)
            .field("phase", &self.phase)
            .finish_non_exhaustive()
    }
}
impl Cluster {
    /// Return public metadata without credentials or private SQL.
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({"id":self.id,"development":self.development,"phase":self.phase,"error":self.error,"brokers":self.brokers.iter().map(|b|serde_json::json!({"id":b.id,"ipv4":b.ipv4})).collect::<Vec<_>>()})
    }
}

/// Save cluster ownership unless an operator disabled supervision.
fn persist(state: &AppState, cluster: &Cluster) -> Result<(), Error> {
    if state
        .store
        .managed_database_rabbitmq()
        .map_err(|_| Error::Invalid("Cannot load RabbitMQ ownership"))?
        .is_some_and(|c| !c.enabled && cluster.enabled)
    {
        return Err(Error::Invalid("Managed RabbitMQ was disabled"));
    }
    state
        .store
        .save_managed_database_rabbitmq(cluster)
        .map_err(|_| Error::Invalid("Cannot save RabbitMQ ownership"))
}

/// Reserve capacity before downloading an image or creating any broker.
pub fn preflight(state: &AppState, development: bool) -> Result<(), Error> {
    let count = if development { 1 } else { 3 };
    let total = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u64>().ok())
        })
        .unwrap_or(0)
        / 1024;
    let reserved: u64 = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .map(|v| u64::from(v.ram))
        .sum();
    if total < reserved + count * u64::from(BROKER_RAM_MIB) + 1024 {
        return Err(Error::Invalid(
            "Managed RabbitMQ needs 1024 MiB per broker plus 1024 MiB host headroom; increase host capacity or connect an existing RabbitMQ cluster",
        ));
    }
    state.ensure_storage_capacity(state.storage.default_id(),count*10*1024*1024*1024).map_err(|_|Error::Invalid("Managed RabbitMQ needs 10 GiB of durable storage per broker; increase storage or connect an existing cluster"))?;
    Ok(())
}

/// Start owned brokers and supervise each endpoint with bounded repair attempts.
pub fn queue(state: AppState, cluster: Cluster) {
    tokio::spawn(async move {
        let mut cluster = cluster;
        if let Err(error) = start(&state, &mut cluster).await {
            tracing::warn!(%error, "Managed RabbitMQ setup failed");
            cluster.phase = "failed".into();
            cluster.error = Some(format!(
                "RabbitMQ setup failed: {error}. Owned broker disks are retained. Retry setup after checking capacity, image import and networking."
            ));
            let _ = persist(&state, &cluster);
            return;
        }
        let mut misses = BTreeMap::<Uuid, u8>::new();
        let mut restarted = BTreeMap::<Uuid, std::time::Instant>::new();
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            if !state
                .store
                .managed_database_rabbitmq()
                .ok()
                .flatten()
                .is_some_and(|c| c.enabled)
            {
                return;
            }
            let stopped = cluster.brokers.iter().any(|b| {
                state
                    .vms
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get(&b.id)
                    .is_some_and(|v| matches!(v.state, VmState::Stopped | VmState::Error))
            });
            if stopped {
                if start(&state, &mut cluster).await.is_err() {
                    cluster.phase = "degraded".into();
                    cluster.error =
                        Some("A RabbitMQ broker needs recovery; its data disk is retained".into());
                    let _ = persist(&state, &cluster);
                }
                continue;
            }
            // Probe every broker directly. One reachable bootstrap address
            // alone cannot establish that all three replicas are available.
            let probes =
                futures_util::future::join_all(cluster.brokers.iter().map(|broker| async {
                    let mut config = cluster.config.clone().expect("configured cluster");
                    config.development = true;
                    config.brokers = vec![format!("{}:5671", broker.ipv4)];
                    let healthy = match tokio::time::timeout(
                        Duration::from_secs(8),
                        config.connect(),
                    )
                    .await
                    {
                        Ok(Ok(connection)) => {
                            let _ = connection.close(200, "health check".into()).await;
                            true
                        }
                        _ => false,
                    };
                    (broker.id, healthy)
                }))
                .await;
            let healthy =
                probes.iter().all(|(_, healthy)| *healthy) && members_ready(&cluster).await;
            for (id, healthy) in &probes {
                let count = misses.entry(*id).or_default();
                *count = if *healthy { 0 } else { count.saturating_add(1) };
            }
            // Three misses avoid rebooting on one slow probe. Repair one
            // broker at a time and let it settle before another restart.
            if let Some(id) = probes.iter().map(|(id, _)| *id).find(|id| {
                misses[id] >= 3
                    && restarted
                        .get(id)
                        .is_none_or(|at| at.elapsed() >= Duration::from_secs(120))
            }) {
                restarted.insert(id, std::time::Instant::now());
                if vms::stop_vm(
                    State(state.clone()),
                    Extension(RequestId(cluster.id)),
                    Path(id.to_string()),
                )
                .await
                .is_ok()
                {
                    let _ = vms::start_vm(
                        State(state.clone()),
                        Extension(RequestId(cluster.id)),
                        Path(id.to_string()),
                    )
                    .await;
                }
            }
            let phase = if healthy { "ready" } else { "degraded" };
            if cluster.phase != phase {
                cluster.phase = phase.into();
                cluster.error = if healthy {
                    None
                } else {
                    Some(
                        "RabbitMQ is unavailable; source events remain in the database outbox"
                            .into(),
                    )
                };
                let _ = persist(&state, &cluster);
            }
        }
    });
}

/// Resolve and import the pinned RabbitMQ image through the existing OCI pipeline.
async fn image(state: &AppState, cluster: &mut Cluster) -> Result<String, Error> {
    let id = cluster.id;
    let original = ImageReference::parse(&cluster.image)
        .map_err(|_| Error::Invalid("Invalid RabbitMQ image"))?;
    if matches!(original.version, ImageVersion::Digest(_)) {
        let alias = oci::template_name_from_reference(&original)
            .map_err(|_| Error::Invalid("Cannot name the RabbitMQ image"))?
            .alias;
        if state.templates.resolve_alias(&alias).is_some() {
            return Ok(alias);
        }
    }
    let credential = oci_handler::stored_credential_for(state, &original.registry, RequestId(id))
        .await
        .map_err(|_| Error::Invalid("Cannot authenticate RabbitMQ image registry"))?;
    let resolved = oci::resolve(&original, Architecture::HOST, false, credential.clone())
        .await
        .map_err(|_| Error::Invalid("Cannot resolve the RabbitMQ image"))?;
    let pinned = ImageReference {
        version: ImageVersion::Digest(
            Sha256Digest::parse(&resolved.digest)
                .map_err(|_| Error::Invalid("Invalid RabbitMQ image digest"))?,
        ),
        ..original
    };
    cluster.image = format!("docker.io/library/rabbitmq@{}", resolved.digest);
    persist(state, cluster)?;
    let alias = oci::template_name_from_reference(&pinned)
        .map_err(|_| Error::Invalid("Cannot name the RabbitMQ image"))?
        .alias;
    if state.templates.resolve_alias(&alias).is_some() {
        return Ok(alias);
    }
    match state
        .oci_imports
        .begin_with(&alias, "managed RabbitMQ image import")
    {
        Ok(_) => {
            oci::run_oci_import(
                state.oci_imports.clone(),
                (*state.templates).clone(),
                pinned,
                alias.clone(),
                credential,
            )
            .await
        }
        Err("running") => tokio::time::timeout(Duration::from_secs(1800), async {
            while state.oci_imports.is_running(&alias) {
                tokio::time::sleep(Duration::from_millis(500)).await
            }
        })
        .await
        .map_err(|_| Error::Invalid("RabbitMQ image import timed out"))?,
        Err(_) => return Err(Error::Invalid("Cannot import the RabbitMQ image")),
    }
    if state.oci_imports.snapshot(&alias).status != ImageInstallStatus::Succeeded {
        return Err(Error::Invalid("RabbitMQ image import failed"));
    }
    Ok(alias)
}

/// Reuse broker disks, boot quorum members and activate the private RabbitMQ client.
async fn start(state: &AppState, cluster: &mut Cluster) -> Result<(), Error> {
    let _permit = state
        .service_deploy_permits
        .acquire()
        .await
        .map_err(|_| Error::Invalid("Host is stopping"))?;
    cluster.phase = "importing".into();
    cluster.error = None;
    persist(state, cluster)?;
    let alias = image(state, cluster).await?;
    let network = crate::service_runtime::default_network(state, cluster.id)
        .await
        .map_err(|_| Error::Invalid("Cannot create the RabbitMQ network"))?;
    let count = if cluster.development { 1 } else { 3 };
    while cluster.brokers.len() < count {
        let (_, Json(vm)) = vms::create_vm(
            State(state.clone()),
            Extension(RequestId(cluster.id)),
            ValidatedJson(CreateVmRequest {
                block_storages: Vec::new(),
                name: format!(
                    "rabbitmq-{}-{}",
                    &cluster.id.simple().to_string()[..8],
                    cluster.brokers.len() + 1
                ),
                template: alias.clone(),
                ram: BROKER_RAM_MIB,
                cpu: 1,
                disk_gb: 10,
                egress_policy: EgressPolicy::Isolated,
                micro_network_id: network,
                security_group_id: None,
                storage_root: None,
                shell_ids: vec![],
                env: BTreeMap::new(),
                port_forwards: vec![],
            }),
        )
        .await
        .map_err(|_| Error::Invalid("Cannot create a RabbitMQ broker"))?;
        cluster.brokers.push(Broker {
            id: vm.id,
            ipv4: vm
                .ipv4
                .ok_or(Error::Invalid("RabbitMQ broker has no address"))?,
            env: BTreeMap::new(),
            files: BTreeMap::new(),
        });
        persist(state, cluster)?;
    }
    if cluster.config.is_none() {
        configure(cluster)?;
        persist(state, cluster)?;
    }
    cluster.phase = "starting".into();
    persist(state, cluster)?;
    // Boot concurrently: quorum members must become available together.
    let futures = cluster.brokers.iter().map(|broker| async {
        let running = state
            .vms
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&broker.id)
            .is_some_and(|v| v.state == VmState::Running);
        if running {
            return Ok(());
        }
        vms::start_vm(
            State(state.clone()),
            Extension(RequestId(cluster.id)),
            Path(broker.id.to_string()),
        )
        .await
        .map(|_| ())
        .map_err(|_| Error::Invalid("RabbitMQ broker failed to boot"))
    });
    for result in futures_util::future::join_all(futures).await {
        result?;
    }
    let config = cluster
        .config
        .as_ref()
        .ok_or(Error::Invalid("RabbitMQ TLS setup is incomplete"))?;
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            if members_ready(cluster).await {
                break;
            }
            tokio::time::sleep(Duration::from_secs(2)).await
        }
    })
    .await
    .map_err(|_| Error::Invalid("RabbitMQ did not become ready"))?;
    let _claim = state.service_mutations.lock().await;
    if !state
        .store
        .managed_database_rabbitmq()
        .map_err(|_| Error::Invalid("Cannot load RabbitMQ ownership"))?
        .is_some_and(|c| c.enabled)
    {
        return Err(Error::Invalid("Managed RabbitMQ was disabled"));
    }
    state
        .store
        .save_database_rabbitmq(config)
        .map_err(|_| Error::Invalid("Cannot activate RabbitMQ"))?;
    cluster.phase = "ready".into();
    persist(state, cluster)
}

/// Do not create a production quorum queue while only one broker has joined.
async fn members_ready(cluster: &Cluster) -> bool {
    let Some(config) = &cluster.config else {
        return false;
    };
    let Some(ca) = config
        .ca_certificate
        .as_ref()
        .and_then(|pem| reqwest::Certificate::from_pem(pem.as_bytes()).ok())
    else {
        return false;
    };
    let Some(identity) = config
        .client_certificate
        .as_ref()
        .zip(config.client_key.as_ref())
        .and_then(|(cert, key)| {
            reqwest::Identity::from_pem(format!("{cert}\n{key}").as_bytes()).ok()
        })
    else {
        return false;
    };
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .add_root_certificate(ca)
        .identity(identity)
        .build()
    else {
        return false;
    };
    for broker in &cluster.brokers {
        let Ok(response) = client
            .get(format!("https://{}:15671/api/nodes", broker.ipv4))
            .basic_auth(&config.username, Some(&config.password))
            .send()
            .await
        else {
            continue;
        };
        if !response.status().is_success() {
            continue;
        }
        let Ok(nodes) = response.json::<Vec<serde_json::Value>>().await else {
            continue;
        };
        return cluster.brokers.iter().all(|broker| {
            nodes.iter().any(|node| {
                node["name"] == format!("rabbit@{}", broker.ipv4) && node["running"] == true
            })
        });
    }
    false
}

/// Generate cluster identity, mutual TLS and durable replica settings.
fn configure(cluster: &mut Cluster) -> Result<(), Error> {
    use rcgen::{
        BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
        KeyUsagePurpose,
    };
    let fail = |_| Error::Invalid("Cannot generate RabbitMQ certificates");
    let mut ca = CertificateParams::new(Vec::<String>::new()).map_err(fail)?;
    let now = std::time::SystemTime::now();
    ca.not_before = (now - Duration::from_secs(86400)).into();
    ca.not_after = (now + Duration::from_secs(3650 * 86400)).into();
    ca.distinguished_name.push(
        rcgen::DnType::CommonName,
        format!("Firecrab RabbitMQ CA {}", cluster.id),
    );
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
    ];
    let ca_key = KeyPair::generate().map_err(fail)?;
    let ca_cert = ca.self_signed(&ca_key).map_err(fail)?.pem();
    let issuer = Issuer::new(ca, ca_key);
    let certificate = |names: Vec<String>| -> Result<(String, String), Error> {
        let common_name = names
            .first()
            .cloned()
            .unwrap_or_else(|| "Firecrab RabbitMQ client".into());
        let mut params = CertificateParams::new(names).map_err(fail)?;
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, common_name);
        params.not_before = (now - Duration::from_secs(86400)).into();
        params.not_after = (now + Duration::from_secs(3650 * 86400)).into();
        params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];
        let key = KeyPair::generate().map_err(fail)?;
        let cert = params.signed_by(&key, &issuer).map_err(fail)?.pem();
        Ok((cert, key.serialize_pem()))
    };
    let nodes = cluster
        .brokers
        .iter()
        .enumerate()
        .map(|(i, b)| {
            format!(
                "cluster_formation.classic_config.nodes.{} = rabbit@{}\n",
                i + 1,
                b.ipv4
            )
        })
        .collect::<String>();
    let password = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let cookie = Uuid::new_v4().simple().to_string();
    for broker in &mut cluster.brokers {
        let (cert, key) = certificate(vec![broker.ipv4.clone()])?;
        let config = format!(
            "listeners.tcp = none\nmanagement.ssl.port = 15671\nmanagement.ssl.cacertfile = /etc/firecrab/rabbitmq-ca.pem\nmanagement.ssl.certfile = /etc/firecrab/rabbitmq-cert.pem\nmanagement.ssl.keyfile = /etc/firecrab/rabbitmq.key\nmanagement.ssl.verify = verify_peer\nmanagement.ssl.fail_if_no_peer_cert = true\ndefault_user_tags.administrator = true\nlisteners.ssl.default = 5671\nssl_options.cacertfile = /etc/firecrab/rabbitmq-ca.pem\nssl_options.certfile = /etc/firecrab/rabbitmq-cert.pem\nssl_options.keyfile = /etc/firecrab/rabbitmq.key\nssl_options.verify = verify_peer\nssl_options.fail_if_no_peer_cert = true\ncluster_formation.peer_discovery_backend = classic_config\n{nodes}cluster_partition_handling = pause_minority\nvm_memory_high_watermark.absolute = 512MiB\ndisk_free_limit.absolute = 1GB\n"
        );
        broker.files = BTreeMap::from([
            ("/etc/rabbitmq/enabled_plugins".into(),"[rabbitmq_management].\n".into()),
            ("/etc/firecrab/rabbitmq.conf".into(),config),
            ("/etc/firecrab/rabbitmq-ca.pem".into(),ca_cert.clone()),
            ("/etc/firecrab/rabbitmq-cert.pem".into(),cert),
            ("/etc/firecrab/rabbitmq.key".into(),key),
            ("/etc/firecrab/rabbitmq-ssl.config".into(), "[{server,[{cacertfile,\"/etc/firecrab/rabbitmq-ca.pem\"},{certfile,\"/etc/firecrab/rabbitmq-cert.pem\"},{keyfile,\"/etc/firecrab/rabbitmq.key\"},{verify,verify_peer},{fail_if_no_peer_cert,true}]},{client,[{cacertfile,\"/etc/firecrab/rabbitmq-ca.pem\"},{certfile,\"/etc/firecrab/rabbitmq-cert.pem\"},{keyfile,\"/etc/firecrab/rabbitmq.key\"},{verify,verify_peer}]}].\n".into()),
        ]);
        broker.env = BTreeMap::from([
            (
                "RABBITMQ_NODENAME".into(),
                format!("rabbit@{}", broker.ipv4),
            ),
            ("RABBITMQ_USE_LONGNAME".into(), "true".into()),
            ("RABBITMQ_ERLANG_COOKIE".into(), cookie.clone()),
            ("RABBITMQ_DEFAULT_USER".into(), "firecrab".into()),
            ("RABBITMQ_DEFAULT_PASS".into(), password.clone()),
            ("RABBITMQ_DEFAULT_VHOST".into(), "firecrab".into()),
            (
                "RABBITMQ_CONFIG_FILE".into(),
                "/etc/firecrab/rabbitmq.conf".into(),
            ),
            (
                "RABBITMQ_SERVER_ADDITIONAL_ERL_ARGS".into(),
                "+S 2:2 -proto_dist inet_tls -ssl_dist_optfile /etc/firecrab/rabbitmq-ssl.config"
                    .into(),
            ),
            (
                "RABBITMQ_CTL_ERL_ARGS".into(),
                "+S 1:1 -proto_dist inet_tls -ssl_dist_optfile /etc/firecrab/rabbitmq-ssl.config"
                    .into(),
            ),
        ]);
    }
    let (client_cert, client_key) = certificate(vec!["firecrab-host".into()])?;
    cluster.config = Some(RabbitMQInput {
        brokers: cluster
            .brokers
            .iter()
            .map(|b| format!("{}:5671", b.ipv4))
            .collect(),
        tls: true,
        ca_certificate: Some(ca_cert),
        username: "firecrab".into(),
        password,
        virtual_host: "firecrab".into(),
        client_certificate: Some(client_cert),
        client_key: Some(client_key),
        development: cluster.development,
    });
    Ok(())
}

/// Resume supervision only for an enabled persisted managed cluster.
pub fn recover(state: AppState) {
    if let Ok(Some(cluster)) = state.store.managed_database_rabbitmq()
        && cluster.enabled
    {
        queue(state, cluster)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn broker_memory_satisfies_vm_creation_contract() {
        assert!(vms::is_valid_ram_mib(BROKER_RAM_MIB));
    }
    #[tokio::test]
    async fn pinned_local_image_recovers_without_registry_access() {
        use crate::templates::{TemplateRegistry, TemplateSpec};
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("kernel"), b"qa kernel").unwrap();
        std::fs::write(directory.path().join("rootfs"), b"qa rootfs").unwrap();
        let reference = format!(
            "registry.invalid/library/rabbitmq@sha256:{}",
            "a".repeat(64)
        );
        let parsed = ImageReference::parse(&reference).unwrap();
        let alias = oci::template_name_from_reference(&parsed).unwrap().alias;
        let templates = TemplateRegistry::from_specs(
            directory.path(),
            vec![TemplateSpec {
                alias: alias.clone(),
                version: "qa".into(),
                kernel: "kernel".into(),
                initrd: None,
                rootfs: "rootfs".into(),
                boot_args: String::new(),
            }],
        )
        .unwrap();
        let state = AppState::with_db_file(templates, directory.path().join("state.db"))
            .await
            .unwrap();
        let mut cluster = Cluster {
            id: Uuid::new_v4(),
            enabled: true,
            image: reference,
            development: false,
            phase: "ready".into(),
            error: None,
            brokers: vec![],
            config: None,
        };
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), image(&state, &mut cluster))
                .await
                .unwrap()
                .unwrap(),
            alias
        );
    }
    #[test]
    #[ignore = "exports disposable TLS RabbitMQ fixture to FIRECRAB_RABBITMQ_EXPORT"]
    fn export_managed_rabbitmq_fixture() {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut cluster = Cluster {
            id: Uuid::new_v4(),
            enabled: true,
            image: default_image(),
            development: false,
            phase: "qa".into(),
            error: None,
            brokers: (1..=3)
                .map(|i| Broker {
                    id: Uuid::new_v4(),
                    ipv4: format!(
                        "{}-{i}.internal",
                        std::env::var("FIRECRAB_RABBITMQ_QA_PREFIX")
                            .unwrap_or_else(|_| "firecrab-413-qa-rabbitmq".into())
                    ),
                    env: BTreeMap::new(),
                    files: BTreeMap::new(),
                })
                .collect(),
            config: None,
        };
        configure(&mut cluster).unwrap();
        let path = std::env::var("FIRECRAB_RABBITMQ_EXPORT").unwrap();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(&serde_json::to_vec(&cluster).unwrap())
            .unwrap();
        assert!(
            cluster
                .brokers
                .iter()
                .all(|b| b.env["RABBITMQ_SERVER_ADDITIONAL_ERL_ARGS"].contains("inet_tls"))
        );
    }
}

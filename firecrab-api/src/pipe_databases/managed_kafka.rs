//! Durable, isolated Kafka MicroVMs. Stop/start preserves each broker's data disk.
use super::{events::KafkaInput, write::Error};
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

const BROKER_RAM_MIB: u32 = 2048;

#[derive(Clone, Deserialize, Serialize)]
/// Owned broker VM and its private generated configuration.
pub struct Broker {
    /// Stable UUID for this owned resource.
    pub id: Uuid,
    /// Broker address advertised to its peers and the host relay.
    pub ipv4: String,
    /// Private broker settings, including generated TLS keys.
    pub env: BTreeMap<String, String>,
}
#[derive(Clone, Deserialize, Serialize)]
/// Private managed Kafka lifecycle and broker ownership.
pub struct Cluster {
    /// Stable UUID for this owned resource.
    pub id: Uuid,
    #[serde(default = "default_enabled")]
    /// Whether this host should supervise the managed cluster.
    pub enabled: bool,
    #[serde(default = "default_image")]
    /// Official Kafka image reference, pinned before import.
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
    pub config: Option<KafkaInput>,
}
/// Resume managed cluster supervision unless explicitly disabled.
fn default_enabled() -> bool {
    true
}
/// Official Kafka image whose digest is resolved before import.
pub fn default_image() -> String {
    "docker.io/apache/kafka:4.3.1".into()
}
impl std::fmt::Debug for Cluster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KafkaCluster")
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
        .managed_database_kafka()
        .map_err(|_| Error::Invalid("Cannot load Kafka ownership"))?
        .is_some_and(|c| !c.enabled && cluster.enabled)
    {
        return Err(Error::Invalid("Managed Kafka was disabled"));
    }
    state
        .store
        .save_managed_database_kafka(cluster)
        .map_err(|_| Error::Invalid("Cannot save Kafka ownership"))
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
            "Managed Kafka needs 2048 MiB per broker plus 1024 MiB host headroom; increase host capacity or connect an existing Kafka cluster",
        ));
    }
    state.ensure_storage_capacity(state.storage.default_id(),count*10*1024*1024*1024).map_err(|_|Error::Invalid("Managed Kafka needs 10 GiB of durable storage per broker; increase storage or connect an existing cluster"))?;
    Ok(())
}

/// Start owned brokers and supervise each endpoint with bounded repair attempts.
pub fn queue(state: AppState, cluster: Cluster) {
    tokio::spawn(async move {
        let mut cluster = cluster;
        if let Err(error) = start(&state, &mut cluster).await {
            tracing::warn!(%error, "Managed Kafka setup failed");
            cluster.phase = "failed".into();
            cluster.error = Some(format!(
                "Kafka setup failed: {error}. Owned broker disks are retained. Retry setup after checking capacity, image import and networking."
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
                .managed_database_kafka()
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
                        Some("A Kafka broker needs recovery; its data disk is retained".into());
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
                    config.brokers = vec![format!("{}:9092", broker.ipv4)];
                    let healthy = tokio::time::timeout(Duration::from_secs(8), config.connect())
                        .await
                        .is_ok_and(|r| r.is_ok());
                    (broker.id, healthy)
                }))
                .await;
            let healthy = probes.iter().all(|(_, healthy)| *healthy);
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
                    Some("Kafka is unavailable; source events remain in the database outbox".into())
                };
                let _ = persist(&state, &cluster);
            }
        }
    });
}

/// Resolve and import the pinned Kafka image through the existing OCI pipeline.
async fn image(state: &AppState, cluster: &mut Cluster) -> Result<String, Error> {
    let id = cluster.id;
    let original =
        ImageReference::parse(&cluster.image).map_err(|_| Error::Invalid("Invalid Kafka image"))?;
    if matches!(original.version, ImageVersion::Digest(_)) {
        let alias = oci::template_name_from_reference(&original)
            .map_err(|_| Error::Invalid("Cannot name the Kafka image"))?
            .alias;
        if state.templates.resolve_alias(&alias).is_some() {
            return Ok(alias);
        }
    }
    let credential = oci_handler::stored_credential_for(state, &original.registry, RequestId(id))
        .await
        .map_err(|_| Error::Invalid("Cannot authenticate Kafka image registry"))?;
    let resolved = oci::resolve(&original, Architecture::HOST, false, credential.clone())
        .await
        .map_err(|_| Error::Invalid("Cannot resolve the Kafka image"))?;
    let pinned = ImageReference {
        version: ImageVersion::Digest(
            Sha256Digest::parse(&resolved.digest)
                .map_err(|_| Error::Invalid("Invalid Kafka image digest"))?,
        ),
        ..original
    };
    cluster.image = format!("docker.io/apache/kafka@{}", resolved.digest);
    persist(state, cluster)?;
    let alias = oci::template_name_from_reference(&pinned)
        .map_err(|_| Error::Invalid("Cannot name the Kafka image"))?
        .alias;
    if state.templates.resolve_alias(&alias).is_some() {
        return Ok(alias);
    }
    match state
        .oci_imports
        .begin_with(&alias, "managed Kafka image import")
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
        .map_err(|_| Error::Invalid("Kafka image import timed out"))?,
        Err(_) => return Err(Error::Invalid("Cannot import the Kafka image")),
    }
    if state.oci_imports.snapshot(&alias).status != ImageInstallStatus::Succeeded {
        return Err(Error::Invalid("Kafka image import failed"));
    }
    Ok(alias)
}

/// Reuse broker disks, boot quorum members and activate the private Kafka client.
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
        .map_err(|_| Error::Invalid("Cannot create the Kafka network"))?;
    let count = if cluster.development { 1 } else { 3 };
    while cluster.brokers.len() < count {
        let (_, Json(vm)) = vms::create_vm(
            State(state.clone()),
            Extension(RequestId(cluster.id)),
            ValidatedJson(CreateVmRequest {
                name: format!(
                    "kafka-{}-{}",
                    &cluster.id.simple().to_string()[..8],
                    cluster.brokers.len() + 1
                ),
                template: alias.clone(),
                ram: BROKER_RAM_MIB,
                cpu: 1,
                disk_gb: 10,
                egress_policy: EgressPolicy::Isolated,
                micro_network_id: network,
                storage_root: None,
                shell_ids: vec![],
                env: BTreeMap::new(),
                port_forwards: vec![],
            }),
        )
        .await
        .map_err(|_| Error::Invalid("Cannot create a Kafka broker"))?;
        cluster.brokers.push(Broker {
            id: vm.id,
            ipv4: vm
                .ipv4
                .ok_or(Error::Invalid("Kafka broker has no address"))?,
            env: BTreeMap::new(),
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
        .map_err(|_| Error::Invalid("Kafka broker failed to boot"))
    });
    for result in futures_util::future::join_all(futures).await {
        result?;
    }
    let config = cluster
        .config
        .as_ref()
        .ok_or(Error::Invalid("Kafka TLS setup is incomplete"))?;
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            if config.connect().await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(2)).await
        }
    })
    .await
    .map_err(|_| Error::Invalid("Kafka did not become ready"))?;
    let _claim = state.service_mutations.lock().await;
    if !state
        .store
        .managed_database_kafka()
        .map_err(|_| Error::Invalid("Cannot load Kafka ownership"))?
        .is_some_and(|c| c.enabled)
    {
        return Err(Error::Invalid("Managed Kafka was disabled"));
    }
    state
        .store
        .save_database_kafka(config)
        .map_err(|_| Error::Invalid("Cannot activate Kafka"))?;
    cluster.phase = "ready".into();
    persist(state, cluster)
}

/// Generate cluster identity, mutual TLS and durable replica settings.
fn configure(cluster: &mut Cluster) -> Result<(), Error> {
    use rcgen::{
        BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
        KeyUsagePurpose,
    };
    let fail = |_| Error::Invalid("Cannot generate Kafka certificates");
    let mut ca = CertificateParams::new(Vec::<String>::new()).map_err(fail)?;
    let now = std::time::SystemTime::now();
    ca.not_before = (now - Duration::from_secs(86400)).into();
    ca.not_after = (now + Duration::from_secs(3650 * 86400)).into();
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
    ];
    let ca_key = KeyPair::generate().map_err(fail)?;
    let ca_cert = ca.self_signed(&ca_key).map_err(fail)?.pem();
    let issuer = Issuer::new(ca, ca_key);
    let certificate = |names: Vec<String>| -> Result<(String, String), Error> {
        let mut params = CertificateParams::new(names).map_err(fail)?;
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
    let voters = cluster
        .brokers
        .iter()
        .enumerate()
        .map(|(i, b)| format!("{}@{}:9093", i + 1, b.ipv4))
        .collect::<Vec<_>>()
        .join(",");
    let replicas = if cluster.development { "1" } else { "3" };
    let min_isr = if cluster.development { "1" } else { "2" };
    let cluster_id = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        cluster.id.as_bytes(),
    );
    for (index, broker) in cluster.brokers.iter_mut().enumerate() {
        let (cert, key) = certificate(vec![broker.ipv4.clone()])?;
        broker.env = BTreeMap::from([
            ("CLUSTER_ID".into(), cluster_id.clone()),
            ("KAFKA_NODE_ID".into(), (index + 1).to_string()),
            ("KAFKA_PROCESS_ROLES".into(), "broker,controller".into()),
            (
                "KAFKA_LISTENERS".into(),
                "BROKER://:9092,CONTROLLER://:9093".into(),
            ),
            (
                "KAFKA_ADVERTISED_LISTENERS".into(),
                format!("BROKER://{}:9092", broker.ipv4),
            ),
            (
                "KAFKA_LISTENER_SECURITY_PROTOCOL_MAP".into(),
                "BROKER:SSL,CONTROLLER:SSL".into(),
            ),
            ("KAFKA_INTER_BROKER_LISTENER_NAME".into(), "BROKER".into()),
            (
                "KAFKA_CONTROLLER_LISTENER_NAMES".into(),
                "CONTROLLER".into(),
            ),
            ("KAFKA_CONTROLLER_QUORUM_VOTERS".into(), voters.clone()),
            ("KAFKA_SSL_CLIENT_AUTH".into(), "required".into()),
            ("KAFKA_SSL_KEYSTORE_TYPE".into(), "PEM".into()),
            ("KAFKA_SSL_KEYSTORE_KEY".into(), key.replace('\n', "\\n")),
            (
                "KAFKA_SSL_KEYSTORE_CERTIFICATE_CHAIN".into(),
                cert.replace('\n', "\\n"),
            ),
            ("KAFKA_SSL_TRUSTSTORE_TYPE".into(), "PEM".into()),
            (
                "KAFKA_SSL_TRUSTSTORE_CERTIFICATES".into(),
                ca_cert.replace('\n', "\\n"),
            ),
            ("KAFKA_LOG_DIRS".into(), "/var/lib/kafka/data".into()),
            ("KAFKA_AUTO_CREATE_TOPICS_ENABLE".into(), "false".into()),
            (
                "KAFKA_UNCLEAN_LEADER_ELECTION_ENABLE".into(),
                "false".into(),
            ),
            ("KAFKA_MIN_INSYNC_REPLICAS".into(), min_isr.into()),
            ("KAFKA_DEFAULT_REPLICATION_FACTOR".into(), replicas.into()),
            (
                "KAFKA_OFFSETS_TOPIC_REPLICATION_FACTOR".into(),
                replicas.into(),
            ),
            (
                "KAFKA_TRANSACTION_STATE_LOG_REPLICATION_FACTOR".into(),
                replicas.into(),
            ),
            ("KAFKA_TRANSACTION_STATE_LOG_MIN_ISR".into(), min_isr.into()),
            ("KAFKA_HEAP_OPTS".into(), "-Xms512m -Xmx768m".into()),
            ("KAFKA_LOG_RETENTION_HOURS".into(), "168".into()),
        ]);
    }
    let (client_cert, client_key) = certificate(vec!["firecrab-host".into()])?;
    cluster.config = Some(KafkaInput {
        brokers: cluster
            .brokers
            .iter()
            .map(|b| format!("{}:9092", b.ipv4))
            .collect(),
        tls: true,
        ca_certificate: Some(ca_cert),
        username: None,
        password: None,
        client_certificate: Some(client_cert),
        client_key: Some(client_key),
        development: cluster.development,
    });
    Ok(())
}

/// Resume supervision only for an enabled persisted managed cluster.
pub fn recover(state: AppState) {
    if let Ok(Some(cluster)) = state.store.managed_database_kafka()
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
        let reference = format!("registry.invalid/apache/kafka@sha256:{}", "a".repeat(64));
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
    #[ignore = "exports disposable TLS Kafka fixture to FIRECRAB_KAFKA_EXPORT"]
    fn export_managed_kafka_fixture() {
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
                        "{}-{i}",
                        std::env::var("FIRECRAB_KAFKA_QA_PREFIX")
                            .unwrap_or_else(|_| "firecrab-413-qa-kafka".into())
                    ),
                    env: BTreeMap::new(),
                })
                .collect(),
            config: None,
        };
        configure(&mut cluster).unwrap();
        let path = std::env::var("FIRECRAB_KAFKA_EXPORT").unwrap();
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
                .all(|b| b.env["KAFKA_MIN_INSYNC_REPLICAS"] == "2")
        );
    }
}

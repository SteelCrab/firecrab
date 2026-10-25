//! Controller service identity, releases and durable host-local deployment intents.
use crate::{DeployMicroServiceRequest, HostCapacity, MicroServiceDeployment, MicroServiceSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

/// Application state must survive moving a request to another replica.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClusterServiceStateMode {
    #[default]
    StatelessHttp,
    ExternalState,
}

/// Exclusive jobs need the ownership and fencing protocol in #462.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClusterServiceBackgroundWork {
    #[default]
    Disabled,
    DuplicateSafe,
    Exclusive,
}

/// Operator declaration; firecrab does not replicate application data or implement deduplication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ClusterServiceState {
    pub mode: ClusterServiceStateMode,
    /// Environment name containing the shared external session-store URL.
    pub sessions: Option<String>,
    /// Environment name containing the shared external upload-store URL.
    pub uploads: Option<String>,
    /// Environment name containing the shared external database URL.
    pub database: Option<String>,
    /// Requests must tolerate retries and execution on different replicas.
    pub requests_duplicate_safe: bool,
    pub background_work: ClusterServiceBackgroundWork,
}

impl Default for ClusterServiceState {
    fn default() -> Self {
        Self {
            mode: ClusterServiceStateMode::StatelessHttp,
            sessions: None,
            uploads: None,
            database: None,
            requests_duplicate_safe: true,
            background_work: ClusterServiceBackgroundWork::Disabled,
        }
    }
}

/// Controller PipeSecret and its exact version, scoped to one guest environment name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ClusterServiceCredential {
    pub secret_id: Uuid,
    pub version: u64,
}

/// Scheduling mode. Active/standby scheduling belongs to #463.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClusterServiceMode {
    #[default]
    Replicated,
}

/// Explicit replica distribution with a network owned by each enrolled host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ClusterServiceTarget {
    pub host_id: Uuid,
    pub replicas: u16,
    pub request: DeployMicroServiceRequest,
}

/// One immutable OCI digest and configuration distributed across workload hosts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ClusterServiceSpec {
    pub service: MicroServiceSpec,
    #[serde(default)]
    pub state: ClusterServiceState,
    /// Values stay in encrypted vaults and are never part of the public service journal.
    #[serde(default)]
    pub credentials: BTreeMap<String, ClusterServiceCredential>,
    #[serde(default)]
    pub mode: ClusterServiceMode,
    pub targets: Vec<ClusterServiceTarget>,
}

/// Updates compare against the operation generation inspected by the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdateClusterServiceRequest {
    pub expected_generation: u64,
    pub spec: ClusterServiceSpec,
}

/// Stop is also fenced against an unseen newer release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StopClusterServiceRequest {
    pub expected_generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClusterServiceAction {
    Create,
    Update,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClusterServiceDesired {
    Running,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClusterServiceObserved {
    Pending,
    Running,
    Degraded,
    Stopped,
}

/// Worker protocol uses the controller service UUID as its local service UUID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServiceWorkerIntent {
    pub generation: u64,
    pub release_generation: u64,
    pub desired: ClusterServiceDesired,
    pub deployment_id: Uuid,
    pub spec: MicroServiceSpec,
    #[serde(default)]
    pub state: ClusterServiceState,
    #[serde(default)]
    pub credentials: BTreeMap<String, ClusterServiceCredential>,
    pub request: DeployMicroServiceRequest,
}

/// Ownership remains recorded even when observation or cleanup fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterServiceHost {
    pub host_id: Uuid,
    /// Reserved for #463; replicated services have no active/standby role.
    pub role: Option<String>,
    /// Capacity retained until this host acknowledges cleanup or the replacement release.
    pub reservation: HostCapacity,
    pub intent: ServiceWorkerIntent,
    pub observed_generation: Option<u64>,
    pub deployment: Option<MicroServiceDeployment>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterService {
    pub id: Uuid,
    pub spec: ClusterServiceSpec,
    /// Changes for every accepted lifecycle intent, including Stop.
    pub generation: u64,
    /// Changes only for a new image/configuration release.
    pub release_generation: u64,
    pub operation_id: Uuid,
    pub desired: ClusterServiceDesired,
    pub observed: ClusterServiceObserved,
    pub pending: bool,
    pub degraded: bool,
    pub hosts: Vec<ClusterServiceHost>,
}

/// The exact accepted request and outcome remain inspectable after later releases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterServiceOperation {
    pub id: Uuid,
    pub idempotency_key: String,
    pub action: ClusterServiceAction,
    pub expected_generation: Option<u64>,
    /// A newer lifecycle intent superseded this operation's pending work.
    pub superseded_by: Option<Uuid>,
    pub service: ClusterService,
}

/// Durable authenticated ownership, including deployment IDs from older releases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceWorkerReceipt {
    pub id: Uuid,
    pub controller_id: Uuid,
    pub intent: ServiceWorkerIntent,
    pub deployment_ids: Vec<Uuid>,
    pub deployment: Option<MicroServiceDeployment>,
    pub error: Option<String>,
}

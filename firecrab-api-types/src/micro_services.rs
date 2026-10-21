//! Desired service configuration. Deployment and live health are separate resources.

use std::collections::BTreeMap;

use crate::VmState;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Execution backend for each service replica.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceRuntime {
    /// Run each replica in a Firecracker MicroVM.
    #[default]
    Vm,
}

/// Body for POST /api/micro-services and full replacement by PUT /api/micro-services/{id}.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MicroServiceSpec {
    /// Unique DNS label; also usable as the future `<name>.internal` address.
    pub name: String,
    /// OCI image reference (tag or sha256 digest). Resolved only during deployment.
    pub image: String,
    /// Replica execution backend; defaults to a MicroVM.
    #[serde(default)]
    pub runtime: ServiceRuntime,
    /// TCP port on which the application listens inside each guest.
    pub port: u16,
    /// Absolute HTTP health-check path; defaults to `/health`.
    #[serde(default = "default_health_path")]
    pub health_path: String,
    /// Desired public hostname. Host-port deployment currently rejects this field.
    #[serde(default)]
    pub domain: Option<String>,
    /// Guest RAM in MiB, using the same sizes as MicroVMs.
    pub ram: u32,
    /// Virtual CPU count for each replica.
    pub cpu: u8,
    /// Desired number of instances; defaults to one.
    #[serde(default = "default_replicas")]
    pub replicas: u16,
    /// Seconds allowed for HTTP health checks after each VM boots; defaults to 300.
    #[serde(default = "default_startup_timeout")]
    pub startup_timeout: u32,
    /// Seconds to drain existing requests after withdrawing host ports; defaults to 30.
    #[serde(default = "default_drain_timeout")]
    pub drain_timeout: u32,
    /// Plain-text environment variables passed to the image entrypoint.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Environment variable name -> CrabSecrets UUID. Never contains secret values.
    /// Host-port deployment currently rejects nonempty secret references.
    #[serde(default)]
    pub secrets: BTreeMap<String, Uuid>,
}

/// Saved configuration and its stable service identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MicroServiceResponse {
    /// Identifier used by configuration and deployment routes.
    pub id: Uuid,
    /// Desired configuration, flattened into the response object.
    #[serde(flatten)]
    pub spec: MicroServiceSpec,
}

/// Network placement and host ports for a new deployment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DeployMicroServiceRequest {
    /// Existing guest network; omission selects or creates the `services` network.
    #[serde(default)]
    pub micro_network_id: Option<Uuid>,
    /// First replica's host TCP port; remaining replicas use consecutive ports.
    pub host_port: u16,
}

/// Deployment progress or the health of an existing deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServicePhase {
    /// Accepted and waiting for the deployment worker.
    Queued,
    /// Resolving the OCI digest and preparing the VM image.
    Importing,
    /// Creating or booting a replica.
    Starting,
    /// Waiting for a booted replica to answer its HTTP health check.
    CheckingHealth,
    /// Every requested replica passed its startup health check.
    Running,
    /// A live status read found a missing, stopped or unhealthy replica.
    Degraded,
    /// Withdrawing ports, draining requests and removing owned VMs.
    Stopping,
    /// All owned VMs have been removed.
    Stopped,
    /// Deployment or cleanup failed; remaining instances can be removed with Stop.
    Failed,
}

impl ServicePhase {
    /// Whether an operation is still in progress and blocks service mutations.
    pub fn is_busy(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Importing | Self::Starting | Self::CheckingHealth | Self::Stopping
        )
    }
}

/// One deployment-owned VM and its most recently observed state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceInstance {
    /// Owned VM identifier, retained until cleanup succeeds.
    pub vm_id: Uuid,
    /// Guest IPv4 address used for direct application health checks.
    pub ipv4: String,
    /// Host TCP port forwarded to the application's guest port.
    pub host_port: u16,
    /// Latest VM lifecycle state; a missing VM is reported as stopped.
    pub state: VmState,
    /// Whether the latest HTTP health check returned a success status.
    pub healthy: bool,
}

/// Durable deployment snapshot. Live reads refresh instance states and health.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MicroServiceDeployment {
    /// Identifier for this deployment attempt.
    pub id: Uuid,
    /// Service whose configuration was deployed.
    pub service_id: Uuid,
    /// Latest operation phase, refreshed for health on status reads.
    pub phase: ServicePhase,
    /// Configuration snapshot captured when deployment was accepted.
    pub spec: MicroServiceSpec,
    /// Network and host-port settings, including the selected default network.
    pub request: DeployMicroServiceRequest,
    /// Immutable OCI image digest, set after image resolution.
    pub digest: Option<String>,
    /// VMs owned by this attempt and still requiring cleanup.
    pub instances: Vec<ServiceInstance>,
    /// Deployment progress messages in chronological order.
    pub log: Vec<String>,
    /// Failure description, if deployment or cleanup failed.
    pub error: Option<String>,
}

/// Default application health endpoint.
fn default_health_path() -> String {
    "/health".to_owned()
}

/// Start with a single application instance.
fn default_replicas() -> u16 {
    1
}

/// Allow five minutes for application health after VM boot.
fn default_startup_timeout() -> u32 {
    300
}

/// Allow thirty seconds for in-flight requests during Stop.
fn default_drain_timeout() -> u32 {
    30
}

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
    /// Optional hostname routed automatically to healthy replicas through PipeIngress.
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
    /// Environment variable name -> PipeSecrets UUID. Never contains secret values.
    /// Resolved into an encrypted deployment snapshot and injected only into guests.
    #[serde(default)]
    pub secrets: BTreeMap<String, Uuid>,
    /// PipeStorage connection for the application's AWS SDK. Credentials stay on the host.
    #[serde(default)]
    pub storage_connection_id: Option<Uuid>,
    /// Binding alias -> Queue reference and least-privilege application access.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub queues: BTreeMap<String, crate::QueueBinding>,
    /// Policy copied to every replica and captured in deployment snapshots.
    #[serde(default)]
    pub security_group_id: Option<Uuid>,
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

/// Optional network placement and host ports for a new deployment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DeployMicroServiceRequest {
    /// Existing guest network; omission selects or creates the `services` network.
    #[serde(default)]
    pub micro_network_id: Option<Uuid>,
    /// First replica's host TCP port. Required without a domain; omitted for domain routing.
    /// Zero means no host port and is accepted only when the service has a domain.
    #[serde(default)]
    pub host_port: u16,
}

/// Deployment progress or the health of an existing deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServicePhase {
    /// Applying a verified Nginx upstream generation.
    SwitchingTraffic,
    /// Keeping retired VMs alive until old Nginx workers finish.
    Draining,
    /// Restoring the previous healthy release after a failed update.
    RollingBack,
    /// Accepted and waiting for the deployment worker.
    Queued,
    /// Resolving the OCI digest and preparing the VM image.
    Importing,
    /// Creating or booting a replica.
    Starting,
    /// Waiting for a booted replica to answer its HTTP health check.
    CheckingHealth,
    /// Connecting healthy replicas to the service domain.
    ConnectingDomain,
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
            Self::Queued
                | Self::SwitchingTraffic
                | Self::Draining
                | Self::RollingBack
                | Self::Importing
                | Self::Starting
                | Self::CheckingHealth
                | Self::ConnectingDomain
                | Self::Stopping
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
    /// Host TCP port forwarded to the app; zero when a domain uses direct guest routing.
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
    /// Unix time when the attempt was accepted; zero for older records.
    #[serde(default)]
    pub started_at: i64,
    /// Unix time when the attempt reached a terminal outcome.
    #[serde(default)]
    pub finished_at: Option<i64>,
    /// Durable update/recovery journal. Older initial deployments omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<Box<ServiceUpdate>>,
    /// Identifier for this deployment attempt.
    pub id: Uuid,
    /// Service whose configuration was deployed.
    pub service_id: Uuid,
    /// Latest operation phase, refreshed for health on status reads.
    pub phase: ServicePhase,
    /// Configuration snapshot captured when deployment was accepted.
    pub spec: MicroServiceSpec,
    /// Network and host-port settings, including the selected default network.
    /// Domain deployments use zero host ports.
    pub request: DeployMicroServiceRequest,
    /// Immutable OCI image digest, set after image resolution.
    pub digest: Option<String>,
    /// VMs owned by this attempt and still requiring cleanup.
    pub instances: Vec<ServiceInstance>,
    /// Deployment progress messages in chronological order.
    pub log: Vec<String>,
    /// Domain publication was attempted and has not yet been confirmed withdrawn.
    /// Persisted before the helper call so interrupted publication remains owned.
    #[serde(default)]
    pub domain_route_owned: bool,
    /// Failure description, if deployment or cleanup failed.
    pub error: Option<String>,
}

/// How Deploy replaces the current healthy instances.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployStrategy {
    /// Replace one old instance at a time with at most one additional VM.
    #[default]
    Rolling,
    /// Prepare all replacements before switching traffic.
    BlueGreen,
}

/// Update a running Nginx-backed HTTP service without stopping it first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdateServiceRequest {
    /// Reject a stale editor/plan when another release has become active.
    #[serde(default)]
    pub expected_release_id: Option<Uuid>,
    /// Client-generated idempotency identity; reuse it after a lost response.
    pub operation_id: Uuid,
    /// Complete configuration of the desired release.
    pub spec: MicroServiceSpec,
    /// Rolling by default; blue-green needs capacity for every new replica.
    #[serde(default)]
    pub strategy: DeployStrategy,
}

/// Redeploy one successful, digest-pinned release through the same executor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RollbackServiceRequest {
    /// Active release reviewed by the caller.
    #[serde(default)]
    pub expected_release_id: Option<Uuid>,
    /// Client-generated idempotency identity.
    pub operation_id: Uuid,
    /// A successful release belonging to this exact service.
    pub release_id: Uuid,
    /// Replacement strategy.
    #[serde(default)]
    pub strategy: DeployStrategy,
}

/// Resume only the exact interrupted operation that the caller inspected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RetryServiceRequest {
    /// Current interrupted Deploy operation.
    pub operation_id: Uuid,
}

/// Public journal holds references and VM ownership, never resolved credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceUpdate {
    /// Original failure is retained independently of later recovery errors.
    #[serde(default)]
    pub failure: Option<String>,
    /// Explicit Stop withdraws traffic and cleans both generations.
    #[serde(default)]
    pub cancelled: bool,
    /// Previous release, without a recursively nested update journal.
    pub previous: Box<MicroServiceDeployment>,
    /// Original accepted configuration used to detect conflicting retries.
    pub requested_spec: MicroServiceSpec,
    /// Requested replacement strategy.
    pub strategy: DeployStrategy,
    /// Source release for an explicit rollback.
    pub rollback_of: Option<Uuid>,
    /// Recovery must restore the previous release before retiring candidates.
    pub restoring: bool,
    /// New deployment owns the current route after its first transition.
    pub routed_new: bool,
    /// Exact transition saved before calling the privileged helper.
    pub transition: Option<ServiceRouteTransition>,
    /// VM identities waiting for verified drain and cleanup.
    pub retiring: Vec<Uuid>,
    /// Terminal attempts retain metadata but no extra live resources.
    pub completed: bool,
}

/// A compare-and-swap route change that can be retried after a lost response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceRouteTransition {
    /// Expected deployment owner before the change.
    pub expected_deployment_id: Uuid,
    /// Exact previously observed upstreams.
    pub expected_upstreams: Vec<String>,
    /// New deployment owner.
    pub next_deployment_id: Uuid,
    /// Exact healthy upstreams to publish.
    pub next_upstreams: Vec<String>,
    /// Worker boot/PID/start identities observed before reload.
    pub workers: Vec<String>,
    /// Helper confirmed the effective Nginx generation.
    pub confirmed: bool,
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

//! Single-host Sandbox environments. These records never carry credentials.
use crate::{MicroNetworkDnsMode, SandboxPrincipal, SandboxRuntimeRef};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Container engine inside a managed MicroVM, never on the controller host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxContainerRuntime {
    /// A digest-pinned Docker image.
    Docker,
    /// A digest-pinned OCI image executed with containerd and crun.
    Crun,
    /// A pre-installed, content-addressed nspawn root filesystem.
    Nspawn,
}

/// Execution and data retention are explicit, independent of task kind.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SandboxExecutionMode {
    /// An owned container inside a managed guest.
    AgentSandbox {
        /// Guest container engine.
        runtime: SandboxContainerRuntime,
        /// Docker/crun repo@sha256:digest or nspawn sha256:digest.
        image: String,
    },
    /// Direct execution; code, packages and configuration survive stop/resume.
    PersistentWorkspace,
}

/// Immutable environment generation and selected role captured at admission.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SandboxEnvironmentRef {
    /// Saved environment identity.
    pub id: Uuid,
    /// Captured generation; a new environment never inherits old attempts.
    pub generation: Uuid,
    /// Exact named role selected for the attempt.
    pub role: String,
}

/// Complete template identity, checked before provisioning and dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SandboxTemplatePin {
    /// Registered template alias.
    pub name: String,
    /// Immutable registered version.
    pub version: String,
    /// Kernel content SHA256.
    pub kernel_sha256: String,
    /// Root filesystem content SHA256.
    pub rootfs_sha256: String,
    /// Optional initrd content SHA256.
    pub initrd_sha256: Option<String>,
    /// Boot arguments SHA256.
    pub boot_args_sha256: String,
}

/// Declared TCP service, used for discovery and application readiness.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SandboxEnvironmentService {
    /// Service name unique within its role.
    pub name: String,
    /// Guest TCP port.
    pub port: u16,
    /// Whether this service must accept a connection before dispatch.
    #[serde(default)]
    pub readiness: bool,
}

/// One dedicated MicroVM with fixed image and resource limits.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SandboxEnvironmentRole {
    /// Stable role name, such as debug, test, backend or db.
    pub name: String,
    /// Exact template contents.
    pub template: SandboxTemplatePin,
    /// VM vCPU count.
    pub cpu: u8,
    /// VM memory in MiB.
    pub memory_mib: u32,
    /// Dedicated root disk capacity in GiB.
    pub disk_gb: u16,
    /// Roles whose three readiness checks must pass before this VM starts.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Named TCP services exposed by this role.
    #[serde(default)]
    pub services: Vec<SandboxEnvironmentService>,
    /// Guest execution mode.
    pub execution_mode: SandboxExecutionMode,
}

/// One permitted TCP connection between exact roles in this environment.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SandboxEnvironmentAllow {
    /// Originating role.
    pub from: String,
    /// Destination role.
    pub to: String,
    /// Exact destination TCP port, declared by the destination role.
    pub port: u16,
}

/// Temporary cleanup requires a successful durable result export.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SandboxEnvironmentRetention {
    /// Optional lifetime; retained Workspaces cannot use automatic TTL deletion.
    pub ttl_seconds: Option<u64>,
}

/// Immutable single-host manifest. Only newly reserved resources are managed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SandboxEnvironmentSpec {
    /// Manifest protocol. Currently 1.
    pub version: u16,
    /// Dedicated, non-overlapping IPv4 subnet. IPv6 is disabled.
    pub subnet_cidr: String,
    /// Dedicated VM definitions.
    pub roles: Vec<SandboxEnvironmentRole>,
    /// Complete allow list; omitted connections and Internet egress are denied.
    #[serde(default)]
    pub allow: Vec<SandboxEnvironmentAllow>,
    /// Data retention rules.
    #[serde(default)]
    pub retention: SandboxEnvironmentRetention,
}

/// Readiness must be observed for each layer independently.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxReadiness {
    /// Not yet checked or prerequisite unavailable.
    #[default]
    Pending,
    /// This exact runtime passed its check.
    Ready,
    /// The guest lacks a required operation.
    Unsupported,
    /// A check failed.
    Failed,
}

/// Named service resolved only within its owning environment.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxDiscoveredService {
    /// Declared service name.
    pub name: String,
    /// Current dedicated VM IPv4 address.
    pub address: String,
    /// Declared TCP port.
    pub port: u16,
    /// Application probe result, independent of network and agent readiness.
    pub ready: bool,
}

/// Reserved resource identities remain stable through partial provisioning.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxEnvironmentRoleState {
    /// Manifest role name.
    pub role: String,
    /// Environment-owned VM UUID, reserved before creation.
    pub vm_id: Uuid,
    /// Environment-owned security group UUID, reserved before creation.
    pub security_group_id: Uuid,
    /// Observed boot identity; absent before the VM starts.
    pub runtime: Option<SandboxRuntimeRef>,
    /// Current lease, never an arbitrary external address.
    pub ipv4: Option<String>,
    /// Authenticated guest execution capability.
    pub agent: SandboxReadiness,
    /// Lease and host network reconciliation status.
    pub network: SandboxReadiness,
    /// Declared application health status.
    pub application: SandboxReadiness,
    /// Current declared services.
    pub services: Vec<SandboxDiscoveredService>,
    /// Bounded, sanitized check failure.
    pub error: Option<String>,
}

/// Durable desired lifecycle; reconciliation never retries task effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxEnvironmentState {
    /// Continue provisioning the recorded resource IDs.
    Provisioning,
    /// All roles passed readiness.
    Ready,
    /// Stop requested; root disks and configuration stay retained.
    Stopped,
    /// Export succeeded and owned-resource deletion is in progress.
    CleanupPending,
    /// All owned resources are gone; the manifest remains as a tombstone.
    Deleted,
    /// Provisioning or readiness needs explicit reconciliation.
    Failed,
}

/// Durable environment and its complete ownership ledger.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxEnvironment {
    /// Client-generated idempotent creation identity.
    pub id: Uuid,
    /// Owning project.
    pub project_id: Uuid,
    /// Authenticated creator.
    pub owner: SandboxPrincipal,
    /// Immutable resource generation.
    pub generation: Uuid,
    /// Immutable requested manifest.
    pub spec: SandboxEnvironmentSpec,
    /// Dedicated owned network UUID reserved before creation.
    pub network_id: Uuid,
    /// Immutable expected DNS posture. Older saved environments retain legacy forwarding.
    #[serde(default)]
    pub dns_mode: MicroNetworkDnsMode,
    /// Reserved dedicated role resources and observed readiness.
    pub roles: Vec<SandboxEnvironmentRoleState>,
    /// Desired lifecycle and reconciliation status.
    pub state: SandboxEnvironmentState,
    /// Creation time in Unix milliseconds.
    pub created_at: u64,
    /// Last checkpoint in Unix milliseconds.
    pub updated_at: u64,
    /// Earliest automatic cleanup time; absent for retained Workspaces.
    pub expires_at: Option<u64>,
    /// Last verified durable result export.
    pub exported_at: Option<u64>,
    /// Sanitized reconciliation failure.
    pub error: Option<String>,
}

/// Final Workspace deletion always requires an explicit request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SandboxEnvironmentDelete {
    /// Explicit authorization to permanently delete retained Workspace disks.
    #[serde(default)]
    pub authorize_workspace_deletion: bool,
}

/// Evidence that every environment attempt was durably exported before cleanup.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxEnvironmentExport {
    /// Exact environment generation.
    pub environment: SandboxEnvironmentRef,
    /// Verified exported task records.
    pub task_ids: Vec<Uuid>,
    /// Verification timestamp.
    pub exported_at: u64,
}

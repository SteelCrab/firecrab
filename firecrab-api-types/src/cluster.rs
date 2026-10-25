//! Authenticated hosts and durable, host-owned VM placement (#455).

use crate::{EgressPolicy, VmResponse};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

/// A host-local resource is identified by both fields, never by its ID alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HostResourceRef {
    /// Certificate UUID of the owning host.
    pub host_id: Uuid,
    /// ID within that host's VM, network, or storage namespace.
    pub resource_id: String,
}

/// CPU and memory quantities for capacity, usage, or reservations.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HostCapacity {
    /// Virtual CPU units.
    pub cpu: u32,
    /// Memory quantity in MiB.
    pub ram_mib: u64,
}

/// A worker's immutable image fingerprint, not a mutable registry tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HostImage {
    /// Installed template alias.
    pub alias: String,
    /// Pinned template version.
    pub version: String,
    /// SHA256 of the verified kernel.
    pub kernel_sha256: String,
    /// SHA256 of the optional initrd; absence is part of the fingerprint.
    #[serde(default)]
    pub initrd_sha256: Option<String>,
    /// SHA256 of the verified root filesystem.
    pub rootfs_sha256: String,
    /// SHA256 of the kernel command line.
    pub boot_args_sha256: String,
}

/// A named host-local resource advertised by its authenticated owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HostNetwork {
    /// Stable resource UUID.
    pub id: Uuid,
    /// Operator-visible name.
    pub name: String,
}

/// Disk quota and reservations outside this controller's managed placements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HostStorage {
    /// Registered storage-root ID within the host.
    pub id: String,
    /// Allocatable disk quota in GiB, including VM reservations.
    pub capacity_gib: u64,
    /// Disk reserved by local VMs outside controller ownership.
    pub unmanaged_gib: u64,
}

/// Worker registration and heartbeat. The ID must match the TLS peer UUID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HostReport {
    /// Stable resource UUID.
    pub id: Uuid,
    /// Operator-visible name.
    pub name: String,
    /// Reachable HTTPS origin without credentials, path, query or fragment.
    pub endpoint: String,
    /// Worker CPU architecture.
    pub architecture: String,
    /// Allocatable CPU and memory.
    pub capacity: HostCapacity,
    /// CPU and memory reserved outside this controller.
    pub unmanaged: HostCapacity,
    /// Installed boot-image fingerprints.
    pub images: Vec<HostImage>,
    /// Networks local to this authenticated host.
    pub networks: Vec<HostNetwork>,
    /// Local storage IDs and disk quotas.
    pub storages: Vec<HostStorage>,
    /// Exact-match placement labels.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// Online is derived from the controller's receipt time, never a worker clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterHost {
    /// Latest authenticated worker inventory.
    pub report: HostReport,
    /// Controller receipt time in Unix milliseconds.
    pub last_seen_ms: i64,
    /// Whether new placements are blocked; existing VMs remain owned.
    pub draining: bool,
    /// Whether the report is within the heartbeat deadline.
    pub online: bool,
    /// CPU and memory reserved by nonterminal placements.
    pub reserved: HostCapacity,
    /// Nonterminal disk reservations by local storage ID.
    pub reserved_disk_gib: BTreeMap<String, u64>,
}

/// Drain prevents new placement; it does not migrate or stop existing VMs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DrainHostRequest {
    /// Whether new placements are blocked; existing VMs remain owned.
    pub draining: bool,
}

/// Explicit candidates retain each network/storage resource's host ownership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlacementTarget {
    /// Network owned by this candidate host.
    pub network: HostResourceRef,
    /// Storage owned by the same candidate host.
    pub storage: HostResourceRef,
}

/// VM configuration shared by candidates; host-local resources are separate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlacementSpec {
    /// Operator-visible name.
    pub name: String,
    /// Exact required boot-image fingerprint.
    pub image: HostImage,
    /// Virtual CPU units.
    pub cpu: u8,
    /// VM memory in MiB; a supported power of two.
    pub ram: u32,
    /// Disk reservation in GiB.
    pub disk_gb: u16,
    /// Normal host-local VM egress policy.
    #[serde(default)]
    pub egress_policy: EgressPolicy,
    /// Plaintext VM environment, fixed for this placement.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Labels every eligible host must match.
    #[serde(default)]
    pub required_labels: BTreeMap<String, String>,
    /// Explicit candidate-local network/storage pairs, one per host.
    pub targets: Vec<PlacementTarget>,
}

/// Intent is persisted before sending a lifecycle operation to its owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementAction {
    Create,
    Start,
    Stop,
    Delete,
}

/// An uncertain result retains its owner and reservation until acknowledged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementPhase {
    Pending,
    Uncertain,
    Confirmed,
    Rejected,
    Deleted,
}

impl PlacementPhase {
    /// Only durable rejection or a deletion tombstone releases capacity.
    pub fn reserves_capacity(self) -> bool {
        !matches!(self, Self::Rejected | Self::Deleted)
    }
}

/// Controller journal entry; `vm.hostId` and `vm.resourceId` never change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmPlacement {
    /// Stable resource UUID.
    pub id: Uuid,
    /// Controller-wide retry key for this exact creation request.
    pub idempotency_key: String,
    /// Immutable owner host and preassigned VM UUID.
    pub vm: HostResourceRef,
    /// Selected host-local resources; never reassigned.
    pub target: PlacementTarget,
    /// Canonical creation configuration retained unchanged for retries.
    pub spec: PlacementSpec,
    /// Monotonic lifecycle generation.
    pub generation: u64,
    /// Desired operation for this generation.
    pub action: PlacementAction,
    /// Durable acknowledgement state, separate from VM runtime health.
    pub phase: PlacementPhase,
    /// Latest VM observation from the recorded owner, when available.
    pub observed: Option<VmResponse>,
    /// Redacted operation failure or unresolved-owner guidance.
    pub error: Option<String>,
    /// Admission time in Unix milliseconds.
    pub created_at_ms: i64,
}

/// Private worker protocol. Lower generations cannot resurrect deleted VMs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WorkerIntent {
    /// Monotonic lifecycle generation.
    pub generation: u64,
    /// Desired operation for this generation.
    pub action: PlacementAction,
    /// Canonical creation configuration retained unchanged for retries.
    pub spec: PlacementSpec,
    /// Selected host-local resources; never reassigned.
    pub target: PlacementTarget,
}

/// Durable worker receipt; a confirmed delete remains as a tombstone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerReceipt {
    /// Stable resource UUID.
    pub id: Uuid,
    /// Authenticated controller that owns this worker receipt.
    pub controller_id: Uuid,
    /// Exact request and generation accepted by the worker.
    pub intent: WorkerIntent,
    /// Durable acknowledgement state, separate from VM runtime health.
    pub phase: PlacementPhase,
    /// Latest VM observation from the recorded owner, when available.
    pub observed: Option<VmResponse>,
    /// Redacted operation failure or unresolved-owner guidance.
    pub error: Option<String>,
}

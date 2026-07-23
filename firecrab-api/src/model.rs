//! Internal VM/lease record types, re-exporting the wire types shared with
//! `firecrab-frontend` from `firecrab-api-types` alongside server-only
//! fields (e.g. template artifact hashes) that never cross the API boundary.

use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use firecrab_api_types::CreateVmRequest;
pub use firecrab_api_types::EgressPolicy;
pub use firecrab_api_types::PackageUpdateStatus;
pub use firecrab_api_types::StartupStep;
pub use firecrab_api_types::UpdateVmResourcesRequest;
pub use firecrab_api_types::VmState;
pub use firecrab_helper_protocol::network::MacAddr;

/// An active IPv4 + MAC assignment for one VM, drawn from the shared bridge
/// subnet (see `firecrab-net-helper/src/bridge.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    /// The VM this lease belongs to.
    pub vm_id: Uuid,
    /// Allocated IPv4 address.
    pub ipv4: Ipv4Addr,
    /// Allocated MAC address.
    pub mac: MacAddr,
}

/// The full server-side VM record, persisted in [`crate::persistence::Store`]
/// — a superset of [`firecrab_api_types::VmResponse`] with fields (template
/// artifact hashes) the API response never exposes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VmRecord {
    /// Stable identifier, also the `data/vms/<id>/` directory name.
    pub id: Uuid,
    /// User-supplied name.
    pub name: String,
    /// Current lifecycle state.
    pub state: VmState,
    /// Template alias this VM was created from.
    pub template: String,
    /// Pinned template version the alias resolved to at creation time.
    #[serde(default)]
    pub template_version: String,
    /// SHA256 of the template's kernel artifact at creation time.
    #[serde(default)]
    pub template_kernel_sha256: String,
    /// SHA256 of the template's rootfs artifact at creation time.
    #[serde(default)]
    pub template_rootfs_sha256: String,
    /// SHA256 of the template's boot args at creation time.
    #[serde(default)]
    pub template_boot_args_sha256: String,
    /// vCPU count.
    pub cpu: u8,
    /// RAM in MiB.
    pub ram: u32,
    /// Disk capacity in GiB.
    #[serde(default = "default_disk_gb")]
    pub disk_gb: u16,
    /// Outbound network posture, applied on every `start_vm` (see
    /// `setup_vm_network`) — not live, same as cpu/ram/disk.
    #[serde(default)]
    pub egress_policy: EgressPolicy,
    /// Live progress while `state == Starting`; never persisted (a restart
    /// already demotes any in-flight start to `Stopped`, see
    /// `restart_demotes_active_states_to_stopped`) and irrelevant otherwise.
    #[serde(skip)]
    pub startup_step: Option<StartupStep>,
    /// Outcome of the most recent `packages/update` run, if any — never
    /// persisted; a restart loses no state a fresh run can't reproduce, and
    /// it's purely informational (unlike `state`, nothing else in the
    /// lifecycle depends on it).
    #[serde(skip)]
    pub package_update: Option<PackageUpdateStatus>,
}

/// Matches the fixed rootfs template size that applied before disk capacity
/// became configurable, for records written before this field existed.
fn default_disk_gb() -> u16 {
    2
}

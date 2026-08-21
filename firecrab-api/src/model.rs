//! Internal VM/lease record types, re-exporting the wire types shared with
//! `firecrab-frontend` from `firecrab-api-types` alongside server-only
//! fields (e.g. template artifact hashes) that never cross the API boundary.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use firecrab_api_types::CreateVmRequest;
pub use firecrab_api_types::EgressPolicy;
pub use firecrab_api_types::UpdateVmResourcesRequest;
pub use firecrab_api_types::VmState;
pub use firecrab_api_types::{StartupStep, StartupStepOutcome, StartupStepRun};
pub use firecrab_helper_protocol::network::MacAddr;

/// An active IPv4 + MAC assignment for one VM, drawn from the shared bridge
/// subnet (see `firecrab-net-helper/src/bridge.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    /// The VM this lease belongs to.
    pub vm_id: Uuid,
    /// Allocated IPv4 address.
    pub ipv4: Ipv4Addr,
    /// Allocated IPv6 address, when the VM's MicroNetwork is dual-stack.
    pub ipv6: Option<std::net::Ipv6Addr>,
    /// Allocated MAC address.
    pub mac: MacAddr,
}

/// What a VM record represents. Only `Builder` VMs are hidden from the
/// dashboard's normal list — everything else about their lifecycle (start,
/// console, stop, delete) is identical to a user-created instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmPurpose {
    /// A user-created VM, shown in the normal MicroVM list.
    #[default]
    Instance,
    /// A short-lived VM driving an image job (`handlers::bootstrap`) — never
    /// shown in `list_vms`; its own session endpoint reports on it instead.
    Builder,
}

impl VmPurpose {
    pub fn id(self) -> &'static str {
        match self {
            VmPurpose::Instance => "instance",
            VmPurpose::Builder => "builder",
        }
    }
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
    /// What this record represents — see [VmPurpose]. `#[serde(default)]`
    /// because legacy `vms.json` records predate this field.
    #[serde(default)]
    pub purpose: VmPurpose,
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
    /// The MicroNetwork this VM belongs to. Fixed at creation: the VM's
    /// lease comes out of that network's subnet, so moving it would mean
    /// reallocating the address its guest already booted with.
    pub micro_network_id: Uuid,
    /// Storage root id (from `FIRECRAB_STORAGE_ROOTS` / `GET /api/storage`).
    /// Disks live at `{root}/vms/{id}/`. Defaults to `"default"` so records
    /// written before multi-disk support keep the legacy `data/vms` path.
    #[serde(default = "default_storage_root")]
    pub storage_root: String,
    /// Active disk generation UUID. Host path is derived as
    /// `{vms}/{vm}/disks/{generation}.ext4` — never stored as an absolute path.
    /// `None` until the first successful prepare.
    #[serde(default)]
    pub disk_generation: Option<Uuid>,
    /// Most recent start's runtime directory id (config/socket/console under
    /// `runtimes/{id}/`). Used for log reads after the process exits.
    #[serde(default)]
    pub last_runtime_id: Option<Uuid>,
    /// Live progress while `state == Starting`; never persisted (a restart
    /// already demotes any in-flight start to `Stopped`, see
    /// `restart_demotes_active_states_to_stopped`) and irrelevant otherwise.
    #[serde(skip)]
    pub startup_step: Option<StartupStep>,
    /// Timed record of the most recent start attempt's steps. Transient for
    /// the same reason as `startup_step` — a restart demotes any in-flight
    /// start, so there is no half-finished timeline worth persisting.
    #[serde(skip)]
    pub startup_timeline: Vec<StartupStepRun>,
    /// Per-VM environment applied on the next start into
    /// `/etc/firecrab/services.d/app`. Empty is valid. JSON on the row.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Matches the fixed rootfs template size that applied before disk capacity
/// became configurable, for records written before this field existed.
fn default_disk_gb() -> u16 {
    2
}

/// Id of the implicit storage root when `FIRECRAB_STORAGE_ROOTS` is unset.
fn default_storage_root() -> String {
    "default".to_owned()
}

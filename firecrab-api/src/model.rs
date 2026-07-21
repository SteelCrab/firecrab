use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use firecrab_api_types::CreateVmRequest;
pub use firecrab_api_types::StartupStep;
pub use firecrab_api_types::VmState;
pub use firecrab_helper_protocol::network::MacAddr;

/// An active IPv4 + MAC assignment for one VM, drawn from the shared bridge
/// subnet (see `firecrab-net-helper/src/bridge.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    pub vm_id: Uuid,
    pub ipv4: Ipv4Addr,
    pub mac: MacAddr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VmRecord {
    pub id: Uuid,
    pub name: String,
    pub state: VmState,
    pub template: String,
    #[serde(default)]
    pub template_version: String,
    #[serde(default)]
    pub template_kernel_sha256: String,
    #[serde(default)]
    pub template_rootfs_sha256: String,
    #[serde(default)]
    pub template_boot_args_sha256: String,
    pub cpu: u8,
    pub ram: u32,
    #[serde(default = "default_disk_gb")]
    pub disk_gb: u16,
    /// Live progress while `state == Starting`; never persisted (a restart
    /// already demotes any in-flight start to `Stopped`, see
    /// `restart_demotes_active_states_to_stopped`) and irrelevant otherwise.
    #[serde(skip)]
    pub startup_step: Option<StartupStep>,
}

/// Matches the fixed rootfs template size that applied before disk capacity
/// became configurable, for records written before this field existed.
fn default_disk_gb() -> u16 {
    2
}

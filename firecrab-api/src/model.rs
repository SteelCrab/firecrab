use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use firecrab_api_types::CreateVmRequest;
pub use firecrab_api_types::VmState;

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub cpu: f64,
    pub ram: u32,
}

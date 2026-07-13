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
    pub cpu: f64,
    pub ram: u32,
}

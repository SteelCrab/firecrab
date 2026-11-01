use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One retryable request reserves exactly one Ubuntu coding VM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateWorkspaceRequest {
    pub id: Uuid,
    #[serde(default)]
    pub name: String,
    #[serde(default = "cpu")]
    pub cpu: u8,
    #[serde(default = "ram")]
    pub ram: u32,
    #[serde(default = "disk")]
    pub disk_gb: u16,
    #[serde(default)]
    pub micro_network_id: Option<Uuid>,
    #[serde(default)]
    pub storage_root: Option<String>,
}

fn cpu() -> u8 {
    2
}
fn ram() -> u32 {
    4096
}
fn disk() -> u16 {
    32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceSetupState {
    Queued,
    Running,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceSetupPhase {
    Image,
    Network,
    Create,
    Start,
    Complete,
}

/// Public progress contains no guest credentials or host paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSetup {
    pub id: Uuid,
    pub vm_id: Uuid,
    pub name: String,
    pub cpu: u8,
    pub ram: u32,
    pub disk_gb: u16,
    pub micro_network_id: Option<Uuid>,
    pub storage_root: Option<String>,
    pub state: WorkspaceSetupState,
    pub phase: WorkspaceSetupPhase,
    pub error: Option<String>,
}

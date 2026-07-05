use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VmState {
    Created,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmRecord {
    pub id: Uuid,
    pub name: String,
    pub state: VmState,
    pub template: String,
    pub cpu: f64,
    pub ram: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateVmRequest {
    pub name: String,
    pub template: String,
    pub ram: u32,
    pub cpu: f64,
}
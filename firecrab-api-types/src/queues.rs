//! Host-local RabbitMQ engine and durable application queues.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueAccess {
    Publish,
    Consume,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct QueueBinding {
    pub queue_id: Uuid,
    pub access: QueueAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueEnginePhase {
    Installing,
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateQueueEngineRequest {
    pub micro_network_id: Uuid,
    #[serde(default = "default_cpu")]
    pub cpu: u8,
    #[serde(default = "default_ram")]
    pub ram: u32,
    #[serde(default = "default_disk")]
    pub disk_gb: u16,
    #[serde(default)]
    pub storage_root: Option<String>,
}
fn default_cpu() -> u8 {
    4
}
fn default_ram() -> u32 {
    4096
}
fn default_disk() -> u16 {
    8
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEngineResponse {
    pub vm_id: Uuid,
    pub micro_network_id: Uuid,
    pub phase: QueueEnginePhase,
    pub image: String,
    pub error: Option<String>,
    pub log: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateQueueRequest {
    pub name: String,
    #[serde(default = "default_delivery_limit")]
    pub delivery_limit: u16,
    #[serde(default = "default_max_bytes")]
    pub max_bytes: u64,
}
fn default_delivery_limit() -> u16 {
    5
}
fn default_max_bytes() -> u64 {
    64 * 1024 * 1024
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueuePhase {
    Provisioning,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueServiceRef {
    pub id: Uuid,
    pub name: String,
    pub bindings: BTreeMap<String, QueueAccess>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueResponse {
    pub id: Uuid,
    pub name: String,
    pub phase: QueuePhase,
    pub delivery_limit: u16,
    pub max_bytes: u64,
    pub error: Option<String>,
    pub services: Vec<QueueServiceRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueMetrics {
    pub ready: u64,
    pub unacknowledged: u64,
    pub consumers: u64,
    pub failed: u64,
}

/// Returned only by an explicit reveal request; never in lists or service specs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueConnection {
    pub url: String,
    pub queue: String,
}

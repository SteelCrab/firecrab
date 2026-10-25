//! Host-local RabbitMQ engine and durable application queues.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

/// Application permission granted by a Queue connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueAccess {
    /// Publish to the main queue without consume or configure permission.
    Publish,
    /// Read the main and failure queues without publish or configure permission.
    Consume,
}

/// Credential-free service reference to one Queue and one application role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct QueueBinding {
    /// Stable identifier of the referenced Queue.
    pub queue_id: Uuid,
    /// Required publisher or consumer permission.
    pub access: QueueAccess,
}

/// Lifecycle state of the single persistent RabbitMQ engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueEnginePhase {
    /// The pinned image is being imported.
    Installing,
    /// The existing broker VM is starting or recovering.
    Starting,
    /// The broker VM runs and its management API responds.
    Running,
    /// The broker VM is stopping while retaining its data.
    Stopping,
    /// The broker is stopped and its disk is retained.
    Stopped,
    /// Provisioning or runtime verification failed; explicit recovery is required.
    Failed,
}

/// Network and resource allocation for a new internal RabbitMQ engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateQueueEngineRequest {
    /// Existing private network shared with connected applications.
    pub micro_network_id: Uuid,
    /// Guest virtual CPUs; defaults to four.
    #[serde(default = "default_cpu")]
    pub cpu: u8,
    /// Guest memory in MiB; defaults to 4096.
    #[serde(default = "default_ram")]
    pub ram: u32,
    /// Persistent guest disk capacity in GiB; defaults to eight.
    #[serde(default = "default_disk")]
    pub disk_gb: u16,
    /// Optional registered storage root for the broker disk.
    #[serde(default)]
    pub storage_root: Option<String>,
}
/// Default broker CPU allocation.
fn default_cpu() -> u8 {
    4
}
/// Default broker memory allocation in MiB.
fn default_ram() -> u32 {
    4096
}
/// Default persistent broker disk capacity in GiB.
fn default_disk() -> u16 {
    8
}

/// Public engine state without administrator credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEngineResponse {
    /// Stable identity of the broker VM; recovery must reuse it.
    pub vm_id: Uuid,
    /// Private network carrying AMQP and management traffic.
    pub micro_network_id: Uuid,
    /// Reconciled engine lifecycle state.
    pub phase: QueueEnginePhase,
    /// Official image reference pinned by its immutable digest.
    pub image: String,
    /// Sanitized recovery or provisioning failure, if any.
    pub error: Option<String>,
    /// Image-import progress for the internal broker template.
    pub log: String,
}

/// Declaration settings for a main queue and its failure queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateQueueRequest {
    /// Unique queue name with 1-40 lowercase letters, digits or hyphens.
    pub name: String,
    /// Failed redelivery limit before work moves to the failure queue.
    #[serde(default = "default_delivery_limit")]
    pub delivery_limit: u16,
    /// Byte limit applied separately to the main and failure queues.
    #[serde(default = "default_max_bytes")]
    pub max_bytes: u64,
}
/// Default number of failed redeliveries before dead-lettering.
fn default_delivery_limit() -> u16 {
    5
}
/// Default 64 MiB limit for each main and failure queue.
fn default_max_bytes() -> u64 {
    64 * 1024 * 1024
}

/// Progress of a persisted application queue declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueuePhase {
    /// RabbitMQ resources are being declared with persisted identities.
    Provisioning,
    /// The main queue, failure queue and role permissions are available.
    Ready,
    /// Declaration was interrupted or rejected and can be retried.
    Failed,
}

/// Saved service bindings that prevent a referenced Queue from being deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueServiceRef {
    /// Identifier of the saved MicroService configuration.
    pub id: Uuid,
    /// Display name of the referencing service.
    pub name: String,
    /// Binding aliases and their access roles for this Queue.
    pub bindings: BTreeMap<String, QueueAccess>,
}

/// Public queue settings and referencing services without connection passwords.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueResponse {
    /// Stable identity used by vhost and application credentials.
    pub id: Uuid,
    /// Application queue name and prefix of its failure queue.
    pub name: String,
    /// Declaration state; failed provisioning retains its identity for retry.
    pub phase: QueuePhase,
    /// Configured failed-redelivery limit.
    pub delivery_limit: u16,
    /// Capacity of each main or failure queue in bytes.
    pub max_bytes: u64,
    /// Sanitized declaration failure, if any.
    pub error: Option<String>,
    /// Saved services that still reference this Queue.
    pub services: Vec<QueueServiceRef>,
}

/// Message and consumer counts sampled from the RabbitMQ management API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueMetrics {
    /// Main-queue messages waiting for a consumer.
    pub ready: u64,
    /// Main-queue deliveries awaiting manual acknowledgement.
    pub unacknowledged: u64,
    /// Active consumers of the main queue.
    pub consumers: u64,
    /// Messages retained in the failure queue.
    pub failed: u64,
}

/// Returned only by an explicit reveal request; never in lists or service specs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueConnection {
    /// AMQP URL containing the role username, password and vhost.
    pub url: String,
    /// Queue name used as the routing key on the default exchange.
    pub queue: String,
}

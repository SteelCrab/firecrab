//! Saved-token preview bindings. Registration credentials are never accepted here.
use crate::{SandboxEnvironmentRef, SandboxPrincipal, SandboxRuntimeRef};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use uuid::Uuid;

/// Explicit application selected from one admitted task runtime.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SandboxTunnelTarget {
    /// Exact declared service in the environment generation captured by the task.
    EnvironmentService {
        environment: SandboxEnvironmentRef,
        service: String,
    },
    /// Port on the task's original VM generation; no caller-supplied IP address.
    VmApp {
        runtime: SandboxRuntimeRef,
        port: u16,
    },
    /// Existing public-eligible Nginx routes. Every upstream must belong to the task runtime.
    Nginx { host: String },
}

/// Idempotent binding configuration; task and address cannot change after creation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxTunnelSpec {
    pub task_id: Uuid,
    /// Address registered once through Pipe's private token flow.
    pub subdomain: String,
    pub target: SandboxTunnelTarget,
}

/// Desired state persists across API/browser restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxTunnelState {
    Waiting,
    Connecting,
    Connected,
    Stopping,
    Stopped,
    Deleting,
    Deleted,
    NeedsReconciliation,
}

/// A helper heartbeat alone cannot verify public HTTPS, WebSockets or shutdown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxTunnelVerification {
    #[default]
    Unverified,
    Verified,
}

/// Remote reservations stay under the registration dashboard's authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxTunnelRetirement {
    NotRequested,
    OperatorRequired,
}

/// Last exact helper target, retained to detect edits made by another entry point.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SandboxTunnelResolvedTarget {
    Apps { upstreams: Vec<SocketAddr> },
    Nginx { host: String },
}

/// Durable preview/result reference. Local and remote outcomes are independent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxTunnelBinding {
    pub id: Uuid,
    pub project_id: Uuid,
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub runtime: SandboxRuntimeRef,
    pub owner: SandboxPrincipal,
    pub spec: SandboxTunnelSpec,
    pub state: SandboxTunnelState,
    pub app_ready: bool,
    pub connector_id: Option<Uuid>,
    pub applied_target: Option<SandboxTunnelResolvedTarget>,
    pub public_url: Option<String>,
    pub helper_connected: bool,
    pub public_https: SandboxTunnelVerification,
    pub web_socket_handshake: SandboxTunnelVerification,
    pub public_shutdown: SandboxTunnelVerification,
    pub remote_retirement: SandboxTunnelRetirement,
    pub local_removed: bool,
    pub created_at: u64,
    pub updated_at: u64,
    pub error_code: Option<String>,
}

/// Explicit public checks against the saved fixed URL; redirects are never followed.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxTunnelVerify {
    /// Literal application path; no alternate origins or credentials.
    pub path: String,
    /// Exact SHA-256 of the expected bounded HTTP body.
    pub body_sha256: Option<String>,
    /// Request a WebSocket protocol upgrade instead of an HTTP body check.
    #[serde(default)]
    pub web_socket: bool,
}

/// Non-secret saved address available for an explicit binding selection.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxTunnelAddress {
    pub subdomain: String,
    pub public_url: String,
    pub connected: bool,
    pub claimed_by: Option<Uuid>,
}

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
        /// Exact environment generation and role captured by the task.
        environment: SandboxEnvironmentRef,
        /// Service name declared in that role's immutable manifest.
        service: String,
    },
    /// Port on the task's original VM generation; no caller-supplied IP address.
    VmApp {
        /// Admitted VM and runtime identities; a new boot requires retargeting.
        runtime: SandboxRuntimeRef,
        /// Application TCP port on that exact VM.
        port: u16,
    },
    /// Existing public-eligible Nginx routes. Every upstream must belong to the task runtime.
    Nginx {
        /// Saved Nginx host whose upstream ownership is checked at publication.
        host: String,
    },
}

/// Idempotent binding configuration; task and address cannot change after creation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxTunnelSpec {
    /// Existing task whose attempt and runtime authorize the preview.
    pub task_id: Uuid,
    /// Address registered once through Pipe's private token flow.
    pub subdomain: String,
    /// Declared application target resolved by the server.
    pub target: SandboxTunnelTarget,
}

/// Desired state persists across API/browser restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxTunnelState {
    /// Application readiness has not yet been confirmed.
    Waiting,
    /// The saved connector is starting or reconnecting.
    Connecting,
    /// The helper reports a connection; public probes remain separate.
    Connected,
    /// Stop requested; the prior target remains reserved until confirmed.
    Stopping,
    /// Local forwarding is confirmed stopped; the saved address is retained.
    Stopped,
    /// Local binding deletion is in progress.
    Deleting,
    /// The local binding has been removed; remote retirement may still be required.
    Deleted,
    /// An explicit successor owns the saved address; this record can no longer control it.
    Superseded,
    /// Observed connector state or ownership differs from the saved binding.
    NeedsReconciliation,
}

/// A helper heartbeat alone cannot verify public HTTPS, WebSockets or shutdown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxTunnelVerification {
    /// No matching successful public probe has been recorded.
    #[default]
    Unverified,
    /// The explicit public check succeeded for the saved target.
    Verified,
}

/// Remote reservations stay under the registration dashboard's authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxTunnelRetirement {
    /// Remote address or token retirement has not been requested.
    NotRequested,
    /// The operator must retire the remote reservation through its registration service.
    OperatorRequired,
}

/// Last exact helper target, retained to detect edits made by another entry point.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SandboxTunnelResolvedTarget {
    /// Exact VM socket addresses last applied to the connector.
    Apps {
        /// Server-resolved upstreams; callers cannot supply arbitrary addresses.
        upstreams: Vec<SocketAddr>,
    },
    /// Saved Nginx host last applied to the connector.
    Nginx {
        /// Validated saved host name.
        host: String,
    },
}

/// Durable preview/result reference. Local and remote outcomes are independent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxTunnelBinding {
    /// Durable binding identity, retained in the retirement inventory after deletion.
    pub id: Uuid,
    /// Project that authorizes all binding operations.
    pub project_id: Uuid,
    /// Original task; retargeting creates a successor instead of rewriting it.
    pub task_id: Uuid,
    /// Original immutable attempt identity.
    pub attempt_id: Uuid,
    /// Original admitted VM generation.
    pub runtime: SandboxRuntimeRef,
    /// Principal that owns this binding.
    pub owner: SandboxPrincipal,
    /// Saved address and declared target.
    pub spec: SandboxTunnelSpec,
    /// Desired and observed local lifecycle state.
    pub state: SandboxTunnelState,
    /// Application readiness observed before connector startup.
    pub app_ready: bool,
    /// Helper-side connector identity, absent before creation.
    pub connector_id: Option<Uuid>,
    /// Exact target applied to the connector, retained until stop is confirmed.
    pub applied_target: Option<SandboxTunnelResolvedTarget>,
    /// Fixed HTTPS address allocated by the saved Pipe registration.
    pub public_url: Option<String>,
    /// Latest helper heartbeat status; does not prove public reachability.
    pub helper_connected: bool,
    /// Explicit public HTTPS response verification status.
    pub public_https: SandboxTunnelVerification,
    /// Explicit public WebSocket upgrade verification status.
    pub web_socket_handshake: SandboxTunnelVerification,
    /// Verification that public forwarding stopped after local shutdown.
    pub public_shutdown: SandboxTunnelVerification,
    /// Separate status of remote token and address retirement.
    pub remote_retirement: SandboxTunnelRetirement,
    /// Local metadata and connector removal have been confirmed.
    pub local_removed: bool,
    /// Creation time in Unix milliseconds.
    pub created_at: u64,
    /// Last durable update in Unix milliseconds.
    pub updated_at: u64,
    /// Sanitized failure code; never contains registration credentials.
    pub error_code: Option<String>,
    /// Predecessor whose saved address was explicitly transferred to this binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_binding_id: Option<Uuid>,
    /// Successor that exclusively owns the address after transfer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub successor_binding_id: Option<Uuid>,
}

/// New client identity and current task/runtime; the original attempt never changes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxTunnelRetarget {
    /// New successor binding identity, chosen before an uncertain create.
    pub id: Uuid,
    /// Current task whose admitted runtime authorizes the successor target.
    pub task_id: Uuid,
    /// New declared target; the saved address is inherited from the predecessor.
    pub target: SandboxTunnelTarget,
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
    /// Saved registration address available for an explicit selection.
    pub subdomain: String,
    /// Fixed public HTTPS URL for that registration.
    pub public_url: String,
    /// Current helper connection status, independent of a public probe.
    pub connected: bool,
    /// Active binding that reserves the address, when present.
    pub claimed_by: Option<Uuid>,
}

//! Durable native mutations that must keep authority beyond an HTTP waiter.

use crate::ResourceOwnership;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A typed backend action; public callers cannot supply arbitrary commands or paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeAction {
    /// Delete one project's private workspace and metadata; borrowed sources survive.
    DeleteSandboxProject,
    /// Disconnect Git configuration without deleting its Stack or infrastructure.
    DeleteStackSource,
}

/// Native work remains fenced until completion or explicit recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeOperationState {
    /// A server worker may be changing the resource.
    Running,
    /// The outcome is uncertain; ownership and identity stay reserved.
    NeedsRecovery,
    /// The recorded action completed; this acknowledgement never changes.
    Succeeded,
}

/// Public operation metadata contains no desired values, credentials or filesystem paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeOperation {
    /// Stable operation UUID retained across recovery.
    pub operation_id: Uuid,
    /// Exact endpoint, resource and authority captured before dispatch.
    pub ownership: ResourceOwnership,
    /// Explicit backend action accepted by the server.
    pub action: NativeAction,
    /// Durable completion or recovery status.
    pub state: NativeOperationState,
    /// Static failure category, without upstream messages or supplied values.
    pub error_code: Option<String>,
    /// Original configuration generation for a versioned metadata removal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration_generation: Option<u64>,
}

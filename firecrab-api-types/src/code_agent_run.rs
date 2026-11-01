//! A user-requested, single-use agent invocation against one retained CODE runtime.
use crate::CodeAgent;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
/// Server derives the agent and AUTH account from the unchanged saved workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodeAgentRunRequest {
    /// Current saved revision.
    pub expected_revision: u64,
    /// Observed workspace VM.
    pub vm_id: Uuid,
    /// Observed VM incarnation.
    pub runtime_id: Uuid,
    /// Observed completed lifecycle operation.
    pub operation_id: Uuid,
    /// Optional model override; empty uses the pinned agent default.
    #[serde(default)]
    pub model: String,
    /// Explicit task instructions, at most 16 KiB.
    pub prompt: String,
}
/// Invocation lifetime. Interrupted work is never automatically replayed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeAgentRunPhase {
    /// Reserved before side effects.
    Queued,
    /// Guest CLI configuration and launch in progress.
    Starting,
    /// The assigned provider accepted a model request.
    Running,
    /// Explicit cancellation is being applied.
    Cancelling,
    /// CLI and provider completed successfully.
    Succeeded,
    /// CLI, provider or transport failed.
    Failed,
    /// Explicit cancellation ended the invocation.
    Cancelled,
    /// Controller/VM/configuration/account changed; never replay this invocation.
    Interrupted,
}
impl CodeAgentRunPhase {
    /// Whether a worker may still own the invocation.
    pub fn active(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Starting | Self::Running | Self::Cancelling
        )
    }
}
/// Bounded, sanitized operator-only execution observation. No credential or prompt is stored here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAgentRun {
    /// Client-generated, idempotent invocation identity.
    pub id: Uuid,
    /// Owning saved workspace.
    pub workspace_id: Uuid,
    /// Captured configuration revision.
    pub revision: u64,
    /// Assigned account identity.
    pub connection_id: Uuid,
    /// Selected CLI agent.
    pub agent: CodeAgent,
    /// Verified authentication recipe.
    pub adapter_id: String,
    /// Exact supported CLI version.
    pub cli_version: String,
    /// Requested model, or first model observed when using the agent default.
    pub model: String,
    /// Actual accepted model IDs, including agent helper calls.
    #[serde(default)]
    pub models_used: Vec<String>,
    /// Captured VM.
    pub vm_id: Uuid,
    /// Captured VM incarnation.
    pub runtime_id: Uuid,
    /// Captured lifecycle operation.
    pub operation_id: Uuid,
    /// Current observation.
    pub phase: CodeAgentRunPhase,
    /// Bounded textual CLI output after credential redaction.
    pub output: String,
    /// Whether output exceeded its capture limit.
    pub truncated: bool,
    /// Number of provider requests accepted by the execution broker.
    pub provider_requests: u32,
    /// Confirmed CLI exit, when observed.
    pub exit_code: Option<i32>,
    /// Stable failure category; never an upstream body.
    pub error: Option<String>,
    /// Admission timestamp.
    pub created_at_ms: i64,
    /// Last observation timestamp.
    pub updated_at_ms: i64,
}

/// Durable cancellation also fences a delayed, not-yet-admitted request ID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAgentRunCancellation {
    /// Retired invocation ID.
    pub id: Uuid,
    /// Owning workspace.
    pub workspace_id: Uuid,
    /// Existing observation, if the start request was admitted.
    pub run: Option<CodeAgentRun>,
}

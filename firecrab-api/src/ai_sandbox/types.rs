//! Private durable data; provider credentials are never stored in task records.
use firecrab_api_types::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
/// Private target identity prevents retries or cancellation from reaching another VM boot.
pub type GuestBinding = SandboxRuntimeRef;
/// Current model permission proof, invalidated when the authenticated account/key changes.
#[derive(Clone, Deserialize, Serialize)]
pub struct ModelProof {
    /// Connection and model verified together.
    pub selection: AiModelSelection,
    /// SHA256 account binding, never returned to the browser.
    pub fingerprint: String,
    /// Complete text permission probe succeeded.
    pub verified: bool,
    /// Native function-call round trip succeeded.
    pub tools_verified: bool,
}

/// Public task plus private filesystem and guest reconciliation state.
#[derive(Clone, Deserialize, Serialize)]
pub struct SavedTask {
    /// Authenticated admission identity; policies are re-read before subsequent effects.
    #[serde(default)]
    pub caller: super::access::Caller,
    /// Shared public task, used by conversation, board, CLI and MCP.
    pub task: SandboxTask,
    /// Exact project settings captured at admission.
    pub project: SandboxProjectSpec,
    /// Owned attempt directory, independent of the original source.
    pub workspace: Option<PathBuf>,
    /// Borrowed VM boot identity captured before any guest side effect.
    pub guest: Option<GuestBinding>,
    /// A guest command, retained file write, or unexported workspace remains uncertain.
    pub guest_pending: bool,
    /// Immutable identity of the most recently admitted guest command.
    #[serde(default)]
    pub guest_command: Option<uuid::Uuid>,
    /// A known attempt-owned guest workspace exists and must be cleaned after export.
    #[serde(default)]
    pub guest_staged: bool,
    /// Required outputs were persisted and exported before temporary Environment cleanup.
    #[serde(default)]
    pub results_exported: bool,
    /// Environment-owned execution isolation fixed at admission; absent for borrowed VMs.
    #[serde(default)]
    pub execution_mode: Option<SandboxExecutionMode>,
    /// Immutable saved rules and their retained project binding, fixed before dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<SavedInstructions>,
    /// Immutable selected skill guidance, retained by the existing canonical task record.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<crate::project_workspace::skills::tasks::SavedSkill>,
}

/// Saved instruction provenance; it contains no model account configuration.
#[derive(Clone, Deserialize, Serialize)]
pub struct SavedInstructions {
    /// Private project database selected at admission.
    pub database_id: uuid::Uuid,
    /// Retained project volume selected at admission.
    pub volume_id: uuid::Uuid,
    /// Reject a later runtime handoff instead of silently reusing context.
    pub generation: u64,
    /// Exact ordered saved-file versions; private editor drafts are never included.
    pub snapshot: ProjectInstructionSnapshot,
}

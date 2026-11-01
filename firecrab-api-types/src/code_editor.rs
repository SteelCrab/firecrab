//! Browser editor preparation is bound to the saved workspace and live runtime.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Prepare the fixed Web VS Code service; no commands, paths or credentials are accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodeEditorPrepare {
    /// Current workspace configuration revision.
    pub expected_revision: u64,
    /// Server-reserved workspace VM.
    pub vm_id: Uuid,
    /// Observed running VM instance.
    pub runtime_id: Uuid,
    /// Observed completed runtime operation.
    pub operation_id: Uuid,
}
/// Ephemeral controller session state; a restart requires reopening the retained editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeEditorPhase {
    /// Pinned editor installation and SSH transport setup in progress.
    Preparing,
    /// Editor and terminal may be opened through authenticated, isolated origins.
    Ready,
    /// Preparation or the editor transport failed; explicit retry is available.
    Failed,
}
/// Metadata only. Authentication cookies and one-use handoff tickets are never returned here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeEditorStatus {
    /// Saved workspace identity.
    pub workspace_id: Uuid,
    /// Current configuration revision.
    pub revision: u64,
    /// Workspace VM identity.
    pub vm_id: Uuid,
    /// Live runtime instance.
    pub runtime_id: Uuid,
    /// Runtime operation captured at preparation.
    pub operation_id: Uuid,
    /// Current controller observation.
    pub phase: CodeEditorPhase,
    /// Pinned code-server version.
    pub version: String,
}

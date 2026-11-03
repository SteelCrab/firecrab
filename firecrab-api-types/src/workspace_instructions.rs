//! Immutable saved AGENTS.md context for a project request.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Capture saved rules for a workspace-relative directory; private editor drafts are excluded.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureProjectInstructions {
    /// Valid project directory, or empty for the project root.
    #[serde(default)]
    pub directory: String,
}
/// Exact saved text and its retained file-history identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInstructionFile {
    /// Root or ancestor-relative AGENTS.md path.
    pub path: String,
    /// Nonzero project file-history version.
    pub version: u64,
    /// SHA-256 of the captured UTF-8 bytes.
    pub sha256: String,
    /// Exact saved text, never an editor's unsaved buffer.
    pub content: String,
}
/// Caller-private immutable context retained in the project database.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInstructionSnapshot {
    /// Stable request UUID; reusing it returns the original context.
    pub id: Uuid,
    /// Directory whose root-to-leaf instruction chain was captured.
    pub directory: String,
    /// Snapshot commit time as Unix seconds.
    pub created_at: u64,
    /// SHA-256 of the directory and ordered file records.
    pub sha256: String,
    /// Existing saved rules in root-to-leaf order; absent files are omitted.
    pub files: Vec<ProjectInstructionFile>,
}

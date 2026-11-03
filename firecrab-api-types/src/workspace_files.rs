//! Project files share one retained-volume source of truth across clients.
use crate::SandboxPrincipal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One immediate child in an authorized workspace directory.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFileEntry {
    /// UTF-8 component, without a directory prefix.
    pub name: String,
    /// Whether this entry is an ordinary directory.
    pub directory: bool,
}

/// One retained file observation or accepted save.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFileVersion {
    /// Monotonic version identity within this project.
    pub id: u64,
    /// Workspace-relative path.
    pub path: String,
    /// SHA-256 of the exact file bytes.
    pub sha256: String,
    /// Length of the original bytes.
    pub byte_length: u64,
    /// Verified writer, absent for changes observed from the guest filesystem.
    pub author: Option<SandboxPrincipal>,
    /// Source history record when a restore created this new version.
    pub restored_from: Option<u64>,
    /// Unix timestamp in seconds.
    pub created_at: u64,
}

/// Bounded file contents and their immutable version metadata.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFile {
    /// Immutable version details.
    pub version: ProjectFileVersion,
    /// Base64 of the original bytes, including binary files.
    pub content_base64: String,
}

/// A save's stable identity survives a lost response or controller restart.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveProjectFile {
    /// Reuse this identity only for the identical save.
    pub operation_id: Uuid,
    /// Observed file version, or zero when the file is absent.
    pub expected_version: u64,
    /// Base64 file bytes; decoded size is limited to 1 MiB.
    pub content_base64: String,
}

/// Restore creates a new version instead of replacing existing history.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestoreProjectFile {
    /// Stable identity for this restore operation.
    pub operation_id: Uuid,
    /// Observed current file version, or zero when absent.
    pub expected_version: u64,
    /// Historical version of the same path to restore.
    pub version: u64,
}

/// Two immutable versions for a text or binary comparison.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFileDiff {
    /// Earlier selected file contents.
    pub before: ProjectFile,
    /// Later selected file contents.
    pub after: ProjectFile,
}

/// A pending receipt must be reconciled rather than dispatched as a new save.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectFileOperationStatus {
    /// The durable request has no confirmed guest receipt yet.
    Pending,
    /// The exact guest write and its history version were recorded.
    Applied,
    /// The guest confirmed rejection before changing file contents.
    Failed,
}

/// Public operation state excludes pending file bytes and private host paths.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFileOperation {
    /// Client-selected stable identity.
    pub id: Uuid,
    /// Workspace-relative path.
    pub path: String,
    /// Requested prior file version, or zero for absence.
    pub expected_version: u64,
    /// Verified writer captured before dispatch.
    pub author: SandboxPrincipal,
    /// Confirmed resulting history version, when available.
    pub version: Option<u64>,
    /// Durable outcome of this request.
    pub status: ProjectFileOperationStatus,
}

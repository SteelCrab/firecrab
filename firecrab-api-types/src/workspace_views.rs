//! Private canvas views and drafts are scoped to the verified caller and a stable tab UUID.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A private view contains only the caller's layout, open files and editor preferences.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectView {
    /// Stable, private canvas tab UUID.
    pub id: Uuid,
    /// Monotonic resource revision used for optimistic saves.
    pub revision: u64,
    /// Private layout and open-file metadata, limited to 64 KiB.
    pub document: serde_json::Value,
    /// Last accepted save time as Unix seconds.
    pub updated_at: u64,
}
/// A lightweight list does not return every saved document or buffer.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectViewSummary {
    /// Stable, private canvas tab UUID.
    pub id: Uuid,
    /// Monotonic resource revision used for optimistic saves.
    pub revision: u64,
    /// Last accepted save time as Unix seconds.
    pub updated_at: u64,
}
/// Stable operation identities allow retrying a lost response without overwriting newer work.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveProjectView {
    /// Stable UUID reused only for an identical logical mutation.
    pub operation_id: Uuid,
    /// Exact prior revision, or zero when no resource exists.
    pub expected_revision: u64,
    /// Private layout and open-file metadata, limited to 64 KiB.
    pub document: serde_json::Value,
}
/// An unsaved buffer retains the saved file version it was based on.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDraft {
    /// Authorized workspace-relative file path.
    pub path: String,
    /// Monotonic resource revision used for optimistic saves.
    pub revision: u64,
    /// Saved file version underlying this buffer, or zero for a new file.
    pub base_version: u64,
    /// None denotes a discarded draft; its revision still fences stale saves.
    pub content_base64: Option<String>,
}
/// Active draft metadata without returning its private contents.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDraftSummary {
    /// Authorized workspace-relative file path.
    pub path: String,
    /// Monotonic resource revision used for optimistic saves.
    pub revision: u64,
    /// Saved file version underlying this buffer, or zero for a new file.
    pub base_version: u64,
    /// Number of original draft bytes before base64 encoding.
    pub byte_length: u64,
}
/// An optimistic private buffer save, separate from a shared file save.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveProjectDraft {
    /// Stable UUID reused only for an identical logical mutation.
    pub operation_id: Uuid,
    /// Exact prior revision, or zero when no resource exists.
    pub expected_revision: u64,
    /// Saved file version underlying this buffer, or zero for a new file.
    pub base_version: u64,
    /// Exact unsaved file bytes encoded as base64.
    pub content_base64: String,
}
/// Discard private contents while retaining a revision that rejects stale retries.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteProjectDraft {
    /// Stable UUID reused only for an identical logical mutation.
    pub operation_id: Uuid,
    /// Exact prior revision, or zero when no resource exists.
    pub expected_revision: u64,
}
/// This is the acknowledged operation revision, not a claim about later concurrent edits.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPrivateSave {
    /// Monotonic resource revision used for optimistic saves.
    pub revision: u64,
}

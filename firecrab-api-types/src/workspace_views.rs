//! Private canvas views and drafts are scoped to the verified caller and a stable tab UUID.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A private view contains only the caller's layout, open files and editor preferences.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectView {
    pub id: Uuid,
    pub revision: u64,
    pub document: serde_json::Value,
    pub updated_at: u64,
}
/// A lightweight list does not return every saved document or buffer.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectViewSummary {
    pub id: Uuid,
    pub revision: u64,
    pub updated_at: u64,
}
/// Stable operation identities allow retrying a lost response without overwriting newer work.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveProjectView {
    pub operation_id: Uuid,
    pub expected_revision: u64,
    pub document: serde_json::Value,
}
/// An unsaved buffer retains the saved file version it was based on.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDraft {
    pub path: String,
    pub revision: u64,
    pub base_version: u64,
    /// None denotes a discarded draft; its revision still fences stale saves.
    pub content_base64: Option<String>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDraftSummary {
    pub path: String,
    pub revision: u64,
    pub base_version: u64,
    pub byte_length: u64,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveProjectDraft {
    pub operation_id: Uuid,
    pub expected_revision: u64,
    pub base_version: u64,
    pub content_base64: String,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteProjectDraft {
    pub operation_id: Uuid,
    pub expected_revision: u64,
}
/// This is the acknowledged operation revision, not a claim about later concurrent edits.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPrivateSave {
    pub revision: u64,
}

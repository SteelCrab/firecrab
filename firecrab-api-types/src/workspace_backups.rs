//! Operator-controlled matched project backups and recoverable offline restoration.
use crate::ProjectWorkspaceCheckpoint;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Explicit opt-in scheduling; running workloads are deferred rather than stopped.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveProjectBackupPolicy {
    /// Current binding generation acknowledged by the operator.
    pub expected_generation: u64,
    /// Whether the service should create backups while this workload is stopped.
    pub enabled: bool,
    /// Period in seconds, from five minutes through seven days.
    pub interval_seconds: u64,
}
/// Durable scheduler state is separate from the project database being restored.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectBackupPolicy {
    /// Explicit opt-in configuration.
    pub enabled: bool,
    /// Requested period between successful checkpoints.
    pub interval_seconds: u64,
    /// Earliest next attempt in Unix seconds.
    pub next_run_at: u64,
    /// Most recent completed scheduled checkpoint.
    pub last_checkpoint: Option<Uuid>,
    /// Deferred or failed attempt; existing backups are retained.
    pub error: Option<String>,
}
/// One immutable operator intent, reused after an uncertain HTTP response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectBackupRequest {
    /// Expected current resource binding.
    pub expected_generation: u64,
    /// Omit to create a backup; select a completed checkpoint to restore it.
    pub restore_checkpoint: Option<Uuid>,
}
/// Matched bytes and a separately retained control-database snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectBackupManifest {
    /// Completed database and volume integrity proof.
    pub checkpoint: ProjectWorkspaceCheckpoint,
    /// Exact data-volume byte length; restoration does not resize volumes.
    pub volume_bytes: u64,
    /// Hash of the separate private control DB copy, never a downloadable API payload.
    pub control_database_sha256: String,
}
/// Public operation progress; host paths and backup contents stay private.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectBackupOperation {
    /// Stable operation UUID.
    pub id: Uuid,
    /// Existing project owner.
    pub project_id: Uuid,
    /// Immutable requested action and generation.
    pub request: ProjectBackupRequest,
    /// Pending, database_frozen, prepared, installing, complete or aborted.
    pub phase: String,
    /// Data is committed but maintenance cleanup may still need an exact retry.
    #[serde(default)]
    pub pending_reconciliation: bool,
    /// Unix admission time.
    pub created_at: u64,
    /// Most recent durable progress time.
    pub updated_at: u64,
    /// Backup metadata when available; provisional until the operation is complete.
    pub manifest: Option<ProjectBackupManifest>,
    /// Binding generation after successful restoration.
    pub restored_generation: Option<u64>,
    /// Human-readable recovery requirement without host paths or secrets.
    pub error: Option<String>,
}

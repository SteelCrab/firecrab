//! Retention policies and immutable, recoverable cleanup intents.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Explicit opt-in deletion policy; newly admitted cleanup finishes even after disabling it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveProjectBackupRetention {
    /// Current project binding acknowledged by the operator.
    pub expected_generation: u64,
    /// Whether maintenance may admit automatic cleanup.
    pub enabled: bool,
    /// Minimum recent checkpoints retained, from one through 63; one catalog slot is reserved for rotation.
    pub keep_latest: u32,
    /// Only delete backups older than this many days; null applies the count alone.
    pub max_age_days: Option<u32>,
    /// Permit automatic cleanup of manual backups in addition to scheduled backups.
    pub include_manual: bool,
}
/// Durable retention configuration and scheduler result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectBackupRetention {
    /// Automatic cleanup is disabled by default.
    pub enabled: bool,
    /// Minimum number of recent checkpoints to retain.
    pub keep_latest: u32,
    /// Optional minimum age for automatically selected checkpoints.
    pub max_age_days: Option<u32>,
    /// Manual backups are excluded by default.
    pub include_manual: bool,
    /// Earliest next automatic attempt, in Unix seconds.
    pub next_run_at: u64,
    /// Most recent completed cleanup time.
    pub last_cleanup_at: Option<u64>,
    /// Sanitized deferral or failure status.
    pub error: Option<String>,
}
impl Default for ProjectBackupRetention {
    fn default() -> Self {
        Self {
            enabled: false,
            keep_latest: 10,
            max_age_days: None,
            include_manual: false,
            next_run_at: 0,
            last_cleanup_at: None,
            error: None,
        }
    }
}
/// Protection changes require an explicit current-generation acknowledgement.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveProjectBackupProtection {
    /// Current binding generation.
    pub expected_generation: u64,
    /// Protected checkpoints cannot be deleted manually or automatically.
    pub protected: bool,
}
/// Exact artifacts selected by their retained backup/restore operation identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectBackupCleanupRequest {
    /// Current binding generation acknowledged before admission.
    pub expected_generation: u64,
    /// One through 64 backup, aborted-operation or completed-restore IDs.
    pub target_ids: Vec<Uuid>,
}
/// Non-authoritative preview; admission rechecks permissions, dependencies and a valid survivor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectBackupCleanupCandidate {
    /// Retained backup/restore operation ID.
    pub id: Uuid,
    /// Checkpoint, partial or restore_originals.
    pub kind: String,
    /// Original operation admission time.
    pub created_at: u64,
    /// Whether explicit cleanup can be requested; admission still validates the entire batch.
    pub eligible: bool,
    /// Checkpoint is protected from deletion.
    pub protected: bool,
    /// Explanation when this candidate cannot be selected.
    pub reason: Option<String>,
}
/// Retention preview never deletes, tombstones or admits an operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectBackupCleanupPreview {
    /// Artifacts represented by retained operation receipts.
    pub candidates: Vec<ProjectBackupCleanupCandidate>,
    /// Checkpoints selected by current age/count policy, independent of its enabled switch.
    pub automatic_target_ids: Vec<Uuid>,
}
/// Cleanup progress contains no server paths or private file bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectBackupCleanupOperation {
    /// Stable cleanup identity reused after uncertain responses.
    pub id: Uuid,
    /// Authorized project owner.
    pub project_id: Uuid,
    /// Immutable admitted selection.
    pub request: ProjectBackupCleanupRequest,
    /// Deleting or complete; complete can still require maintenance-gate cleanup.
    pub phase: String,
    /// The operation still holds its maintenance gate.
    pub pending_reconciliation: bool,
    /// Whether this selection came from an explicitly enabled policy.
    pub automatic: bool,
    /// Unix admission time.
    pub created_at: u64,
    /// Most recent durable progress time.
    pub updated_at: u64,
    /// Logical bytes removed, not physical sparse-disk allocation.
    pub logical_bytes: u64,
    /// Number of reconciled file removals.
    pub removed_files: u32,
    /// Sanitized recovery requirement; retry retains the exact plan.
    pub error: Option<String>,
}

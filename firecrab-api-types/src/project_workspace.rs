//! Project-owned durable workspace bindings. Private database paths are never transport fields.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Bind the existing project's selected VM to one retained volume.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindProjectWorkspace {
    /// Completed MicroBlockStorage volume retained for this project.
    pub volume_id: Uuid,
    /// Required when changing an existing runtime binding.
    pub expected_generation: Option<u64>,
}

/// Optimistic update of bounded project metadata; source files use their own volume.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateProjectWorkspace {
    /// Revision observed before editing; stale saves return a conflict.
    pub expected_revision: u64,
    /// Project metadata object, limited to 64 KiB when serialized.
    pub document: serde_json::Value,
}

/// Readiness is observed against the current runtime, never inferred from a prior boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectWorkspaceStatus {
    /// Resource reservation exists but provisioning has not completed.
    Provisioning,
    /// Retained storage is available while the workload VM is inactive.
    Stopped,
    /// The current runtime has not supplied an accepted mount proof.
    AwaitingMount,
    /// The current runtime supplied the expected mount and initialization proof.
    Ready,
    /// The registered runtime or retained volume is missing or mismatched.
    Unavailable,
}

/// Authorized project metadata and observed runtime readiness, without host filesystem paths.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWorkspaceResponse {
    /// Existing Sandbox project identity.
    pub project_id: Uuid,
    /// Stable private database identity, retained across runtime replacement.
    pub database_id: Uuid,
    /// Stable retained data-volume identity.
    pub volume_id: Uuid,
    /// Explicitly selected workload VM.
    pub vm_id: Uuid,
    /// Monotonic runtime binding generation used to reject obsolete handoffs.
    pub generation: u64,
    /// Private SQLite schema version.
    pub schema_version: u32,
    /// Current optimistic metadata revision.
    pub revision: u64,
    /// Bounded project metadata object.
    pub document: serde_json::Value,
    /// Whether the initial guest import has been confirmed.
    pub initialized: bool,
    /// Current readiness observation.
    pub status: ProjectWorkspaceStatus,
    /// Boot-specific runtime identity associated with the last accepted proof.
    pub verified_runtime_id: Option<Uuid>,
}

/// A completed, matched database/disk checkpoint. Paths are server-owned implementation details.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWorkspaceCheckpoint {
    /// Caller-selected idempotency identity for this checkpoint.
    pub id: Uuid,
    /// Project whose database and disk were quiesced together.
    pub project_id: Uuid,
    /// Private database identity captured in the backup.
    pub database_id: Uuid,
    /// Retained volume identity captured in the backup.
    pub volume_id: Uuid,
    /// Binding generation at checkpoint creation.
    pub generation: u64,
    /// Database schema version at checkpoint creation.
    pub schema_version: u32,
    /// Project metadata revision at checkpoint creation.
    pub revision: u64,
    /// SHA256 digest of the SQLite backup.
    pub database_sha256: String,
    /// SHA256 digest of the full retained disk copy.
    pub volume_sha256: String,
    /// Completion timestamp in Unix seconds.
    pub created_at: u64,
}

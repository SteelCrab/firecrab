//! CODE owns a dedicated VM; its working files survive stop and resume.
use crate::{SandboxTemplatePin, VmState};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Placement selected once when a CODE runtime is provisioned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodeProvisionRequest {
    /// Current saved configuration revision.
    pub expected_revision: u64,
    /// Installed template alias; the server pins its exact content identity.
    pub template: String,
    /// Existing internet-enabled MicroNetwork used by the development VM.
    pub micro_network_id: Uuid,
    /// Registered storage root ID; omission selects the existing default.
    pub storage_root: Option<String>,
}

/// A lifecycle request cannot supply VM IDs, paths, credentials or commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodeRuntimeRequest {
    /// Current saved configuration revision.
    pub expected_revision: u64,
}

/// Resolved placement retained across API restarts and image-alias changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRuntimePlacement {
    /// Server-resolved immutable template identity.
    pub template: SandboxTemplatePin,
    /// Existing network; CODE does not own or delete it.
    pub micro_network_id: Uuid,
    /// Existing storage root; CODE does not own or delete it.
    pub storage_root: String,
}

/// Requested operation, recovered from the journal after an API restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeRuntimeAction {
    /// Create if not yet created, boot, and initialize or resume working files.
    Start,
    /// Confirm guest filesystem sync, then stop without deleting disks.
    Stop,
}

/// Durable lifecycle phase. Live VM observations remain separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeRuntimePhase {
    /// An operation is durably reserved and waiting for a worker.
    Queued,
    /// The reserved VM is being created or booted.
    Provisioning,
    /// Guest runtime tools and repository files are being prepared.
    Preparing,
    /// Guest preparation completed for the recorded runtime instance.
    Ready,
    /// Guest sync and stop are in progress.
    Stopping,
    /// The VM is stopped; working files and disks remain.
    Stopped,
    /// An operation failed; resources are retained for an explicit retry.
    Failed,
    /// VM/runtime observations differ; start again to reconcile retained files.
    ReconcileRequired,
}

impl CodeRuntimePhase {
    /// Only unfinished operations are replayed by maintenance.
    pub fn pending(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Provisioning | Self::Preparing | Self::Stopping
        )
    }
}

/// Stable public errors contain no guest output, provider bodies or credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeRuntimeFailure {
    /// The reserved VM no longer matches its recorded ownership or image.
    OwnershipChanged,
    /// Existing VM APIs rejected creation.
    CreationFailed,
    /// Existing VM APIs could not boot the guest.
    StartupFailed,
    /// GitHub identity or exact branch read verification failed.
    RepositoryDenied,
    /// The selected account changed during a repository operation.
    AccountChanged,
    /// Pinned SSH guest transport was unavailable or changed during preparation.
    GuestUnavailable,
    /// Guest tools, repository transfer, or checkout validation failed.
    PreparationFailed,
    /// Guest filesystem sync was not confirmed; the running VM is retained.
    SyncFailed,
    /// Existing VM APIs could not confirm stop.
    StopFailed,
}

/// Pollable runtime state; readiness requires a matching live VM runtime ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeWorkspaceRuntime {
    /// Existing CODE workspace UUID.
    pub workspace_id: Uuid,
    /// Server-reserved VM UUID, reused on retries.
    pub vm_id: Uuid,
    /// Configuration revision captured by the last accepted lifecycle request.
    pub configuration_revision: u64,
    /// Durable operation UUID; concurrent retries reuse the active operation.
    pub operation_id: Uuid,
    /// Last requested lifecycle action.
    pub action: CodeRuntimeAction,
    /// Current phase, adjusted when live VM observations invalidate readiness.
    pub phase: CodeRuntimePhase,
    /// Stable placement; image-alias updates cannot silently replace it.
    pub placement: CodeRuntimePlacement,
    /// Fixed guest working folder, owned by an unprivileged development user.
    pub project_path: String,
    /// Last guest runtime instance whose working folder was confirmed.
    pub runtime_id: Option<Uuid>,
    /// Commit initially cloned; later edits and local commits are preserved.
    pub initial_commit: Option<String>,
    /// Fresh VM lifecycle observation, absent when the VM does not exist.
    pub vm_state: Option<VmState>,
    /// Stable failure reason; no command output is exposed.
    pub error: Option<CodeRuntimeFailure>,
}

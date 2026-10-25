//! Native apply requests and redacted durable progress. Manifests stay out of status records.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{Change, GitRevision, PlanInputs, Stack};

/// Apply one reviewed observation with an operator-known retry identity.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ApplyRequest {
    /// Validated desired configuration. Never persisted in the operation journal.
    pub stack: Stack,
    /// Fingerprint returned by the plan for this exact declaration and host state.
    pub expected_fingerprint: String,
    /// Reuse this UUID only to retrieve the same desired operation.
    pub operation_id: Uuid,
    /// Permit stopping owned VMs and replacing a service's disposable replicas.
    #[serde(default)]
    pub allow_stop: bool,
}

/// Durable operation lifecycle. Server restarts interrupt unfinished operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyState {
    /// Accepted, with all prospective ownership UUIDs reserved.
    Queued,
    /// Executing resource changes in dependency order.
    Running,
    /// Every requested step completed.
    Succeeded,
    /// One step failed; completed resources remain explicitly owned.
    Failed,
    /// The API restarted before a terminal result was recorded.
    Interrupted,
}

impl ApplyState {
    /// Stable database and CLI spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
    /// Whether polling can stop without assuming successful completion.
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

/// Per-resource progress, without configuration values or backend error details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    /// No mutation for this step has started.
    Pending,
    /// This exact UUID may have been mutated; inspect it after interruption.
    Running,
    /// The requested step completed.
    Succeeded,
    /// This step failed; its reserved UUID remains owned.
    Failed,
    /// An earlier failure prevented this step.
    Skipped,
}

impl StepState {
    /// Stable CLI and JSON spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

/// Redacted execution record for an exact resource UUID.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyStep {
    /// Original action and exact UUID, reserved before creation starts.
    #[serde(flatten)]
    pub change: Change,
    /// Current execution phase.
    pub state: StepState,
}

/// Static error category and schema field names; never backend messages or values.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyFailure {
    /// Stable machine-readable category.
    pub code: String,
    /// Static operator guidance.
    pub message: String,
    /// Relevant field names without submitted values.
    pub fields: Vec<String>,
}

/// Public, durable operation status; safe to read after a lost acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyStatus {
    /// Exact template and service content identities accepted with this operation.
    #[serde(default)]
    pub inputs: PlanInputs,
    /// Operator-supplied retry identity.
    pub operation_id: Uuid,
    /// Exact registration identity; old operations cannot target a later registration.
    pub stack_id: Uuid,
    /// Registration name.
    pub name: String,
    /// Overall execution state.
    pub state: ApplyState,
    /// Dependency-ordered steps and their exact resource UUIDs.
    pub changes: Vec<ApplyStep>,
    /// Static failure information, if any.
    pub error: Option<ApplyFailure>,
    /// Immutable Git provenance for source-driven applies, retained with the operation journal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_revision: Option<GitRevision>,
}

/// Canonical desired identity used only for exact operation-ID retries.
pub fn desired_hash(stack: &Stack) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(stack).expect("stack is JSON"))
    )
}

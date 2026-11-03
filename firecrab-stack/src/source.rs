//! Git source configuration and redacted reconciliation records.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ApplyStatus, Plan};

/// A github.com source. Credentials are selected by reference, never embedded.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitSource {
    /// Canonical `https://github.com/OWNER/REPO` URL without credentials or queries.
    pub repository: String,
    /// Branch, tag, or full commit SHA resolved before reading any tree or blob.
    #[serde(rename = "ref")]
    pub git_ref: String,
    /// Relative path to one regular UTF-8 YAML file; symlinks are rejected.
    pub manifest_path: String,
    /// `github` selects the existing host-local Pipe Auth connection.
    #[serde(default)]
    pub credential_ref: Option<GitCredential>,
    /// Poll period in seconds, from 60 to 86400; defaults to 900.
    #[serde(default = "default_interval")]
    pub poll_interval_seconds: u32,
    /// Disable periodic checks. Explicit checks remain available.
    #[serde(default)]
    pub paused: bool,
    /// Periodic checks only detect drift unless automatic synchronization is selected.
    #[serde(default, skip_serializing_if = "is_detect_only")]
    pub sync_mode: GitSyncMode,
    /// Explicit permission to stop owned workloads during Git synchronization.
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_stop: bool,
}

/// Git-driven mutation is an explicit opt-in; existing sources remain read-only.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitSyncMode {
    /// Observe configuration without applying it.
    #[default]
    DetectOnly,
    /// Apply supported changes on each due poll.
    Automatic,
}

// Keep default source JSON readable by clients predating synchronization support.
fn is_detect_only(mode: &GitSyncMode) -> bool {
    *mode == GitSyncMode::DetectOnly
}

fn is_false(value: &bool) -> bool {
    !value
}

/// Supported host-local credential references.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GitCredential {
    /// Existing GitHub Pipe Auth token. Private sources also need repository read access.
    Github,
}

fn default_interval() -> u32 {
    900
}

impl GitSource {
    /// Reject URL credentials, traversal, ambiguous names, and unbounded polling.
    /// Errors are static and never echo submitted strings.
    pub fn validate(&self) -> Result<(), &'static str> {
        let repo = self
            .repository
            .strip_prefix("https://github.com/")
            .ok_or("repository must be a canonical https://github.com/OWNER/REPO URL")?;
        let parts = repo.split('/').collect::<Vec<_>>();
        if parts.len() != 2
            || parts.iter().any(|p| {
                p.is_empty()
                    || p.len() > 100
                    || matches!(*p, "." | "..")
                    || !p
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            })
            || parts[1].ends_with(".git")
        {
            return Err("repository must be a canonical https://github.com/OWNER/REPO URL");
        }
        if self.git_ref.is_empty()
            || self.git_ref.len() > 255
            || self.git_ref.starts_with(['/', '-'])
            || self.git_ref.ends_with(['/', '.'])
            || self.git_ref.contains("..")
            || self.git_ref.contains("@{")
            || self
                .git_ref
                .split('/')
                .any(|p| p.is_empty() || p.starts_with('.') || p.ends_with(".lock"))
            || !self
                .git_ref
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_/.".contains(&b))
        {
            return Err(
                "ref must be a branch, tag, or full commit SHA without Git revision expressions",
            );
        }
        if self.manifest_path.is_empty()
            || self.manifest_path.len() > 1024
            || self.manifest_path.split('/').count() > 16
            || self
                .manifest_path
                .split('/')
                .any(|p| p.is_empty() || matches!(p, "." | ".."))
            || !self
                .manifest_path
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_/ .".contains(&b))
        {
            return Err(
                "manifestPath must be a relative file path without traversal or URL escapes",
            );
        }
        if !(60..=86400).contains(&self.poll_interval_seconds) {
            return Err("pollIntervalSeconds must be between 60 and 86400");
        }
        Ok(())
    }
}

/// Compare-and-set source update; generation zero creates the first source.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ConfigureGitSource {
    /// Current source generation, including a retained removal generation. Zero only before first configuration.
    pub expected_generation: u64,
    /// Complete next configuration. Updates never merge omitted fields.
    pub source: GitSource,
}

/// Guard manual checks against stale source configuration.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitSourceGeneration {
    /// Exact generation returned by source status.
    pub generation: u64,
}

/// Retryable disconnection of an exact source generation; never destroys bound infrastructure.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DeleteGitSource {
    /// Stable UUID reused after losing the original response.
    pub operation_id: Uuid,
    /// Selected API endpoint identity.
    pub expected_endpoint_id: Uuid,
    /// Exact Stack registration UUID; the URL name alone cannot authorize deletion.
    pub expected_stack_id: Uuid,
    /// Exact source configuration generation to remove.
    pub expected_generation: u64,
}

/// Explicit synchronization, including acknowledgement of a failed operation before recovery.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SyncGitSource {
    /// Exact configuration generation.
    pub generation: u64,
    /// Failed/interrupted operation inspected before requesting a fresh plan and apply.
    #[serde(default)]
    pub recover_operation_id: Option<Uuid>,
}

/// Observation and synchronization lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitCheckState {
    /// Durably accepted before starting network access.
    Checking,
    /// A native apply was atomically linked to this observation.
    Applying,
    /// The linked native apply succeeded. A later check still observes current drift.
    Applied,
    /// The linked apply failed or was interrupted; explicit recovery is required.
    RecoveryRequired,
    /// Declared configuration matches observed configuration.
    InSync,
    /// Supported configuration changes were detected.
    Drifted,
    /// Ownership, missing resources, or unsupported replacement requires review.
    Blocked,
    /// Source acquisition, validation, or planning failed; resources were untouched.
    Failed,
    /// The API restarted before the result was saved. Checking may safely repeat.
    Interrupted,
}

impl GitCheckState {
    /// Active observations prevent source changes and overlapping runs.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Checking | Self::Applying)
    }
}

/// Provenance for desired, attempted and successful configuration; no manifest values.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRevision {
    /// Source configuration used to acquire this revision, containing credential references only.
    pub source: GitSource,
    /// Source configuration generation.
    pub generation: u64,
    /// Resolved immutable Git commit.
    pub commit: String,
    /// Verified regular-file blob.
    pub blob: String,
    /// Hash of the validated declaration.
    pub desired_hash: String,
}

/// A bounded-history check record. Resource values and provider errors are excluded.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCheck {
    /// Durable check identity.
    pub id: Uuid,
    /// Source generation used by this check.
    pub generation: u64,
    /// Exact source configuration for this generation, including only credential references.
    pub source: GitSource,
    /// Time accepted, as Unix seconds.
    pub started_at: i64,
    /// Time completed, as Unix seconds.
    pub finished_at: Option<i64>,
    /// Current check outcome.
    pub state: GitCheckState,
    /// Resolved exact commit, retained even if the file or manifest is invalid.
    pub commit: Option<String>,
    /// Exact regular-file blob from the resolved commit's tree.
    pub blob: Option<String>,
    /// Hash of canonical validated configuration; never the manifest itself.
    pub desired_hash: Option<String>,
    /// Redacted plan against the owned host configuration.
    pub plan: Option<Plan>,
    /// Static failure category, without remote response text or submitted values.
    pub error: Option<String>,
    /// Whether this observation may apply changes. Explicit checks always set false.
    #[serde(default)]
    pub sync: bool,
    /// Acknowledged prior operation, if explicit recovery was requested.
    #[serde(default)]
    pub recover_operation_id: Option<Uuid>,
    /// Durable native apply identity; the apply journal retains exact resource results.
    #[serde(default)]
    pub operation_id: Option<Uuid>,
}

impl GitCheck {
    /// Only a validated manifest with complete provenance identifies a desired revision.
    pub fn revision(&self) -> Option<GitRevision> {
        Some(GitRevision {
            source: self.source.clone(),
            generation: self.generation,
            commit: self.commit.clone()?,
            blob: self.blob.clone()?,
            desired_hash: self.desired_hash.clone()?,
        })
    }
}

/// Durable source metadata and latest observation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitSourceStatus {
    /// Exact stack registration UUID.
    pub stack_id: Uuid,
    /// Stable registered stack name.
    pub name: String,
    /// Monotonic configuration generation.
    pub generation: u64,
    /// Validated source configuration; contains only credential references.
    pub source: GitSource,
    /// Earliest next periodic check, as Unix seconds. Pause suppresses polling.
    pub next_check_at: i64,
    /// Consecutive failed checks for bounded exponential backoff.
    pub failures: u32,
    /// Latest check, including unfinished progress after a lost acknowledgement.
    pub latest: Option<GitCheck>,
    /// Latest validated desired revision, even when its plan is blocked.
    #[serde(default)]
    pub desired_revision: Option<GitRevision>,
    /// Last revision accepted by the native apply journal.
    #[serde(default)]
    pub attempted_revision: Option<GitRevision>,
    /// Last successful apply or synchronization that observed no changes.
    #[serde(default)]
    pub successful_revision: Option<GitRevision>,
    /// Latest linked native operation, including per-resource redacted results.
    #[serde(default)]
    pub last_apply: Option<ApplyStatus>,
    /// Polling cannot mutate until this failed/interrupted operation is acknowledged.
    #[serde(default)]
    pub recovery_operation_id: Option<Uuid>,
}

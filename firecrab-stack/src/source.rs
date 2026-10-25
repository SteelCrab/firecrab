//! Git source configuration and redacted, read-only reconciliation records.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Plan;

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
    /// Current source generation, or zero when no source is configured.
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

/// Read-only check states. None of these states means a deployment succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitCheckState {
    /// Durably accepted before starting network access.
    Checking,
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
}

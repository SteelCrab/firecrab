//! Versioned CLI configuration proof is distinct from running an authenticated agent.
use crate::{AiAuthentication, AiProvider, CodeAgent};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Fixed server-owned authentication recipe. Clients cannot supply commands or versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAgentAuthAdapter {
    /// Versioned recipe identity.
    pub id: String,
    /// Agent family whose CLI consumes this recipe.
    pub agent: CodeAgent,
    /// Official credential provider.
    pub provider: AiProvider,
    /// Shared Pipe AI credential mechanism.
    pub authentication: AiAuthentication,
    /// Exact supported CLI release; unknown versions are rejected.
    pub cli_version: String,
    /// Whether this recipe can be checked. This never asserts workspace authentication.
    pub supported: bool,
}

/// Safe failures contain neither CLI output nor provider responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeAgentAuthFailure {
    /// The selected account is absent.
    Missing,
    /// Provider or method is ineligible.
    Incompatible,
    /// No verified handoff exists for this credential mechanism.
    AdapterUnavailable,
    /// Provider rejected the selected credential.
    AuthenticationRejected,
    /// Shared account approval is incomplete.
    ConsentRequired,
    /// Provider credential check could not complete.
    ProviderUnavailable,
    /// A prepared and unchanged live runtime is required.
    RuntimeUnavailable,
    /// The pinned CLI is absent.
    CliUnavailable,
    /// The CLI installation is writable by an untrusted user.
    CliUntrusted,
    /// Installed CLI version differs from the recipe.
    VersionMismatch,
    /// CLI output did not prove the selected configuration.
    CliRejected,
    /// The bounded check exceeded its deadline.
    TimedOut,
}

/// A disposable probe never grants permission to execute an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CodeAgentAuthResult {
    /// Workspace explicitly disables its agent.
    NotRequired,
    /// Fail closed before enabling this combination.
    Blocked {
        /// Stable recovery category.
        reason: CodeAgentAuthFailure,
    },
    /// Provider accepted the key and the pinned CLI loaded it in an isolated probe.
    ConfigurationVerified {
        /// Server-owned recipe that passed.
        adapter_id: String,
        /// Exact CLI version observed.
        cli_version: String,
        /// Workspace-owned VM checked.
        vm_id: Uuid,
        /// Live VM incarnation, invalid after reboot or stop.
        runtime_id: Uuid,
    },
}

/// Ephemeral observation bound to the saved revision and live guest incarnation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAgentAuthVerification {
    /// Workspace whose saved assignment was checked.
    pub id: Uuid,
    /// Unchanged configuration revision.
    pub revision: u64,
    /// Unix milliseconds when this observation completed.
    pub checked_at_ms: i64,
    /// Configuration proof or a blocking reason, never an execution grant.
    pub result: CodeAgentAuthResult,
}

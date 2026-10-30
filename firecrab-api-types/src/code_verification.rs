//! Point-in-time provider checks, separate from runtime or agent readiness.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Verify an already saved configuration without accepting credentials or endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodeWorkspaceVerify {
    /// Positive configuration revision the operator intends to check.
    pub expected_revision: u64,
}

/// Sanitized check failures. Provider bodies and credentials are never returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeVerificationFailure {
    /// Referenced account has been disconnected.
    Missing,
    /// The saved or remotely authenticated GitHub login differs from the selection.
    IdentityChanged,
    /// Agent, provider, authentication metadata or endpoint do not match.
    Incompatible,
    /// The credential or session is absent, expired or rejected.
    AuthenticationRejected,
    /// The ChatGPT session lacks plan-usage consent.
    ConsentRequired,
    /// Provider denied repository access, including organization SSO restrictions.
    AccessDenied,
    /// Repository or exact branch is absent or hidden from this token.
    NotFound,
    /// Provider rate or quota limit prevents the check.
    RateLimited,
    /// The provider could not be reached or returned a server failure.
    Unavailable,
    /// A bounded provider response does not match the expected contract.
    InvalidResponse,
    /// Check exceeded its deadline.
    TimedOut,
    /// Provider attempted a redirect; credentials are never forwarded.
    Redirected,
}

/// Exact branch read observation. It does not prove a successful Git clone or push.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CodeRepositoryVerification {
    /// The selected account read the selected repository's exact branch reference.
    Verified {
        /// Commit observed at `refs/heads/{branch}`; the branch may advance later.
        commit_sha: String,
    },
    /// The read check did not succeed.
    Failed {
        /// Stable, secret-free failure category.
        reason: CodeVerificationFailure,
    },
}

/// Credential mechanism accepted for the selected agent's provider check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeAuthenticationMethod {
    /// OpenAI API key accepted by the API-key model catalogue.
    OpenaiApiKey,
    /// Consented ChatGPT plan token accepted by its account model catalogue.
    ChatgptPlan,
    /// Anthropic API key accepted by the Anthropic model catalogue.
    AnthropicApiKey,
}

/// Provider-credential observation, never a claim that a CLI agent is logged in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CodeAiVerification {
    /// This workspace does not select an agent.
    NotRequired,
    /// Provider accepted the credential; inference and agent startup remain separate checks.
    CredentialsVerified {
        /// Authentication mechanism used by the provider check.
        method: CodeAuthenticationMethod,
    },
    /// Credentials could not be verified.
    Failed {
        /// Stable failure category with no upstream body or token value.
        reason: CodeVerificationFailure,
    },
}

/// Ephemeral checks bound to an unchanged saved configuration and account snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeWorkspaceVerification {
    /// Existing workspace UUID.
    pub id: Uuid,
    /// Configuration revision that remained current throughout the check.
    pub revision: u64,
    /// Check completion time in Unix milliseconds; not an authorization expiry.
    pub checked_at_ms: i64,
    /// Current GitHub identity and exact branch-read result.
    pub github: CodeRepositoryVerification,
    /// Current provider-credential result, independent of agent execution.
    pub ai: CodeAiVerification,
}

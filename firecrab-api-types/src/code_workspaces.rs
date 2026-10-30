//! CODE configuration references existing accounts; it never carries credentials or runtime proof.
use crate::{AiAuthentication, AiConnection, AiProvider};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;
#[cfg(test)]
#[path = "code_workspaces_tests.rs"]
mod tests;

/// A GitHub account reference and repository identity, not a clone URL or credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodeGithubRepository {
    /// Expected login of the existing `github` Pipe AUTH connection.
    pub account: String,
    /// Repository owner, which may differ from the connected account.
    pub owner: String,
    /// Repository name, without a URL, owner prefix, or `.git` suffix.
    pub repository: String,
    /// Explicit branch-name syntax; remote branch existence is checked separately.
    pub branch: String,
}

/// Desired development environment. Provisioning is a separate operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeRuntime {
    /// JavaScript/TypeScript development environment.
    NodeJs,
    /// Python development environment.
    Python,
    /// Rust development environment.
    Rust,
}

/// Desired guest capacity, using the existing VM API's units and resource bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodeResources {
    /// Number of vCPUs, 1–32.
    pub cpu: u8,
    /// RAM in MiB, a power of two from 128 through 32768.
    pub ram: u32,
    /// Disk capacity in GiB, 1–500; image minimums are checked during provisioning.
    pub disk_gb: u16,
}

/// Requested agent family. Selecting it does not install or authenticate an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeAgent {
    /// OpenAI Codex agent.
    Codex,
    /// Anthropic Claude Code agent.
    ClaudeCode,
    /// OpenCode agent using an official provider API key.
    #[serde(rename = "opencode")]
    OpenCode,
}

impl CodeAgent {
    /// Provider eligibility only. Runtime adapters still require independent verification.
    pub fn provider_compatible(self, connection: &AiConnection) -> bool {
        matches!(
            (self, connection.provider, connection.authentication),
            (
                Self::Codex,
                AiProvider::Openai,
                AiAuthentication::ApiKey | AiAuthentication::Chatgpt
            ) | (
                Self::ClaudeCode | Self::OpenCode,
                AiProvider::Anthropic,
                AiAuthentication::ApiKey
            ) | (Self::OpenCode, AiProvider::Openai, AiAuthentication::ApiKey)
        )
    }
}

/// A reference to the credential lifecycle already owned by Pipe AI AUTH.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodeAgentBinding {
    /// Agent whose provider eligibility is checked against the connection.
    pub agent: CodeAgent,
    /// Existing AI AUTH UUID; credentials are not copied into CODE records.
    pub connection_id: Uuid,
}

/// Configuration-only workspace. VM, filesystem, editor and execution identities are server-owned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodeWorkspaceSpec {
    /// Unique, case-insensitive workspace name, 1–64 ASCII name characters.
    pub name: String,
    /// Existing GitHub account and explicit repository/branch.
    pub github: CodeGithubRepository,
    /// Desired environment, not a VM image or a runtime readiness assertion.
    pub runtime: CodeRuntime,
    /// Desired VM capacity. Saving it allocates no resource.
    pub resources: CodeResources,
    /// `null` explicitly disables the agent and requires no AI AUTH connection.
    pub agent: Option<CodeAgentBinding>,
}

impl CodeWorkspaceSpec {
    /// Validate bounded identifiers without executing Git or contacting account providers.
    pub fn validate(&self) -> BTreeMap<String, String> {
        let mut errors = BTreeMap::new();
        let mut check = |valid: bool, field: &str, message: &str| {
            if !valid {
                errors.insert(field.into(), message.into());
            }
        };
        check(
            name(&self.name, 64),
            "spec.name",
            "use 1–64 ASCII letters, digits, dots, underscores or hyphens",
        );
        for (field, value) in [
            ("spec.github.account", &self.github.account),
            ("spec.github.owner", &self.github.owner),
        ] {
            check(
                !value.is_empty()
                    && value.len() <= 100
                    && value
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-')
                    && !value.starts_with('-')
                    && !value.ends_with('-'),
                field,
                "use a bounded GitHub login without a URL or credential",
            );
        }
        check(
            name(&self.github.repository, 100) && !self.github.repository.ends_with(".git"),
            "spec.github.repository",
            "use a repository name without an owner, URL or .git suffix",
        );
        check(
            branch(&self.github.branch),
            "spec.github.branch",
            "use an explicit valid branch name of at most 255 bytes",
        );
        check(
            (1..=32).contains(&self.resources.cpu),
            "spec.resources.cpu",
            "use 1–32 vCPUs",
        );
        check(
            (128..=32768).contains(&self.resources.ram) && self.resources.ram.is_power_of_two(),
            "spec.resources.ram",
            "use a power of two from 128 through 32768 MiB",
        );
        check(
            (1..=500).contains(&self.resources.disk_gb),
            "spec.resources.diskGb",
            "use 1–500 GiB",
        );
        if let Some(agent) = &self.agent {
            check(
                !agent.connection_id.is_nil(),
                "spec.agent.connectionId",
                "select a non-nil existing AI AUTH UUID",
            );
        }
        errors
    }
}

fn name(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        && value.bytes().any(|c| c.is_ascii_alphanumeric())
}

fn branch(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value != "@"
        && value != "HEAD"
        && !value.starts_with(['-', '/'])
        && !value.ends_with(['.', '/'])
        && !value.contains(['\\', '~', '^', ':', '?', '*', '[', ' '])
        && !value.chars().any(char::is_control)
        && !["..", "@{", "//"].iter().any(|part| value.contains(part))
        && value
            .split('/')
            .all(|part| !part.starts_with('.') && !part.ends_with(".lock"))
}

/// Optimistic write: revision 0 creates; an update must match the current revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodeWorkspaceWrite {
    /// Last revision read by this client; checked atomically in SQLite.
    pub expected_revision: u64,
    /// Entire replacement configuration, never a partial credential document.
    pub spec: CodeWorkspaceSpec,
}

/// Configuration revision required before deleting workspace metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodeWorkspaceDelete {
    /// Positive revision read from the workspace response.
    pub expected_revision: u64,
}

/// Current account-reference observation. No variant claims runtime authentication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeConnectionState {
    /// The account exists and matches; repository access/agent adapter are not verified.
    PresentUnverified,
    /// Referenced account has been disconnected or does not exist.
    Missing,
    /// The single GitHub connection now belongs to another login.
    IdentityChanged,
    /// The AI connection's current provider/authentication is ineligible for this agent.
    Incompatible,
    /// The workspace explicitly disables its AI agent.
    NotRequired,
}

/// Fresh account observations; never persisted as an authoritative readiness flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeWorkspaceConnections {
    /// GitHub account identity observation, independent of repository permissions.
    pub github: CodeConnectionState,
    /// AI AUTH account observation, independent of actual agent authentication.
    pub ai: CodeConnectionState,
}

/// Durable workspace configuration with live, secret-free account-reference observations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeWorkspace {
    /// Client-selected stable UUID. A retry cannot create a second workspace under another ID.
    pub id: Uuid,
    /// Starts at 1 and increases after every accepted replacement.
    pub revision: u64,
    /// Saved desired configuration; contains account references only.
    pub spec: CodeWorkspaceSpec,
    /// Creation time in Unix milliseconds, preserved by updates.
    pub created_at_ms: i64,
    /// Last accepted configuration write in Unix milliseconds.
    pub updated_at_ms: i64,
    /// Current connection observations from the same database snapshot.
    pub connections: CodeWorkspaceConnections,
}

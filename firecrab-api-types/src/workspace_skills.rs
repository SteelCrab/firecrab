//! Official project skill catalogs, immutable source revisions and retained installation receipts.
use serde::{Deserialize, Serialize};
use uuid::Uuid;
/// Fixed official distribution repositories; arbitrary URLs are not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectSkillSource {
    /// Skill components of the official OpenAI plugin marketplace.
    Openai,
    /// Individual skills in Anthropic's official skill marketplace.
    Anthropic,
}
/// Distinguish one skill directory from a plugin's collection of skill directories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectSkillKind {
    /// A single SKILL.md and its companion resources.
    Skill,
    /// A plugin's skill components; connectors, hooks and apps are not activated.
    SkillBundle,
}
/// One entry discovered in an exact official repository commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectSkillEntry {
    /// Official source identity.
    pub source: ProjectSkillSource,
    /// Individual skill or skill-only plugin bundle.
    pub kind: ProjectSkillKind,
    /// Repository-relative directory.
    pub path: String,
    /// Human-readable source label.
    pub name: String,
    /// Exact Git commit, never a mutable branch name.
    pub revision: String,
    /// SKILL.md paths relative to this entry's directory.
    pub instructions: Vec<String>,
}
/// Retained discovery results; refresh is an explicit network operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSkillCatalog {
    /// Official source represented by this catalog.
    pub source: ProjectSkillSource,
    /// Distribution page shown in the UI.
    pub url: String,
    /// Pinned commit, absent before the first successful refresh.
    pub revision: Option<String>,
    /// Entries in the accepted source tree.
    pub entries: Vec<ProjectSkillEntry>,
}
/// Stable request body for an idempotent project installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallProjectSkill {
    /// Select one of the fixed official sources.
    pub source: ProjectSkillSource,
    /// Selected entry's exact repository path.
    pub path: String,
    /// Commit displayed by the accepted project catalog.
    pub revision: String,
    /// The entry's skill or bundle identity.
    pub kind: ProjectSkillKind,
}
/// Installation state persisted independently of any browser window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectSkillStatus {
    /// Some files still require their original write receipts.
    Pending,
    /// All source files have confirmed retained-volume versions.
    Installed,
    /// Disabled for new tasks; historical files and accepted tasks are retained.
    Removed,
}
/// Exact installed source file and its versioned write identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSkillFile {
    /// Path relative to the installation root.
    pub path: String,
    /// Durable file-operation UUID chosen before dispatch.
    pub operation_id: Uuid,
    /// Confirmed file-history version; zero before that file finishes.
    pub version: u64,
    /// Source bytes' SHA-256 digest.
    pub sha256: String,
    /// Source file size.
    pub byte_length: u64,
}
/// Public retained installation metadata; package bytes stay in the project DB/history.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSkillInstallation {
    /// Stable installation operation and revision identity.
    pub id: Uuid,
    /// Exact official entry selected for this operation.
    pub entry: ProjectSkillEntry,
    /// Actual directory in the retained project volume.
    pub root: String,
    /// Pending, confirmed or removed catalog membership.
    pub status: ProjectSkillStatus,
    /// File receipts for reconciliation and provenance.
    pub files: Vec<ProjectSkillFile>,
    /// SHA-256 of the ordered source file paths and digests.
    pub sha256: String,
}
/// Public task provenance for one selected installed revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTaskSkill {
    /// Installation captured by the accepted task.
    pub id: Uuid,
    /// Official source and compatible model adapter.
    pub source: ProjectSkillSource,
    /// Immutable source commit.
    pub revision: String,
    /// Repository-relative source directory.
    pub path: String,
    /// Actual project-volume installation root.
    pub root: String,
    /// Digest of the installed source manifest.
    pub sha256: String,
}

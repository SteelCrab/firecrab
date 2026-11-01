//! Bounded, secret-free choices for CODE's GitHub setup flow.
use serde::{Deserialize, Serialize};

/// One selectable repository, without URLs or credential material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRepositoryChoice {
    /// GitHub user or organization that owns the repository.
    pub owner: String,
    /// Repository name without the owner or `.git` suffix.
    pub repository: String,
    /// GitHub's suggested branch; branch discovery still checks the branch list.
    pub default_branch: String,
    /// Primary language, used only to suggest an editable runtime default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Whether the repository is private.
    pub private: bool,
}

/// One page from the currently authenticated GitHub account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRepositoryPage {
    /// Verified current GitHub login.
    pub account: String,
    /// At most 20 sanitized repositories.
    pub repositories: Vec<CodeRepositoryChoice>,
    /// Next bounded page number, absent at the end of the catalog.
    pub next_page: Option<u32>,
}

/// One page of exact branch names for a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeBranchPage {
    /// Verified current GitHub login.
    pub account: String,
    /// Requested owner.
    pub owner: String,
    /// Requested repository.
    pub repository: String,
    /// At most 20 branch names; no commit bodies or tokens are exposed.
    pub branches: Vec<String>,
    /// Next bounded page number, absent at the end of the catalog.
    pub next_page: Option<u32>,
}

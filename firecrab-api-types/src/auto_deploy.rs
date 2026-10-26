//! Opt-in registry selection and public, credential-free watcher status.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A tagged OCI source, independent of the digest serving the current release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AutoDeployPolicy {
    /// Exact tag or stable version prefix, for example `ghcr.io/team/app:v1.*`.
    pub reference: String,
    /// Registry polling interval, from 60 to 3600 seconds.
    #[serde(default = "default_interval")]
    pub interval_seconds: u32,
}

impl AutoDeployPolicy {
    /// Split an explicitly tagged source; credentials, schemes and digest rules are forbidden.
    pub fn source(&self) -> Option<(&str, &str)> {
        let value = &self.reference;
        if value.len() > 2048
            || value.contains('@')
            || value.contains("://")
            || value
                .bytes()
                .any(|b| !b.is_ascii() || b.is_ascii_whitespace() || b.is_ascii_control())
        {
            return None;
        }
        let (repository, tag) = value.rsplit_once(':')?;
        if repository.is_empty()
            || tag.is_empty()
            || tag.contains('/')
            || repository
                .split('/')
                .any(|part| part.is_empty() || matches!(part, "." | ".."))
        {
            return None;
        }
        Some((repository, tag))
    }

    /// Shared structural validation for REST and YAML; registry ownership is checked by the API.
    pub fn validation_error(&self) -> Option<(&'static str, &'static str)> {
        if !(60..=3600).contains(&self.interval_seconds) {
            return Some(("intervalSeconds", "must be between 60 and 3600 seconds"));
        }
        let invalid = || {
            Some((
                "reference",
                "use an explicitly tagged OCI source; stable version rules are *, v*, 1.*, v1.*, 1.2.* or v1.2.*",
            ))
        };
        let Some((_, tag)) = self.source() else {
            return invalid();
        };
        if tag.len() > 128 {
            return invalid();
        }
        if tag.contains('*') {
            let version = tag.strip_prefix('v').unwrap_or(tag);
            if version == "*" {
                return None;
            }
            let Some(prefix) = version.strip_suffix(".*") else {
                return invalid();
            };
            let parts = prefix.split('.').collect::<Vec<_>>();
            if parts.len() > 2
                || parts.iter().any(|part| {
                    part.is_empty()
                        || (part.len() > 1 && part.starts_with('0'))
                        || !part.bytes().all(|b| b.is_ascii_digit())
                        || part.parse::<u64>().is_err()
                })
            {
                return invalid();
            }
        } else if !tag
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
            || !tag
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
        {
            return invalid();
        }
        None
    }
}

fn default_interval() -> u32 {
    300
}

/// Why an enabled watcher is not submitting another deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoDeployPause {
    /// An operator paused monitoring until an explicit resume.
    Manual,
    /// Rollback suppresses the previously observed digest.
    Rollback,
    /// A failed or cancelled automatic update needs a new digest or explicit resume.
    Failed,
}

/// Watch state contains identifiers and image digests, never registry credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoDeployStatus {
    /// None means registry automation is disabled.
    pub policy: Option<AutoDeployPolicy>,
    /// Current pause reason; rollback/failure can resume on a different digest.
    pub pause: Option<AutoDeployPause>,
    /// Last verified, host-platform image manifest digest.
    pub observed_digest: Option<String>,
    /// The tag selected by the policy's last successful registry read.
    pub observed_tag: Option<String>,
    /// Earliest next metadata check, as Unix seconds.
    pub next_check_at: i64,
    /// Durable automatic operation, including a request awaiting acknowledgement.
    pub operation_id: Option<Uuid>,
    /// Bounded consecutive registry-check failures.
    pub failures: u8,
    /// Static operator guidance; remote response bodies and credentials are excluded.
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_accepts_only_bounded_tag_rules() {
        for tag in [
            "stable",
            "latest",
            "release-1",
            "_test",
            "*",
            "v*",
            "1.*",
            "v1.*",
            "1.2.*",
            "v1.2.*",
        ] {
            let policy: AutoDeployPolicy = serde_json::from_value(
                serde_json::json!({"reference":format!("ghcr.io/team/app:{tag}")}),
            )
            .unwrap();
            assert_eq!(policy.interval_seconds, 300);
            assert_eq!(policy.validation_error(), None, "{tag}");
        }
        for tag in [
            "",
            "-bad",
            ".bad",
            "v01.*",
            "1.2.3.*",
            "1.*.*",
            "v1.0-rc.*",
            "stable*",
            "*test",
            "18446744073709551616.*",
            "00.*",
            "x/y",
            "bad tag",
            "bad@sha256:abc",
            "é",
        ] {
            let policy = AutoDeployPolicy {
                reference: format!("ghcr.io/team/app:{tag}"),
                interval_seconds: 60,
            };
            assert!(policy.validation_error().is_some(), "{tag}");
        }
        for reference in [
            "https://ghcr.io/team/app:stable",
            "ghcr.io//app:stable",
            "ghcr.io/../app:stable",
            "ghcr.io/team/app",
            "ghcr.io/team/app@sha256:abc",
            ":tag",
            "\napp:tag",
        ] {
            assert!(
                AutoDeployPolicy {
                    reference: reference.into(),
                    interval_seconds: 60
                }
                .validation_error()
                .is_some(),
                "{reference}"
            );
        }
        for interval in [0, 59, 3601, u32::MAX] {
            assert_eq!(
                AutoDeployPolicy {
                    reference: "ghcr.io/team/app:stable".into(),
                    interval_seconds: interval
                }
                .validation_error()
                .unwrap()
                .0,
                "intervalSeconds"
            );
        }
        assert!(
            serde_json::from_str::<AutoDeployPolicy>(r#"{"reference":"app:tag","token":"secret"}"#)
                .is_err()
        );
    }
}

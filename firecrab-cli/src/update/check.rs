//! The release check: one unauthenticated `GET` against GitHub's
//! `releases/latest`, and the version comparison that decides whether that tag
//! is actually newer than this build.

use std::time::Duration;

use serde::Deserialize;

use super::UpdateError;

/// Matching `api_client.rs`'s reasoning: a CLI must not hang on a GitHub that
/// stopped answering. Slightly longer than the local API's 3s because this one
/// crosses the internet.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The one field of GitHub's release payload this needs. Every other field is
/// ignored, so a schema addition on GitHub's side cannot break the check.
#[derive(Debug, Deserialize)]
pub struct LatestRelease {
    /// The release's git tag, conventionally `vX.Y.Z`.
    pub tag_name: String,
}

/// `FIRECRAB_RELEASE_API`, else GitHub's `releases/latest` for `repo`.
pub fn release_api_url(repo: &str) -> String {
    std::env::var("FIRECRAB_RELEASE_API")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("https://api.github.com/repos/{repo}/releases/latest"))
}

/// A leading `v` removed, for display and for `UpdateCheckResponse::latest`.
pub fn strip_v(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// `X.Y.Z` as a comparable tuple, tolerating a leading `v` and cutting any
/// `-pre` / `+build` suffix. Deliberately hand-rolled: release tags are always
/// `vX.Y.Z`, so a `semver` dependency would buy nothing.
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let trimmed = text.trim();
    let trimmed = trimmed.strip_prefix('v').unwrap_or(trimmed);
    let core = trimmed.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Whether `latest` is strictly newer than `current`. Tuple comparison, so
/// `0.1.10` correctly beats `0.1.9` (string comparison would not).
pub fn is_newer(latest: (u64, u64, u64), current: (u64, u64, u64)) -> bool {
    latest > current
}

/// Reads `tag_name` from a releases endpoint.
///
/// A `User-Agent` is mandatory — GitHub answers `403` without one. A rate-limit
/// refusal is reported as its own message so the operator sees the reset time
/// rather than a bare `403`.
pub fn fetch_latest_tag(url: &str) -> Result<String, UpdateError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .expect("reqwest client build");
    let response = client
        .get(url)
        .header(
            reqwest::header::USER_AGENT,
            concat!("firecrab/", env!("CARGO_PKG_VERSION")),
        )
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .map_err(|error| UpdateError::Check(format!("unreachable: {error}")))?;

    let status = response.status();
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("unknown")
                .to_owned()
        };
        if header("x-ratelimit-remaining") == "0" {
            return Err(UpdateError::Check(format!(
                "rate limited by GitHub; retry after {}",
                header("x-ratelimit-reset")
            )));
        }
    }
    if !status.is_success() {
        return Err(UpdateError::Check(format!("HTTP {}", status.as_u16())));
    }
    let release = response
        .json::<LatestRelease>()
        .map_err(|error| UpdateError::Check(format!("unreadable release payload: {error}")))?;
    Ok(release.tag_name)
}

#[cfg(test)]
mod tests {
    use super::super::ENV_LOCK;
    use super::*;

    #[test]
    fn parse_version_accepts_v_prefixed_and_bare_tags() {
        assert_eq!(parse_version("v0.1.2"), Some((0, 1, 2)));
        assert_eq!(parse_version("0.1.2"), Some((0, 1, 2)));
        assert_eq!(parse_version(" v1.20.300 "), Some((1, 20, 300)));
        // Pre-release and build metadata are cut before comparison.
        assert_eq!(parse_version("v0.2.0-rc.1"), Some((0, 2, 0)));
        assert_eq!(parse_version("v0.2.0+build7"), Some((0, 2, 0)));
    }

    #[test]
    fn parse_version_rejects_unrecognized_tags() {
        for tag in ["", "v", "nightly", "0.1", "0.1.2.3", "v0.x.2"] {
            assert_eq!(parse_version(tag), None, "{tag} should not parse");
        }
    }

    #[test]
    fn is_newer_compares_component_wise() {
        assert!(is_newer((0, 1, 2), (0, 1, 1)));
        // Lexicographic string comparison would get this one wrong.
        assert!(is_newer((0, 1, 10), (0, 1, 9)));
        assert!(is_newer((0, 2, 0), (0, 1, 99)));
        assert!(is_newer((1, 0, 0), (0, 99, 99)));
        assert!(!is_newer((0, 1, 1), (0, 1, 1)));
        assert!(!is_newer((0, 1, 0), (0, 1, 1)));
    }

    #[test]
    fn strip_v_only_removes_a_leading_v() {
        assert_eq!(strip_v("v0.1.2"), "0.1.2");
        assert_eq!(strip_v("0.1.2"), "0.1.2");
        assert_eq!(strip_v("version-0.1.2"), "ersion-0.1.2");
    }

    #[test]
    fn parse_latest_tag_reads_tag_name_and_ignores_other_fields() {
        let body = r#"{
            "url": "https://api.github.com/repos/SteelCrab/firecrab/releases/1",
            "tag_name": "v0.1.2",
            "name": "firecrab 0.1.2",
            "draft": false,
            "assets": [{"name": "firecrab-host-x86_64-gnu.tar.gz"}]
        }"#;
        let release: LatestRelease = serde_json::from_str(body).expect("deserialize");
        assert_eq!(release.tag_name, "v0.1.2");
    }

    #[test]
    fn check_reports_unreachable_for_a_dead_port() {
        // Port 1 is reserved and never listening, so this fails immediately
        // rather than waiting out the client timeout.
        let error = fetch_latest_tag("http://127.0.0.1:1/releases/latest").unwrap_err();
        assert!(
            matches!(&error, UpdateError::Check(detail) if detail.starts_with("unreachable:")),
            "{error}"
        );
    }

    #[test]
    fn release_api_url_prefers_the_env_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: every test in this crate that touches FIRECRAB_RELEASE_API
        // or FIRECRAB_RELEASE_BASE serializes on ENV_LOCK.
        unsafe { std::env::set_var("FIRECRAB_RELEASE_API", "http://127.0.0.1:1/fake") };
        let overridden = release_api_url("SteelCrab/firecrab");
        unsafe { std::env::remove_var("FIRECRAB_RELEASE_API") };
        let default = release_api_url("SteelCrab/firecrab");

        assert_eq!(overridden, "http://127.0.0.1:1/fake");
        assert_eq!(
            default,
            "https://api.github.com/repos/SteelCrab/firecrab/releases/latest"
        );
    }
}

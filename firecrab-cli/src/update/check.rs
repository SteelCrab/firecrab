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

/// The fields of GitHub's release payload this reads. Every other field is
/// ignored, so a schema addition on GitHub's side cannot break the check.
#[derive(Debug, Deserialize)]
pub struct LatestRelease {
    /// The release's git tag, conventionally `vX.Y.Z`.
    pub tag_name: String,
    /// The release notes as Markdown; `null` or absent when there are none.
    #[serde(default)]
    pub body: Option<String>,
    /// The release's page on GitHub.
    #[serde(default)]
    pub html_url: Option<String>,
}

/// The most characters of release notes the dashboard is sent.
pub const NOTES_LIMIT: usize = 16_000;

/// What the release says changed: the `## Changelog` section that
/// `scripts/write-release-notes.py` writes into every release body, else the
/// whole body, cut to [`NOTES_LIMIT`]. The install instructions and the
/// contributor icons around it say nothing about the update.
pub fn release_notes(body: &str) -> Option<String> {
    let notes = changelog_section(body).unwrap_or(body).trim();
    if notes.is_empty() {
        return None;
    }
    Some(match notes.char_indices().nth(NOTES_LIMIT) {
        Some((cut, _)) => format!("{}…", notes[..cut].trim_end()),
        None => notes.to_owned(),
    })
}

/// The lines after a `## Changelog` heading, up to the next `## ` heading.
fn changelog_section(body: &str) -> Option<&str> {
    let mut start = None;
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        if let Some(title) = line.trim_end().strip_prefix("## ") {
            match start {
                None if title.trim() == "Changelog" => start = Some(offset + line.len()),
                Some(begin) => return Some(&body[begin..offset]),
                None => {}
            }
        }
        offset += line.len();
    }
    start.map(|begin| &body[begin..])
}

/// A release page worth linking to: only `https://` URLs are passed on.
pub fn release_page(url: &str) -> Option<String> {
    url.starts_with("https://").then(|| url.to_owned())
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
pub fn fetch_latest_tag(url: &str) -> Result<String, UpdateError> {
    fetch_latest_release(url).map(|release| release.tag_name)
}

/// Reads the newest release from a releases endpoint.
///
/// A `User-Agent` is mandatory — GitHub answers `403` without one. A rate-limit
/// refusal is reported as its own message so the operator sees the reset time
/// rather than a bare `403`.
pub fn fetch_latest_release(url: &str) -> Result<LatestRelease, UpdateError> {
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
    response
        .json::<LatestRelease>()
        .map_err(|error| UpdateError::Check(format!("unreadable release payload: {error}")))
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
        assert_eq!(release.body, None);
        assert_eq!(release.html_url, None);
    }

    #[test]
    fn the_release_payload_carries_its_notes_and_page() {
        let body = serde_json::json!({
            "tag_name": "v0.3.1",
            "html_url": "https://github.com/SteelCrab/firecrab/releases/tag/v0.3.1",
            "body": "## Changelog\n\n- one",
        })
        .to_string();
        let release: LatestRelease = serde_json::from_str(&body).expect("deserialize");
        assert_eq!(release.body.as_deref(), Some("## Changelog\n\n- one"));
        assert_eq!(
            release.html_url.as_deref(),
            Some("https://github.com/SteelCrab/firecrab/releases/tag/v0.3.1")
        );
        // GitHub sends `null` for a release without notes.
        let empty: LatestRelease =
            serde_json::from_str(r#"{"tag_name":"v0.3.1","body":null}"#).expect("deserialize");
        assert_eq!(empty.body, None);
    }

    const RELEASE_BODY: &str = "[#371](https://github.com/SteelCrab/firecrab/discussions/371)\n\n## Install\n\nOne command.\n\n```sh\n./install.sh --libc gnu    # glibc\n```\n\n## Changelog\n\nSummary line.\n\n### Added\n\n- Thing ([#123]).\n\n[#123]: https://github.com/SteelCrab/firecrab/issues/123\n\n## Contributors\n\n<p>icons</p>\n";

    #[test]
    fn the_notes_are_the_changelog_section_of_the_release_body() {
        let notes = release_notes(RELEASE_BODY).expect("notes");
        assert!(notes.starts_with("Summary line."), "{notes}");
        assert!(notes.contains("### Added"), "{notes}");
        // The link definitions the section's references need stay with it.
        assert!(notes.ends_with("issues/123"), "{notes}");
        assert!(!notes.contains("One command"), "{notes}");
        assert!(!notes.contains("icons"), "{notes}");
    }

    #[test]
    fn a_body_without_a_changelog_heading_is_used_whole() {
        assert_eq!(
            release_notes("Just a note.\n").as_deref(),
            Some("Just a note.")
        );
        // A heading that is not exactly `## Changelog` does not start a section.
        assert_eq!(
            release_notes("## Changelog of nothing\n\ntext").as_deref(),
            Some("## Changelog of nothing\n\ntext")
        );
        // The last section runs to the end.
        assert_eq!(
            release_notes("## Install\n\nx\n\n## Changelog\n\nend").as_deref(),
            Some("end")
        );
    }

    #[test]
    fn empty_notes_are_none() {
        assert_eq!(release_notes(""), None);
        assert_eq!(release_notes("  \n\n"), None);
        assert_eq!(release_notes("## Changelog\n\n## Contributors\n"), None);
    }

    #[test]
    fn long_notes_are_cut_on_a_character_boundary() {
        let long = format!("## Changelog\n\n{}", "한".repeat(NOTES_LIMIT + 50));
        let notes = release_notes(&long).expect("notes");
        assert_eq!(notes.chars().count(), NOTES_LIMIT + 1);
        assert!(notes.ends_with('…'));
        let exact = "a".repeat(NOTES_LIMIT);
        assert_eq!(release_notes(&exact).as_deref(), Some(exact.as_str()));
    }

    #[test]
    fn only_https_release_pages_are_passed_on() {
        assert_eq!(
            release_page("https://github.com/SteelCrab/firecrab/releases/tag/v0.3.1").as_deref(),
            Some("https://github.com/SteelCrab/firecrab/releases/tag/v0.3.1")
        );
        for url in [
            "http://github.com/x",
            "javascript:alert(1)",
            "//github.com/x",
            "",
        ] {
            assert_eq!(release_page(url), None, "{url}");
        }
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

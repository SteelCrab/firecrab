//! Read-only access to the pinned official store on GitHub. No credentials are ever sent.
use super::super::resource::{self, Error};
use super::meta::{self, License};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

pub const REPOSITORY: &str = "anthropics/skills";
const BRANCH: &str = "main";
pub const MAX_FILE: u64 = 2 * 1024 * 1024;
pub const MAX_SKILL: u64 = 10 * 1024 * 1024;
pub const MAX_FILES: usize = 500;
const MAX_TREE: u64 = 8 * 1024 * 1024;
const PREVIEW: usize = 2048;

pub struct Source {
    pub api: String,
    pub raw: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CatalogFile {
    /// Relative to the skill folder.
    pub path: String,
    pub size: u64,
    pub git_sha: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSkill {
    pub name: String,
    pub description: String,
    pub license: License,
    /// `unsupported-files` or `too-large`; such a skill is never imported.
    pub problem: Option<String>,
    pub files: Vec<CatalogFile>,
    pub preview: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub repository: String,
    pub commit: String,
    pub skills: Vec<CatalogSkill>,
}

/// A relative path with plain segments only: no root, `.`/`..`, backslash or control characters.
pub fn valid_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

pub fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub fn utf8_prefix(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[derive(Default)]
struct Grouped {
    files: Vec<CatalogFile>,
    unsupported: bool,
}

impl Source {
    pub fn official() -> Self {
        Self {
            api: "https://api.github.com".into(),
            raw: "https://raw.githubusercontent.com".into(),
        }
    }

    fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, Error> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("firecrab")
            .build()
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let response = client
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .map_err(|_| Error::Invalid("GitHub is unavailable. Try again later.".into()))?;
        let status = response.status().as_u16();
        let exhausted = response
            .headers()
            .get("x-ratelimit-remaining")
            .is_some_and(|v| v == "0");
        if status == 429 || (status == 403 && exhausted) {
            return Err(Error::Invalid(
                "GitHub API rate limit reached. Try again later.".into(),
            ));
        }
        if !(200..300).contains(&status) {
            return Err(Error::Invalid(format!(
                "GitHub request failed with HTTP {status}"
            )));
        }
        let mut body = Vec::new();
        response
            .take(limit + 1)
            .read_to_end(&mut body)
            .map_err(|_| Error::Invalid("GitHub response could not be read".into()))?;
        if body.len() as u64 > limit {
            return Err(Error::Invalid("GitHub response is too large".into()));
        }
        Ok(body)
    }

    pub fn latest_commit(&self) -> Result<String, Error> {
        let body = self.get(
            &format!("{}/repos/{REPOSITORY}/commits/{BRANCH}", self.api),
            1024 * 1024,
        )?;
        let value: Value = serde_json::from_slice(&body)?;
        let sha = value["sha"].as_str().unwrap_or_default();
        if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Invalid("GitHub returned an invalid commit".into()));
        }
        Ok(sha.to_ascii_lowercase())
    }

    pub fn file(&self, commit: &str, path: &str, limit: u64) -> Result<Vec<u8>, Error> {
        self.get(&format!("{}/{REPOSITORY}/{commit}/{path}", self.raw), limit)
    }

    /// The catalog at the current `main` commit, cached on disk per commit.
    pub fn catalog(&self, cache_dir: &Path) -> Result<Catalog, Error> {
        let commit = self.latest_commit()?;
        let cached = cache_dir.join(format!("{commit}.json"));
        if let Some(catalog) = resource::read::<Catalog>(&cached)? {
            return Ok(catalog);
        }
        let body = self.get(
            &format!(
                "{}/repos/{REPOSITORY}/git/trees/{commit}?recursive=1",
                self.api
            ),
            MAX_TREE,
        )?;
        let tree: Value = serde_json::from_slice(&body)?;
        if tree["truncated"] != json!(false) {
            return Err(Error::Invalid(
                "The official skill list is incomplete".into(),
            ));
        }
        let mut grouped: BTreeMap<String, Grouped> = BTreeMap::new();
        for entry in tree["tree"].as_array().into_iter().flatten() {
            let Some(rest) = entry["path"]
                .as_str()
                .and_then(|p| p.strip_prefix("skills/"))
            else {
                continue;
            };
            let Some((name, relative)) = rest.split_once('/') else {
                continue;
            };
            if !valid_name(name) || entry["type"] == "tree" {
                continue;
            }
            let group = grouped.entry(name.to_owned()).or_default();
            let regular = entry["type"] == "blob"
                && matches!(entry["mode"].as_str(), Some("100644" | "100755"));
            if !regular || !valid_relative(relative) {
                group.unsupported = true;
                continue;
            }
            group.files.push(CatalogFile {
                path: relative.to_owned(),
                size: entry["size"].as_u64().unwrap_or(u64::MAX),
                git_sha: entry["sha"].as_str().unwrap_or_default().to_owned(),
            });
        }
        let mut skills = Vec::new();
        for (name, group) in grouped {
            if let Some(skill) = self.skill(&commit, name, group)? {
                skills.push(skill);
            }
        }
        let catalog = Catalog {
            repository: REPOSITORY.into(),
            commit,
            skills,
        };
        resource::write(&cached, &catalog)?;
        Ok(catalog)
    }

    fn skill(
        &self,
        commit: &str,
        name: String,
        group: Grouped,
    ) -> Result<Option<CatalogSkill>, Error> {
        let find = |path: &str| group.files.iter().find(|f| f.path == path);
        let Some(manifest) = find("SKILL.md") else {
            return Ok(None);
        };
        let total = group
            .files
            .iter()
            .map(|f| f.size)
            .fold(0, u64::saturating_add);
        let problem = if group.unsupported {
            Some("unsupported-files")
        } else if group.files.len() > MAX_FILES
            || total > MAX_SKILL
            || group.files.iter().any(|f| f.size > MAX_FILE)
        {
            Some("too-large")
        } else {
            None
        };
        let text = if manifest.size <= MAX_FILE {
            let bytes = self.file(commit, &format!("skills/{name}/SKILL.md"), MAX_FILE)?;
            String::from_utf8_lossy(&bytes).into_owned()
        } else {
            String::new()
        };
        let license_txt = match find("LICENSE.txt") {
            Some(file) if file.size <= MAX_FILE => {
                let bytes = self.file(commit, &format!("skills/{name}/LICENSE.txt"), MAX_FILE)?;
                Some(String::from_utf8_lossy(&bytes).into_owned())
            }
            _ => None,
        };
        let parsed = meta::parse_frontmatter(&text).ok();
        Ok(Some(CatalogSkill {
            description: parsed
                .as_ref()
                .map(|m| m.description.clone())
                .unwrap_or_default(),
            license: meta::classify(
                parsed.as_ref().and_then(|m| m.license.as_deref()),
                license_txt.as_deref(),
            ),
            problem: problem.map(str::to_owned),
            preview: utf8_prefix(&text, PREVIEW).to_owned(),
            files: group.files,
            name,
        }))
    }
}

#[cfg(test)]
pub(crate) mod tests;

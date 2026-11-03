//! Search the skills.sh registry, the same API `npx skills find` uses.
//! Results are community skills: only entries the CLI can install by GitHub shorthand are kept.
use super::super::resource::Error;
use serde_json::{Value, json};
use std::io::Read;
use std::time::Duration;

const LIMIT: &str = "50";
const MAX_BODY: u64 = 4 * 1024 * 1024;

pub struct Registry {
    pub base: String,
}

/// GitHub shorthand `owner/repo` only, so a search result can never become a URL or a flag.
pub fn valid_source(source: &str) -> bool {
    let part = |p: &str| {
        (1..=100).contains(&p.len())
            && p.as_bytes()[0].is_ascii_alphanumeric()
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    };
    matches!(source.split_once('/'), Some((owner, repo)) if part(owner) && part(repo))
}

/// A skill name as the CLI accepts after `--skill`: no leading dash, no shell or control characters.
pub fn valid_skill(skill: &str) -> bool {
    (1..=128).contains(&skill.len())
        && skill.as_bytes()[0].is_ascii_alphanumeric()
        && skill
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b" ._-".contains(&b))
}

fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(200).collect()
}

impl Registry {
    pub fn skills_sh() -> Self {
        Self {
            base: "https://skills.sh".into(),
        }
    }

    pub fn search(&self, query: &str) -> Result<Value, Error> {
        let query = query.trim();
        if !(2..=100).contains(&query.chars().count()) {
            return Err(Error::Invalid(
                "Enter 2 to 100 characters to search skills.".into(),
            ));
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("firecrab")
            .build()
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let response = client
            .get(format!("{}/api/search", self.base))
            .query(&[("q", query), ("limit", LIMIT)])
            .send()
            .map_err(|_| Error::Invalid("skills.sh is unavailable. Try again later.".into()))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(Error::Invalid(format!(
                "skills.sh search failed with HTTP {status}"
            )));
        }
        let mut body = Vec::new();
        response
            .take(MAX_BODY + 1)
            .read_to_end(&mut body)
            .map_err(|_| Error::Invalid("skills.sh response could not be read".into()))?;
        if body.len() as u64 > MAX_BODY {
            return Err(Error::Invalid("skills.sh response is too large".into()));
        }
        let data: Value = serde_json::from_slice(&body)?;
        let skills: Vec<Value> = data["skills"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let source = entry["source"].as_str()?;
                let skill = entry["skillId"].as_str()?;
                (valid_source(source) && valid_skill(skill)).then(|| {
                    json!({
                        "name": clean(entry["name"].as_str().unwrap_or(skill)),
                        "skill": skill,
                        "source": source,
                        "installs": entry["installs"].as_u64().unwrap_or(0),
                    })
                })
            })
            .collect();
        Ok(json!({"query": query, "count": skills.len(), "skills": skills}))
    }
}

#[cfg(test)]
mod tests;

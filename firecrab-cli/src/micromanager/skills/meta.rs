//! SKILL.md frontmatter and license classification.
use super::super::resource::Error;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub license: Option<String>,
}

/// Only an Apache-2.0 skill may be copied out of the official store.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum License {
    Apache2,
    Proprietary,
    Unknown,
}

/// Read the single-line `key: value` pairs between the opening and closing `---` fences.
pub fn parse_frontmatter(text: &str) -> Result<SkillMeta, Error> {
    let invalid = || Error::Invalid("SKILL.md has no valid frontmatter".into());
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Err(invalid());
    }
    let (mut name, mut description, mut license) = (None, String::new(), None);
    for line in lines {
        let line = line.trim_end();
        if line == "---" {
            let name = name
                .filter(|n: &String| !n.is_empty())
                .ok_or_else(invalid)?;
            return Ok(SkillMeta {
                name,
                description,
                license,
            });
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = unquote(value.trim()).to_owned();
        match key.trim() {
            "name" => name = Some(value),
            "description" => description = value,
            "license" => license = Some(value),
            _ => {}
        }
    }
    Err(invalid())
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// A frontmatter `Proprietary…` always wins; otherwise only an Apache 2.0 LICENSE.txt is trusted.
pub fn classify(license_field: Option<&str>, license_txt: Option<&str>) -> License {
    if license_field.is_some_and(|f| {
        f.trim_start()
            .to_ascii_lowercase()
            .starts_with("proprietary")
    }) {
        return License::Proprietary;
    }
    let head = license_txt.map(|t| {
        let mut end = t.len().min(600);
        while !t.is_char_boundary(end) {
            end -= 1;
        }
        &t[..end]
    });
    if head.is_some_and(|h| h.contains("Apache License") && h.contains("Version 2.0")) {
        License::Apache2
    } else {
        License::Unknown
    }
}

#[cfg(test)]
mod tests;

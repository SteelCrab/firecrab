//! User-selected context stays inside the authorized source and never fetches remote content.
use super::{files, redact};
use crate::pipe_ai::SavedConnection;
use firecrab_api_types::SandboxProjectSpec;
use std::{collections::BTreeSet, io, path::Path};

/// Validate bounded source-relative context before persisting project settings.
pub fn validate(spec: &SandboxProjectSpec) -> Result<(), String> {
    let context = &spec.context;
    if context.selected_skills.len() > 8 || context.references.len() > 16 {
        return Err("Select at most eight skills and sixteen reference files".into());
    }
    let mut paths = BTreeSet::new();
    for path in context.selected_skills.iter().chain(&context.references) {
        if !files::allowed(path) || !paths.insert(path) {
            return Err("Context paths must be distinct permitted relative project files".into());
        }
    }
    if context.selected_skills.iter().any(|path| {
        Path::new(path)
            .file_name()
            .is_none_or(|name| name != "SKILL.md")
    }) {
        return Err("Select source-relative SKILL.md files".into());
    }
    if let Some(reference) = &context.git_ref
        && (reference.is_empty()
            || reference.len() > 256
            || reference.starts_with('-')
            || reference.chars().any(char::is_control))
    {
        return Err("Use a local Git ref or commit up to 256 bytes".into());
    }
    if spec.source_path.is_none() && (!paths.is_empty() || context.git_ref.is_some()) {
        return Err("Select a source directory before adding project context".into());
    }
    Ok(())
}

/// Resolve a local ref once. Admission stores the commit, so a moving branch cannot retarget work.
pub fn resolve(spec: &SandboxProjectSpec) -> io::Result<Option<String>> {
    let Some(reference) = &spec.context.git_ref else {
        return Ok(None);
    };
    let source = Path::new(
        spec.source_path
            .as_deref()
            .ok_or_else(|| io::Error::other("Git context requires a source"))?,
    );
    let object = format!("{reference}^{{commit}}");
    let bytes = git(
        source,
        &["rev-parse", "--verify", "--end-of-options", &object],
        128,
    )?;
    let commit = std::str::from_utf8(&bytes)
        .map_err(|_| io::Error::other("Invalid Git commit"))?
        .trim();
    if ![40, 64].contains(&commit.len()) || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::other("Git ref does not identify a local commit"));
    }
    Ok(Some(commit.to_owned()))
}

/// Run read-only Git with no inherited credentials, hooks, external configuration or protocols.
pub fn git(source: &Path, args: &[&str], limit: usize) -> io::Result<Vec<u8>> {
    super::git::Snapshot::open(source)?.run(args, limit)
}

/// Load selected skills and references with the same path and credential boundaries as file tools.
pub fn text(
    spec: &SandboxProjectSpec,
    commit: Option<&str>,
    connections: &[SavedConnection],
) -> io::Result<String> {
    let mut result = String::new();
    let Some(source) = spec.source_path.as_deref() else {
        return Ok(result);
    };
    let source = Path::new(source);
    let root = files::Root::open(source)?;
    let repository = commit
        .map(|_| super::git::Snapshot::open(source))
        .transpose()?;
    for (label, paths) in [
        ("Selected skill", &spec.context.selected_skills),
        ("Reference data", &spec.context.references),
    ] {
        for path in paths {
            let bytes = if let Some(commit) = commit {
                let repository = repository
                    .as_ref()
                    .ok_or_else(|| io::Error::other("Git context snapshot is unavailable"))?;
                let entry = repository.run(&["ls-tree", commit, "--", path], 2048)?;
                if !entry.starts_with(b"100644 blob ") && !entry.starts_with(b"100755 blob ") {
                    return Err(io::Error::other("Git context must select a regular file"));
                }
                repository.run(
                    &["cat-file", "blob", &format!("{commit}:{path}")],
                    32 * 1024,
                )?
            } else {
                root.read(path)?
            };
            if bytes.len() > 32 * 1024 {
                return Err(io::Error::other("Context file exceeds 32 KiB"));
            }
            let content = std::str::from_utf8(&bytes)
                .map_err(|_| io::Error::other("Context file must be UTF-8 text"))?;
            let content = redact(connections, content);
            result.push_str(&format!("\n{label} ({path}):\n{content}\n"));
            if result.len() > 64 * 1024 {
                return Err(io::Error::other("Selected project context exceeds 64 KiB"));
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn context_rejects_credentials_traversal_and_unselected_host_files() {
        let mut spec: SandboxProjectSpec = serde_json::from_value(
            serde_json::json!({"name":"p","sourcePath":"/project","vmId":null,"selection":null}),
        )
        .unwrap();
        for path in [
            "../SKILL.md",
            "/tmp/SKILL.md",
            ".codex/SKILL.md",
            ".aws/credentials",
            ".env",
        ] {
            spec.context.references = vec![path.into()];
            assert!(validate(&spec).is_err(), "{path}");
        }
        spec.context.references = vec!["docs/design.md".into()];
        spec.context.selected_skills = vec![".agents/skills/review/SKILL.md".into()];
        assert!(validate(&spec).is_ok());
        spec.context.git_ref = Some("--exec=anything".into());
        assert!(validate(&spec).is_err());
    }
}

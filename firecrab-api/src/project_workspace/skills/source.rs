//! Bounded HTTPS downloads from fixed official repositories; refs and file bytes are pinned.
use super::*;
use futures_util::StreamExt;
use std::collections::BTreeSet;
/// Largest downloaded source package, independent of transport Content-Length.
pub(super) const PACKAGE_LIMIT: usize = 2 * 1024 * 1024;
/// One regular source blob; executable bits are not run during installation.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Blob {
    /// Entry-relative path preserved for references and scripts.
    pub path: String,
    /// Original bytes, encoded only for private durable JSON storage.
    pub base64: String,
}
/// Tree paths and Git blob proofs from an exact commit.
#[derive(Deserialize)]
struct Tree {
    /// A partial GitHub result cannot establish a complete installation.
    truncated: bool,
    /// Source entries; submodules and symlinks are rejected if selected.
    tree: Vec<Item>,
}
/// One Git tree entry returned by the official host.
#[derive(Deserialize)]
struct Item {
    /// Repository-relative name.
    path: String,
    /// Git mode, with 120000 identifying symlinks.
    mode: String,
    /// Blob or tree object kind.
    #[serde(rename = "type")]
    kind: String,
    /// Exact blob identity.
    sha: String,
    /// Uncompressed blob length when provided.
    size: Option<usize>,
}
/// Fixed source identities are also used by the model compatibility check.
pub(super) fn repo(source: ProjectSkillSource) -> &'static str {
    match source {
        ProjectSkillSource::Openai => "openai/plugins",
        ProjectSkillSource::Anthropic => "anthropics/skills",
    }
}
/// Accept only full immutable Git revisions, never user-provided branches or URLs.
pub(super) fn revision(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
/// Bound the actual streamed response and reject redirects or unsuccessful statuses.
async fn fetch(url: reqwest::Url, limit: usize) -> Result<Vec<u8>, Error> {
    let client = reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("Firecrab-project-skills/1")
        .build()
        .map_err(|_| Error::Conflict("Skill HTTP client unavailable"))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| Error::Conflict("Official skill source unavailable"))?;
    if !response.status().is_success() {
        return Err(Error::Conflict(
            "Official skill source rejected the request; retry later",
        ));
    }
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(Error::Invalid("Official source exceeds the download limit"));
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(part) = stream.next().await {
        let part = part.map_err(|_| Error::Conflict("Official skill download was interrupted"))?;
        if part.len() > limit.saturating_sub(bytes.len()) {
            return Err(Error::Invalid("Official source exceeds the download limit"));
        }
        bytes.extend_from_slice(&part);
    }
    Ok(bytes)
}
/// Construct raw URLs by path segments so repository data never becomes URL syntax.
fn raw_url(source: ProjectSkillSource, rev: &str, path: &str) -> Result<reqwest::Url, Error> {
    if !revision(rev) {
        return Err(Error::Invalid("Use a full source commit"));
    }
    files_guest::path(path, false)?;
    let mut url = reqwest::Url::parse("https://raw.githubusercontent.com/").unwrap();
    url.path_segments_mut()
        .unwrap()
        .extend(repo(source).split('/'))
        .push(rev)
        .extend(path.split('/'));
    Ok(url)
}
/// Verify Git's blob identity in addition to HTTPS and the pinned tree.
fn verify_blob(item: &Item, bytes: &[u8]) -> Result<(), Error> {
    let mut hash = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
    hash.update(format!("blob {}\0", bytes.len()).as_bytes());
    hash.update(bytes);
    let actual: String = hash
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if item.sha != actual || item.size.is_some_and(|n| n != bytes.len()) {
        return Err(Error::Conflict(
            "Official source blob failed its pinned integrity check",
        ));
    }
    Ok(())
}
/// Fetch one complete pinned source tree; the response cannot choose another repository.
async fn tree(source: ProjectSkillSource, rev: &str) -> Result<Tree, Error> {
    if !revision(rev) {
        return Err(Error::Invalid("Use a full source commit"));
    }
    let url = reqwest::Url::parse(&format!(
        "https://api.github.com/repos/{}/git/trees/{rev}?recursive=1",
        repo(source)
    ))
    .unwrap();
    let tree: Tree = serde_json::from_slice(&fetch(url, 4 * 1024 * 1024).await?)?;
    if tree.truncated || tree.tree.len() > 16384 {
        return Err(Error::Invalid(
            "Official source tree is incomplete or too large",
        ));
    }
    Ok(tree)
}
/// Read a regular pinned blob without extracting source-controlled archive entries.
async fn blob(
    source: ProjectSkillSource,
    rev: &str,
    item: &Item,
    limit: usize,
) -> Result<Vec<u8>, Error> {
    if item.kind != "blob"
        || !matches!(item.mode.as_str(), "100644" | "100755")
        || !revision(&item.sha)
    {
        return Err(Error::Invalid(
            "Skill packages require regular files; links and submodules are unsupported",
        ));
    }
    let bytes = fetch(raw_url(source, rev, &item.path)?, limit).await?;
    verify_blob(item, &bytes)?;
    Ok(bytes)
}
/// Discover marketplace entries from the same pinned tree and marketplace file.
async fn entries(
    source: ProjectSkillSource,
    rev: &str,
    tree: &Tree,
) -> Result<Vec<ProjectSkillEntry>, Error> {
    let marketplace = match source {
        ProjectSkillSource::Openai => ".agents/plugins/marketplace.json",
        ProjectSkillSource::Anthropic => ".claude-plugin/marketplace.json",
    };
    let item = tree
        .tree
        .iter()
        .find(|f| f.path == marketplace)
        .ok_or(Error::Missing)?;
    let data: serde_json::Value =
        serde_json::from_slice(&blob(source, rev, item, 256 * 1024).await?)?;
    parse_entries(source, rev, tree, &data)
}
/// Ignore marketplace entries pointing to other publishers; never follow a catalog-supplied URL.
fn parse_entries(
    source: ProjectSkillSource,
    rev: &str,
    tree: &Tree,
    data: &serde_json::Value,
) -> Result<Vec<ProjectSkillEntry>, Error> {
    let plugins = data["plugins"]
        .as_array()
        .ok_or(Error::Invalid("Invalid official marketplace"))?;
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for plugin in plugins {
        if source == ProjectSkillSource::Openai
            && plugin["source"]["source"].as_str() != Some("local")
        {
            continue;
        }
        let roots: Vec<String> = match source {
            ProjectSkillSource::Openai => plugin["source"]["path"]
                .as_str()
                .into_iter()
                .map(|s| s.trim_start_matches("./").to_owned())
                .collect(),
            ProjectSkillSource::Anthropic => plugin["skills"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s.as_str())
                .map(|s| s.trim_start_matches("./").to_owned())
                .collect(),
        };
        for root in roots {
            files_guest::path(&root, false)?;
            if !root.starts_with(if source == ProjectSkillSource::Openai {
                "plugins/"
            } else {
                "skills/"
            }) {
                return Err(Error::Invalid(
                    "Marketplace path escapes the official source layout",
                ));
            }
            let prefix = format!("{root}/");
            let instructions = tree
                .tree
                .iter()
                .filter_map(|f| f.path.strip_prefix(&prefix))
                .filter(|p| {
                    *p == "SKILL.md" || (p.starts_with("skills/") && p.ends_with("/SKILL.md"))
                })
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if instructions.is_empty() || !seen.insert(root.clone()) {
                continue;
            }
            let bundle = source == ProjectSkillSource::Openai;
            result.push(ProjectSkillEntry {
                source,
                kind: if bundle {
                    ProjectSkillKind::SkillBundle
                } else {
                    ProjectSkillKind::Skill
                },
                name: root.rsplit('/').next().unwrap().into(),
                path: root.clone(),
                revision: rev.into(),
                instructions: instructions.clone(),
            });
            if bundle {
                for instruction in instructions {
                    let relative = instruction.strip_suffix("/SKILL.md").unwrap_or("");
                    if relative.is_empty() {
                        continue;
                    }
                    let path = format!("{root}/{relative}");
                    if seen.insert(path.clone()) {
                        result.push(ProjectSkillEntry {
                            source,
                            kind: ProjectSkillKind::Skill,
                            name: format!(
                                "{} / {}",
                                root.rsplit('/').next().unwrap(),
                                relative.trim_start_matches("skills/")
                            ),
                            path,
                            revision: rev.into(),
                            instructions: vec!["SKILL.md".into()],
                        });
                    }
                }
            }
        }
    }
    if result.len() > 1024 {
        return Err(Error::Invalid("Official catalog exceeds its entry limit"));
    }
    result.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(result)
}
/// Refresh only on explicit request; retain the resolved commit with every entry.
pub(super) async fn discover(source: ProjectSkillSource) -> Result<ProjectSkillCatalog, Error> {
    let url = reqwest::Url::parse(&format!(
        "https://api.github.com/repos/{}/git/ref/heads/main",
        repo(source)
    ))
    .unwrap();
    let reference: serde_json::Value = serde_json::from_slice(&fetch(url, 65536).await?)?;
    let rev = reference["object"]["sha"]
        .as_str()
        .filter(|s| revision(s))
        .ok_or(Error::Invalid("Invalid official source revision"))?;
    let tree = tree(source, rev).await?;
    Ok(ProjectSkillCatalog {
        source,
        url: format!("https://github.com/{}", repo(source)),
        revision: Some(rev.into()),
        entries: entries(source, rev, &tree).await?,
    })
}
/// Validate the instruction entry without applying its optional tool or runtime declarations.
fn instruction(bytes: &[u8]) -> Result<(), Error> {
    #[derive(Deserialize)]
    struct Header {
        name: String,
        description: String,
    }
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Invalid("SKILL.md must be UTF-8"))?;
    if bytes.len() > 32768 || bytes.contains(&0) {
        return Err(Error::Invalid(
            "SKILL.md must be at most 32 KiB without NUL",
        ));
    }
    let text = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
        .ok_or(Error::Invalid("SKILL.md requires YAML frontmatter"))?;
    let end = text
        .find("\n---")
        .ok_or(Error::Invalid("SKILL.md frontmatter is incomplete"))?;
    let header: Header = serde_saphyr::from_str(&text[..end])
        .map_err(|_| Error::Invalid("SKILL.md requires a valid name and description"))?;
    if header.name.is_empty()
        || header.name.len() > 64
        || header.description.trim().is_empty()
        || header.description.len() > 1024
    {
        return Err(Error::Invalid(
            "Skill name or description exceeds the supported format",
        ));
    }
    Ok(())
}
/// Keep companion license/notice files from source ancestors without activating plugin surfaces.
fn package_path(entry: &ProjectSkillEntry, item: &Item) -> Option<String> {
    let licenses = [
        "LICENSE",
        "LICENSE.txt",
        "LICENSE.md",
        "LICENSE-MIT",
        "LICENSE-APACHE",
        "COPYING",
        "NOTICE",
        "NOTICE.txt",
        "THIRD_PARTY_NOTICES.md",
    ];
    if let Some(path) = item.path.strip_prefix(&format!("{}/", entry.path)) {
        if entry.kind == ProjectSkillKind::Skill
            || path.starts_with("skills/")
            || path == "SKILL.md"
            || licenses.contains(&path)
        {
            return Some(path.into());
        }
        return None;
    }
    let (parent, name) = item.path.rsplit_once('/').unwrap_or(("", &item.path));
    if licenses.contains(&name)
        && (parent.is_empty() || entry.path.starts_with(&format!("{parent}/")))
    {
        return Some(format!(".source-licenses/{}", item.path));
    }
    None
}
/// Download a selected official entry completely before persisting install admission.
pub(super) async fn package(
    input: &InstallProjectSkill,
) -> Result<(ProjectSkillEntry, Vec<Blob>), Error> {
    let tree = tree(input.source, &input.revision).await?;
    let entry = entries(input.source, &input.revision, &tree)
        .await?
        .into_iter()
        .find(|e| e.path == input.path && e.kind == input.kind)
        .ok_or(Error::Invalid(
            "Select an entry from the official marketplace",
        ))?;
    let mut files = Vec::new();
    let mut total = 0;
    for item in &tree.tree {
        if item.kind == "tree" {
            continue;
        }
        let Some(path) = package_path(&entry, item) else {
            continue;
        };
        files_guest::path(&path, false)?;
        files_guest::path(
            &format!(".agents/firecrab-skills/placeholder/{path}"),
            false,
        )?;
        if files.len() >= 128 {
            return Err(Error::Invalid("Skill package exceeds 128 files"));
        }
        let bytes = blob(
            input.source,
            &input.revision,
            item,
            files_guest::FILE_LIMIT.min(PACKAGE_LIMIT - total),
        )
        .await?;
        total += bytes.len();
        files.push(Blob {
            path,
            base64: STANDARD.encode(bytes),
        });
    }
    if files.is_empty() {
        return Err(Error::Invalid("Official skill package has no files"));
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    for path in &entry.instructions {
        let file = files
            .iter()
            .find(|f| &f.path == path)
            .ok_or(Error::Invalid("Skill instructions are missing"))?;
        let bytes = STANDARD
            .decode(&file.base64)
            .map_err(|_| Error::Invalid("Invalid skill bytes"))?;
        instruction(&bytes)?;
    }
    Ok((entry, files))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_revisions_paths_and_blob_proofs_are_fixed() {
        assert!(!revision("main"));
        assert!(!revision(&"A".repeat(40)));
        assert!(revision(&"a".repeat(40)));
        assert!(raw_url(ProjectSkillSource::Openai, &"a".repeat(40), "../escape").is_err());
        let url = raw_url(
            ProjectSkillSource::Anthropic,
            &"a".repeat(40),
            "skills/example/a#b.md",
        )
        .unwrap();
        assert_eq!(url.host_str(), Some("raw.githubusercontent.com"));
        assert!(url.fragment().is_none());
        assert!(url.as_str().contains("a%23b.md"));
        let item = Item {
            path: "README".into(),
            mode: "100644".into(),
            kind: "blob".into(),
            sha: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
            size: Some(6),
        };
        assert!(verify_blob(&item, b"hello\n").is_ok());
        assert!(verify_blob(&item, b"changed").is_err());
    }
    #[test]
    fn remote_marketplace_plugins_cannot_redirect_the_official_source_adapter() {
        let tree = Tree {
            truncated: false,
            tree: vec![Item {
                path: "plugins/local/skills/review/SKILL.md".into(),
                mode: "100644".into(),
                kind: "blob".into(),
                sha: "a".repeat(40),
                size: Some(10),
            }],
        };
        let data = serde_json::json!({"plugins":[{"name":"local","source":{"source":"local","path":"./plugins/local"}},{"name":"external","source":{"source":"git-subdir","url":"https://example.invalid/code.git","path":"packages/external"}}]});
        let entries =
            parse_entries(ProjectSkillSource::Openai, &"a".repeat(40), &tree, &data).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.path.starts_with("plugins/local")));
        assert!(
            entries
                .iter()
                .any(|e| e.kind == ProjectSkillKind::SkillBundle)
        );
        assert!(entries.iter().any(|e| e.kind == ProjectSkillKind::Skill));
    }
    #[test]
    fn bundles_keep_source_licenses_without_activating_plugin_hooks_or_connectors() {
        let entry = ProjectSkillEntry {
            source: ProjectSkillSource::Openai,
            kind: ProjectSkillKind::SkillBundle,
            path: "plugins/example".into(),
            name: "example".into(),
            revision: "a".repeat(40),
            instructions: vec!["skills/review/SKILL.md".into()],
        };
        let mut item = Item {
            path: "LICENSE".into(),
            mode: "100644".into(),
            kind: "blob".into(),
            sha: "a".repeat(40),
            size: None,
        };
        assert_eq!(
            package_path(&entry, &item).unwrap(),
            ".source-licenses/LICENSE"
        );
        item.path = "plugins/example/hooks.json".into();
        assert!(package_path(&entry, &item).is_none());
        item.path = "plugins/example/skills/review/SKILL.md".into();
        assert_eq!(
            package_path(&entry, &item).unwrap(),
            "skills/review/SKILL.md"
        );
        item.path = "plugins/unrelated/LICENSE".into();
        assert!(package_path(&entry, &item).is_none());
    }
    #[test]
    fn skill_metadata_is_validated_without_activating_tools_or_installers() {
        assert!(instruction(b"---\nname: review\ndescription: Review code\nallowed-tools: Bash\n---\nInstructions\n").is_ok());
        assert!(instruction(b"---\nname: review\n---\nNo description").is_err());
        assert!(instruction(b"Not a skill").is_err());
        assert!(instruction(b"---\nname: review\ndescription: \"\"\n---\n").is_err());
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    /// Explicit network QA only. It downloads public data into memory and executes no skill code.
    #[tokio::test]
    #[ignore = "manual public-source network validation; not workload-VM or real-model acceptance"]
    async fn official_catalogs_and_small_packages_match_their_pinned_git_blobs() {
        for (source, suffix) in [
            (ProjectSkillSource::Openai, "/brainstorming"),
            (ProjectSkillSource::Anthropic, "/brand-guidelines"),
        ] {
            let catalog = discover(source).await.unwrap();
            assert!(!catalog.entries.is_empty());
            let entry = catalog
                .entries
                .iter()
                .find(|e| e.kind == ProjectSkillKind::Skill && e.path.ends_with(suffix))
                .expect("official reference skill exists");
            let (entry, files) = package(&InstallProjectSkill {
                source,
                path: entry.path.clone(),
                revision: entry.revision.clone(),
                kind: entry.kind,
            })
            .await
            .unwrap();
            let record =
                super::super::record(Uuid::new_v4(), Caller::default(), entry, files).unwrap();
            record.validate().unwrap();
            assert!(record.public.files.iter().all(|f| f.version == 0));
            println!(
                "Pinned {:?} {} {}: {} files verified, no code executed",
                source,
                record.public.entry.path,
                record.public.entry.revision,
                record.public.files.len()
            );
        }
    }
}

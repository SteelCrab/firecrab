//! Additive project-local skill catalog, immutable package admission and install progress.
use super::*;
use rusqlite::{OptionalExtension, params};
/// Both discovery and installation survive controller restart without a second authority.
pub(in crate::project_workspace) const SCHEMA: &str =
    "CREATE TABLE skill_catalogs(source TEXT PRIMARY KEY,document TEXT NOT NULL) STRICT;
CREATE TABLE skill_installations(id TEXT PRIMARY KEY,document TEXT NOT NULL) STRICT;";
/// Private admitted package; file operation IDs are fixed before any guest write.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Record {
    /// Public source revision and per-file completion receipts.
    pub public: ProjectSkillInstallation,
    /// Exact caller for file author provenance, not a credential.
    pub caller: Caller,
    /// File-version audit time fixed at package admission and retained across retries.
    pub created_at: u64,
    /// Frozen source bytes required to resume interrupted downloads/writes.
    pub blobs: Vec<source::Blob>,
}
/// Compute a stable installation manifest independently of process and request timing.
pub(super) fn digest(files: &[ProjectSkillFile]) -> Result<String, Error> {
    let manifest: Vec<_> = files
        .iter()
        .map(|f| (&f.path, &f.sha256, f.byte_length))
        .collect();
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&manifest)?)
    ))
}
impl Record {
    /// Validate retained bytes and manifest before recovery or task use.
    pub(super) fn validate(&self) -> Result<(), Error> {
        let p = &self.public;
        files_guest::path(&p.entry.path, false)?;
        if self.created_at > i64::MAX as u64
            || p.id.is_nil()
            || !source::revision(&p.entry.revision)
            || p.root != format!(".agents/firecrab-skills/{}", p.id)
            || p.files.is_empty()
            || p.files.len() > 128
            || p.files.len() != self.blobs.len()
            || digest(&p.files)? != p.sha256
        {
            return Err(Error::Conflict(
                "Retained skill installation failed its identity check",
            ));
        }
        let mut last = "";
        let mut total = 0;
        for (file, blob) in p.files.iter().zip(&self.blobs) {
            files_guest::path(&file.path, false)?;
            files_guest::path(&format!("{}/{}", p.root, file.path), false)?;
            let bytes = STANDARD
                .decode(&blob.base64)
                .map_err(|_| Error::Conflict("Retained skill bytes are invalid"))?;
            total += bytes.len();
            if file.path != blob.path
                || file.path.as_str() <= last
                || file.operation_id.is_nil()
                || file.byte_length != bytes.len() as u64
                || format!("{:x}", Sha256::digest(&bytes)) != file.sha256
                || bytes.len() > files_guest::FILE_LIMIT
                || total > source::PACKAGE_LIMIT
                || (p.status == ProjectSkillStatus::Installed && file.version == 0)
            {
                return Err(Error::Conflict("Retained skill file proof changed"));
            }
            last = &file.path;
        }
        if p.entry.instructions.is_empty()
            || p.entry
                .instructions
                .iter()
                .any(|name| !p.files.iter().any(|f| &f.path == name))
        {
            return Err(Error::Conflict("Retained skill instructions are missing"));
        }
        Ok(())
    }
    /// An operation UUID cannot be reused to install another commit, entry or runtime kind.
    pub(super) fn matches(&self, input: &InstallProjectSkill) -> bool {
        let e = &self.public.entry;
        e.source == input.source
            && e.path == input.path
            && e.revision == input.revision
            && e.kind == input.kind
    }
}
impl super::super::db::ProjectDb {
    /// Read the last accepted official-source catalogs without network or VM access.
    pub(super) fn skill_catalogs(&self) -> Result<Vec<ProjectSkillCatalog>, Error> {
        let mut result = Vec::new();
        for source in [ProjectSkillSource::Openai, ProjectSkillSource::Anthropic] {
            let json: Option<String> = self
                .conn
                .query_row(
                    "SELECT document FROM skill_catalogs WHERE source=?1",
                    [source::repo(source)],
                    |r| r.get(0),
                )
                .optional()?;
            result.push(match json {
                Some(json) => serde_json::from_str(&json)?,
                None => ProjectSkillCatalog {
                    source,
                    url: format!("https://github.com/{}", source::repo(source)),
                    revision: None,
                    entries: vec![],
                },
            });
        }
        Ok(result)
    }
    /// Publish complete discovery results under this operation's ownership epoch.
    pub(super) fn save_skill_catalog(&self, catalog: &ProjectSkillCatalog) -> Result<(), Error> {
        self.check_owner()?;
        let json = serde_json::to_string(catalog)?;
        if json.len() > 1024 * 1024 {
            return Err(Error::Invalid("Official skill catalog exceeds its limit"));
        }
        self.conn.execute("INSERT INTO skill_catalogs VALUES(?1,?2) ON CONFLICT(source) DO UPDATE SET document=excluded.document",params![source::repo(catalog.source),json])?;
        Ok(())
    }
    /// Load one exact retained installation, including pending operations.
    pub(super) fn skill_record(&self, id: Uuid) -> Result<Record, Error> {
        let json: String = self
            .conn
            .query_row(
                "SELECT document FROM skill_installations WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(Error::Missing)?;
        let record: Record = serde_json::from_str(&json)?;
        record.validate()?;
        if record.public.id != id {
            return Err(Error::Conflict("Skill installation identity changed"));
        }
        Ok(record)
    }
    /// Persist a complete package or progress checkpoint before proceeding to the next file.
    pub(super) fn save_skill_record(&self, record: &Record) -> Result<(), Error> {
        self.check_owner()?;
        record.validate()?;
        let json = serde_json::to_string(record)?;
        let (count, size): (i64, i64) = self.conn.query_row(
            "SELECT count(*),COALESCE(sum(length(document)),0) FROM skill_installations",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM skill_installations WHERE id=?1)",
            [record.public.id.to_string()],
            |r| r.get(0),
        )?;
        let previous: i64 = self.conn.query_row(
            "SELECT COALESCE((SELECT length(document) FROM skill_installations WHERE id=?1),0)",
            [record.public.id.to_string()],
            |r| r.get(0),
        )?;
        if (!exists && count >= 64)
            || size
                .saturating_sub(previous)
                .saturating_add(json.len() as i64)
                > 64 * 1024 * 1024
        {
            return Err(Error::Conflict(
                "Skill retention quota reached; preserve a checkpoint before maintenance",
            ));
        }
        self.conn.execute("INSERT INTO skill_installations VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET document=excluded.document",params![record.public.id.to_string(),json])?;
        Ok(())
    }
    /// List confirmed and interrupted installations without exposing frozen package bytes.
    pub(super) fn skill_installations(&self) -> Result<Vec<ProjectSkillInstallation>, Error> {
        let mut query = self
            .conn
            .prepare("SELECT id FROM skill_installations ORDER BY rowid DESC LIMIT 64")?;
        let ids = query
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                let id = Uuid::parse_str(&id)
                    .map_err(|_| Error::Conflict("Invalid retained skill UUID"))?;
                self.skill_record(id).map(|r| r.public)
            })
            .collect()
    }
}

//! One private SQLite file and one advisory ownership lock per project.
use super::{Binding, Error};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

/// Private schema with an additive file-history migration from version 1.
pub const SCHEMA_VERSION: u32 = 4;
/// SQLite application identity prevents opening an unrelated database.
const APPLICATION_ID: i64 = 0x46575031;
/// Maximum serialized size of a project metadata object.
pub const DOCUMENT_LIMIT: usize = 64 * 1024;

/// An explicit unlock prevents unrelated forked children from retaining completed ownership.
#[derive(Debug)]
pub(super) struct OwnerLock(File);
impl OwnerLock {
    /// Wrap only a successfully acquired lock, including error paths during database opening.
    pub(super) fn acquire(file: File) -> Result<Self, Error> {
        file.try_lock().map_err(|_| Error::Busy)?;
        Ok(Self(file))
    }
}
impl Drop for OwnerLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// A private connection fenced by an advisory lock and persisted operation epoch.
pub struct ProjectDb {
    /// Canonical directory containing the database and private artifacts.
    pub directory: PathBuf,
    /// Connection owned exclusively for this bounded operation.
    pub conn: Connection,
    /// Persisted epoch checked by each mutation.
    pub epoch: i64,
    /// Retains the advisory lock until the entire operation is dropped.
    _owner: OwnerLock,
    /// Keep the owning service lock alive through the last in-flight operation.
    pub(super) service: Option<std::sync::Arc<OwnerLock>>,
}

/// Create or validate a server-owned directory without following its final symlink.
pub fn private_dir(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } => {
            return Err(Error::Conflict(
                "Private workspace directory has an unexpected owner or type",
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)?;
            File::open(
                path.parent()
                    .ok_or(Error::Conflict("Invalid private directory"))?,
            )?
            .sync_all()?;
        }
        Err(e) => return Err(e.into()),
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Open a regular owner-only file and reject aliases to other data.
pub(super) fn private_file(path: &Path, create: bool) -> Result<File, Error> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.nlink() != 1 || meta.uid() != unsafe { libc::geteuid() } {
        return Err(Error::Conflict(
            "Private workspace file has an unexpected owner or type",
        ));
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

/// Resolve only the retained project's private directory; creation needs provisioning authority.
pub(super) fn project_directory(
    root: &Path,
    binding: &Binding,
    create: bool,
) -> Result<PathBuf, Error> {
    let initialize = create && binding.provisioning;
    if !root.exists() {
        if !initialize {
            return Err(Error::Conflict("Retained project database is missing"));
        }
        private_dir(
            root.parent()
                .ok_or(Error::Conflict("Invalid workspace root"))?,
        )?;
        private_dir(root)?;
    }
    private_dir(root)?;
    // Resolve operator-owned parent aliases before SQLite enforces NOFOLLOW on the database path.
    let directory = fs::canonicalize(root)?.join(binding.project_id.to_string());
    if !directory.exists() && !initialize {
        return Err(Error::Conflict("Retained project database is missing"));
    }
    private_dir(&directory)?;
    Ok(directory)
}

impl ProjectDb {
    /// Open an existing identity; only interrupted initial provisioning permits creation.
    pub fn open(root: &Path, binding: &Binding, owner: Uuid, create: bool) -> Result<Self, Error> {
        let initialize = create && binding.provisioning;
        let directory = project_directory(root, binding, create)?;
        let lock = private_file(&directory.join("owner.lock"), true)?;
        let lock = OwnerLock::acquire(lock)?;
        let path = directory.join("project.db");
        if !path.exists() && !initialize {
            return Err(Error::Conflict("Retained project database is missing"));
        }
        let file = private_file(&path, initialize)?;
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        conn.busy_timeout(Duration::from_secs(2))?;
        let application: i64 = conn.pragma_query_value(None, "application_id", |r| r.get(0))?;
        let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let new_database = application == 0 && version == 0 && initialize;
        if !new_database
            && (application != APPLICATION_ID || version == 0 || version > SCHEMA_VERSION)
        {
            return Err(Error::Conflict(
                "Unexpected project database identity or schema version",
            ));
        }
        let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        if mode != "wal" {
            return Err(Error::Conflict(
                "Project database requires local-disk WAL support",
            ));
        }
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        if version == 0 {
            conn.execute_batch("CREATE TABLE identity(singleton INTEGER PRIMARY KEY CHECK(singleton=1), project_id TEXT NOT NULL, database_id TEXT NOT NULL, volume_id TEXT NOT NULL, owner TEXT NOT NULL, epoch INTEGER NOT NULL, revision INTEGER NOT NULL, document TEXT NOT NULL, import_sha TEXT, initialized INTEGER NOT NULL CHECK(initialized IN (0,1))) STRICT;")?;
            conn.execute(
                "INSERT INTO identity VALUES(1,?1,?2,?3,?4,0,0,'{}',NULL,0)",
                params![
                    binding.project_id.to_string(),
                    binding.database_id.to_string(),
                    binding.volume_id.to_string(),
                    owner.to_string()
                ],
            )?;
            conn.pragma_update(None, "application_id", APPLICATION_ID)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        let (project, database, volume, epoch): (String, String, String, i64) = conn.query_row(
            "SELECT project_id,database_id,volume_id,epoch FROM identity WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        if project != binding.project_id.to_string()
            || database != binding.database_id.to_string()
            || volume != binding.volume_id.to_string()
        {
            return Err(Error::Conflict(
                "Retained project database does not match its registered identity",
            ));
        }
        if version < 2 {
            conn.execute_batch(super::files_db::SCHEMA)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        if version < 3 {
            conn.execute_batch(super::terminals_db::SCHEMA)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        if version < 4 {
            conn.execute_batch(super::views_db::SCHEMA)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        let epoch = epoch
            .checked_add(1)
            .ok_or(Error::Conflict("Workspace owner epoch exhausted"))?;
        conn.execute(
            "UPDATE identity SET owner=?1,epoch=?2 WHERE singleton=1",
            params![owner.to_string(), epoch],
        )?;
        conn.execute_batch("COMMIT")?;
        file.sync_all()?;
        File::open(&directory)?.sync_all()?;
        Ok(Self {
            directory,
            conn,
            epoch,
            _owner: lock,
            service: None,
        })
    }

    /// Read the retained document under this operation's owner epoch.
    pub fn document(&self) -> Result<(u64, Value, bool), Error> {
        let (revision, json, initialized): (i64, String, bool) = self.conn.query_row(
            "SELECT revision,document,initialized FROM identity WHERE singleton=1 AND epoch=?1",
            [self.epoch],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if json.len() > DOCUMENT_LIMIT {
            return Err(Error::Conflict("Project metadata exceeds its limit"));
        }
        Ok((
            u64::try_from(revision).map_err(|_| Error::Conflict("Invalid workspace revision"))?,
            serde_json::from_str(&json)?,
            initialized,
        ))
    }

    /// Save a bounded object only if its revision and owning epoch still match.
    pub fn save_document(&self, expected: u64, document: &Value) -> Result<(), Error> {
        if expected > i64::MAX as u64 || !document.is_object() {
            return Err(Error::Invalid(
                "Use an object document and a valid workspace revision",
            ));
        }
        let json = serde_json::to_string(document)?;
        if json.len() > DOCUMENT_LIMIT {
            return Err(Error::Invalid("Project metadata is limited to 64 KiB"));
        }
        let changed = self.conn.execute(
            "UPDATE identity SET revision=revision+1,document=?1
             WHERE singleton=1 AND revision=?2 AND epoch=?3",
            params![json, expected as i64, self.epoch],
        )?;
        if changed != 1 {
            return Err(Error::Conflict(
                "Workspace revision or owner changed; reload before saving",
            ));
        }
        Ok(())
    }

    /// Read the immutable initial-import digest.
    pub fn import_sha(&self) -> Result<Option<String>, Error> {
        Ok(self.conn.query_row(
            "SELECT import_sha FROM identity WHERE singleton=1 AND epoch=?1",
            [self.epoch],
            |r| r.get(0),
        )?)
    }
    /// Persist the archive digest before allowing guest initialization.
    pub fn set_import_sha(&self, sha: &str) -> Result<(), Error> {
        let changed = self.conn.execute(
            "UPDATE identity SET import_sha=?1
             WHERE singleton=1 AND epoch=?2 AND (import_sha IS NULL OR import_sha=?1)",
            params![sha, self.epoch],
        )?;
        if changed != 1 {
            return Err(Error::Conflict("Workspace initialization identity changed"));
        }
        Ok(())
    }
    /// Confirm guest initialization without changing its original import identity.
    pub fn mark_initialized(&self) -> Result<(), Error> {
        if self.conn.execute(
            "UPDATE identity SET initialized=1 WHERE singleton=1 AND epoch=?1",
            [self.epoch],
        )? != 1
        {
            return Err(Error::Busy);
        }
        Ok(())
    }
}

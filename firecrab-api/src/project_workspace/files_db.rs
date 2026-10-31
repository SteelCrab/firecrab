//! Immutable file history and durable, idempotent write admission in project SQLite.
use super::{Error, db::ProjectDb};
use base64::{Engine, engine::general_purpose::STANDARD};
use firecrab_api_types::*;
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const HISTORY_LIMIT: i64 = 128 * 1024 * 1024;
const RECORD_LIMIT: i64 = 4096;

pub(super) const SCHEMA: &str = "
CREATE TABLE file_versions (
 id INTEGER PRIMARY KEY AUTOINCREMENT, path TEXT NOT NULL, content BLOB NOT NULL,
 sha256 TEXT NOT NULL, author TEXT, restored_from INTEGER REFERENCES file_versions(id),
 created_at INTEGER NOT NULL CHECK(created_at>=0)) STRICT;
CREATE INDEX file_version_path ON file_versions(path,id);
CREATE TABLE file_operations (
 id TEXT PRIMARY KEY, path TEXT NOT NULL, expected_version INTEGER NOT NULL CHECK(expected_version>=0),
 content BLOB, author TEXT NOT NULL, restored_from INTEGER REFERENCES file_versions(id),
 created_at INTEGER NOT NULL CHECK(created_at>=0),
 version_id INTEGER REFERENCES file_versions(id),
 state TEXT NOT NULL CHECK(state IN ('pending','applied','failed'))) STRICT;
CREATE UNIQUE INDEX pending_file_write ON file_operations(path) WHERE state='pending';
";

/// Immutable admitted request. Applied operations read bytes from their history record.
#[derive(Debug, Clone)]
pub(super) struct Operation {
    pub id: Uuid,
    pub path: String,
    pub expected: u64,
    pub content: Vec<u8>,
    pub author: SandboxPrincipal,
    pub restored_from: Option<u64>,
    pub created_at: u64,
    pub version: Option<u64>,
    pub state: String,
}

pub(super) struct Version {
    pub metadata: ProjectFileVersion,
    pub bytes: Vec<u8>,
}
impl Version {
    pub(super) fn response(self) -> ProjectFile {
        ProjectFile {
            version: self.metadata,
            content_base64: STANDARD.encode(self.bytes),
        }
    }
}

fn unsigned(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}
fn optional_unsigned(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    let value: Option<i64> = row.get(index)?;
    value
        .map(|v| u64::try_from(v).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, v)))
        .transpose()
}
fn integer(value: u64) -> Result<i64, Error> {
    i64::try_from(value).map_err(|_| Error::Invalid("Invalid file version"))
}
fn time() -> Result<i64, Error> {
    integer(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(std::io::Error::other)?
            .as_secs(),
    )
}

impl ProjectDb {
    pub(super) fn check_owner(&self) -> Result<(), Error> {
        let current: i64 =
            self.conn
                .query_row("SELECT epoch FROM identity WHERE singleton=1", [], |r| {
                    r.get(0)
                })?;
        if current != self.epoch {
            return Err(Error::Busy);
        }
        Ok(())
    }

    fn file_quota(&self, bytes: usize, additional: i64) -> Result<(), Error> {
        let (size, records): (i64, i64) = self.conn.query_row(
            "SELECT (SELECT COALESCE(SUM(length(content)),0) FROM file_versions) +
             (SELECT COALESCE(SUM(length(content)),0) FROM file_operations),
             (SELECT COUNT(*) FROM file_versions)+(SELECT COUNT(*) FROM file_operations)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if size.saturating_add(bytes as i64) > HISTORY_LIMIT
            || records.saturating_add(additional) > RECORD_LIMIT
        {
            return Err(Error::Conflict(
                "Project file history limit reached; preserve a checkpoint before maintenance",
            ));
        }
        Ok(())
    }

    pub(super) fn file_version(&self, path: &str, id: u64) -> Result<Version, Error> {
        let row = self.conn.query_row(
            "SELECT content,sha256,author,restored_from,created_at FROM file_versions WHERE path=?1 AND id=?2",
            params![path, integer(id)?], |r| Ok((r.get::<_,Vec<u8>>(0)?,r.get::<_,String>(1)?,
                r.get::<_,Option<String>>(2)?,optional_unsigned(r,3)?,unsigned(r,4)?)))
            .optional()?.ok_or(Error::Missing)?;
        let (content, sha256, author, restored_from, created_at) = row;
        if content.len() > super::files_guest::FILE_LIMIT
            || format!("{:x}", Sha256::digest(&content)) != sha256
        {
            return Err(Error::Conflict(
                "Project file history failed its integrity check",
            ));
        }
        Ok(Version {
            metadata: ProjectFileVersion {
                id,
                path: path.into(),
                sha256,
                byte_length: content.len() as u64,
                author: author.map(|v| serde_json::from_str(&v)).transpose()?,
                restored_from,
                created_at,
            },
            bytes: content,
        })
    }

    pub(super) fn file_history(
        &self,
        path: &str,
        before: Option<u64>,
    ) -> Result<Vec<ProjectFileVersion>, Error> {
        let mut query = self.conn.prepare(
            "SELECT id,sha256,length(content),author,restored_from,created_at,path FROM file_versions
             WHERE (?1='' OR path=?1) AND id<?2 ORDER BY id DESC LIMIT 100",
        )?;
        let rows = query.query_map(
            params![path, integer(before.unwrap_or(i64::MAX as u64))?],
            |r| {
                Ok((
                    unsigned(r, 0)?,
                    r.get::<_, String>(1)?,
                    unsigned(r, 2)?,
                    r.get::<_, Option<String>>(3)?,
                    optional_unsigned(r, 4)?,
                    unsigned(r, 5)?,
                    r.get::<_, String>(6)?,
                ))
            },
        )?;
        rows.map(|row| {
            let (id, sha256, byte_length, author, restored_from, created_at, path) = row?;
            Ok(ProjectFileVersion {
                id,
                path,
                sha256,
                byte_length,
                author: author.map(|v| serde_json::from_str(&v)).transpose()?,
                restored_from,
                created_at,
            })
        })
        .collect()
    }

    /// Reads retain externally observed changes without claiming an API writer.
    pub(super) fn observe_file(&self, path: &str, bytes: &[u8]) -> Result<Version, Error> {
        self.check_owner()?;
        if bytes.len() > super::files_guest::FILE_LIMIT {
            return Err(Error::Invalid("Files are limited to 1 MiB"));
        }
        let previous: Option<u64> = self
            .conn
            .query_row(
                "SELECT id FROM file_versions WHERE path=?1 ORDER BY id DESC LIMIT 1",
                [path],
                |r| unsigned(r, 0),
            )
            .optional()?;
        if let Some(id) = previous {
            let saved = self.file_version(path, id)?;
            if saved.bytes == bytes {
                return Ok(saved);
            }
        }
        self.file_quota(bytes.len(), 1)?;
        self.conn.execute(
            "INSERT INTO file_versions(path,content,sha256,created_at) VALUES(?1,?2,?3,?4)",
            params![path, bytes, format!("{:x}", Sha256::digest(bytes)), time()?],
        )?;
        self.file_version(path, self.conn.last_insert_rowid() as u64)
    }

    pub(super) fn file_operation(&self, id: Uuid) -> Result<Option<Operation>, Error> {
        let row = self.conn.query_row(
            "SELECT path,expected_version,COALESCE(content,(SELECT content FROM file_versions WHERE id=version_id)),
             author,restored_from,created_at,version_id,state FROM file_operations WHERE id=?1",
            [id.to_string()], |r| Ok((r.get::<_,String>(0)?,unsigned(r,1)?,r.get::<_,Vec<u8>>(2)?,
                r.get::<_,String>(3)?,optional_unsigned(r,4)?,unsigned(r,5)?,optional_unsigned(r,6)?,r.get::<_,String>(7)?)))
            .optional()?;
        row.map(
            |(path, expected, content, author, restored_from, created_at, version, state)| {
                Ok(Operation {
                    id,
                    path,
                    expected,
                    content,
                    author: serde_json::from_str(&author)?,
                    restored_from,
                    created_at,
                    version,
                    state,
                })
            },
        )
        .transpose()
    }

    pub(super) fn pending_file(&self, path: &str) -> Result<Option<Operation>, Error> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM file_operations WHERE path=?1 AND state='pending'",
                [path],
                |r| r.get(0),
            )
            .optional()?;
        id.map(|v| Uuid::parse_str(&v).map_err(std::io::Error::other))
            .transpose()?
            .map(|id| self.file_operation(id))
            .transpose()
            .map(Option::flatten)
    }

    pub(super) fn stage_file(&self, mut op: Operation) -> Result<Operation, Error> {
        self.check_owner()?;
        if op.content.len() > super::files_guest::FILE_LIMIT {
            return Err(Error::Invalid("Files are limited to 1 MiB"));
        }
        if self.pending_file(&op.path)?.is_some() {
            return Err(Error::Conflict(
                "Reconcile the existing pending file operation first",
            ));
        }
        if op.expected != 0 {
            self.file_version(&op.path, op.expected)?;
        }
        if let Some(id) = op.restored_from {
            self.file_version(&op.path, id)?;
        }
        self.file_quota(op.content.len(), 2)?;
        op.created_at = time()? as u64;
        self.conn.execute(
            "INSERT INTO file_operations(id,path,expected_version,content,author,restored_from,created_at,state)
             VALUES(?1,?2,?3,?4,?5,?6,?7,'pending')",
            params![op.id.to_string(),op.path,integer(op.expected)?,op.content,serde_json::to_string(&op.author)?,
                op.restored_from.map(integer).transpose()?,integer(op.created_at)?])?;
        Ok(op)
    }

    pub(super) fn finish_file(&self, id: Uuid) -> Result<Version, Error> {
        self.check_owner()?;
        let op = self.file_operation(id)?.ok_or(Error::Missing)?;
        if let Some(version) = op.version {
            return self.file_version(&op.path, version);
        }
        if op.state != "pending" {
            return Err(Error::Conflict(
                "The file operation was rejected; use a new operation identity",
            ));
        }
        let tx = self.conn.unchecked_transaction()?;
        self.conn.execute(
            "INSERT INTO file_versions(path,content,sha256,author,restored_from,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![op.path,op.content,format!("{:x}",Sha256::digest(&op.content)),serde_json::to_string(&op.author)?,
                op.restored_from.map(integer).transpose()?,integer(op.created_at)?])?;
        let version = self.conn.last_insert_rowid();
        self.conn.execute("UPDATE file_operations SET content=NULL,version_id=?1,state='applied' WHERE id=?2 AND state='pending'",
            params![version,id.to_string()])?;
        tx.commit()?;
        self.file_version(&op.path, version as u64)
    }

    pub(super) fn reject_file(&self, id: Uuid) -> Result<(), Error> {
        self.check_owner()?;
        self.conn.execute(
            "UPDATE file_operations SET state='failed' WHERE id=?1 AND state='pending'",
            [id.to_string()],
        )?;
        Ok(())
    }

    pub(super) fn file_operations(&self, path: &str) -> Result<Vec<ProjectFileOperation>, Error> {
        let mut query = self.conn.prepare("SELECT id,expected_version,author,version_id,state,path FROM file_operations WHERE (?1='' OR path=?1) ORDER BY created_at DESC,id LIMIT 100")?;
        let rows = query.query_map([path], |r| {
            Ok((
                r.get::<_, String>(0)?,
                unsigned(r, 1)?,
                r.get::<_, String>(2)?,
                optional_unsigned(r, 3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        rows.map(|row| {
            let (id, expected_version, author, version, state, path) = row?;
            Ok(ProjectFileOperation {
                id: Uuid::parse_str(&id).map_err(std::io::Error::other)?,
                path,
                expected_version,
                author: serde_json::from_str(&author)?,
                version,
                status: match state.as_str() {
                    "pending" => ProjectFileOperationStatus::Pending,
                    "applied" => ProjectFileOperationStatus::Applied,
                    _ => ProjectFileOperationStatus::Failed,
                },
            })
        })
        .collect()
    }
}

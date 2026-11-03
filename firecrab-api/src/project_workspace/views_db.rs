//! Private user/tab data uses optimistic revisions and durable mutation receipts.
use super::*;
use rusqlite::{OptionalExtension, params};

pub(super) const SCHEMA:&str="
CREATE TABLE private_views(principal TEXT NOT NULL,id TEXT NOT NULL,revision INTEGER NOT NULL,
 document TEXT NOT NULL,updated INTEGER NOT NULL,PRIMARY KEY(principal,id)) STRICT;
CREATE TABLE private_drafts(principal TEXT NOT NULL,view TEXT NOT NULL,path TEXT NOT NULL,
 revision INTEGER NOT NULL,base INTEGER NOT NULL,content BLOB,
 PRIMARY KEY(principal,view,path),FOREIGN KEY(principal,view) REFERENCES private_views(principal,id)) STRICT;
CREATE TABLE private_receipts(principal TEXT NOT NULL,id TEXT NOT NULL,resource TEXT NOT NULL,
 sha TEXT NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(principal,id)) STRICT;
";
/// Reject revision values outside SQLite’s signed integer range.
fn integer(n: u64) -> Result<i64, Error> {
    i64::try_from(n).map_err(|_| Error::Invalid("Invalid private document revision"))
}
/// Reject negative revisions recovered from private storage.
fn unsigned(n: i64) -> Result<u64, Error> {
    u64::try_from(n).map_err(|_| Error::Conflict("Invalid private document metadata"))
}
/// Derive the storage key exclusively from the verified transport caller.
fn principal(caller: Caller) -> Result<String, Error> {
    Ok(serde_json::to_string(&caller.principal)?)
}
/// Reject nil identities before using a view or mutation key.
fn uuid(value: Uuid) -> Result<String, Error> {
    if value.is_nil() {
        Err(Error::Invalid("Use a non-empty view or operation identity"))
    } else {
        Ok(value.to_string())
    }
}
/// Bind a mutation receipt to the exact logical request.
fn digest<T: Serialize>(value: &T) -> Result<String, Error> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

impl db::ProjectDb {
    /// Check retained content and metadata bounds before committing a mutation.
    fn private_quota(&self) -> Result<(), Error> {
        let (bytes, drafts, views): (i64, i64, i64) = self.conn.query_row(
            "SELECT
            (SELECT COALESCE(SUM(length(CAST(document AS BLOB))),0) FROM private_views)+
            (SELECT COALESCE(SUM(length(content)),0) FROM private_drafts),
            (SELECT COUNT(*) FROM private_drafts),(SELECT COUNT(*) FROM private_views)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if bytes > 128 * 1024 * 1024 || drafts > 4096 || views > 1024 {
            return Err(Error::Conflict(
                "Private workspace storage limit reached; preserve a checkpoint before maintenance",
            ));
        }
        Ok(())
    }
    /// Return an identical recent acknowledgement without overwriting newer data.
    fn private_receipt(
        &self,
        principal: &str,
        id: Uuid,
        resource: &str,
        sha: &str,
    ) -> Result<Option<ProjectPrivateSave>, Error> {
        self.check_owner()?;
        let row = self
            .conn
            .query_row(
                "SELECT resource,sha,revision FROM private_receipts WHERE principal=?1 AND id=?2",
                params![principal, uuid(id)?],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(original, hash, revision)| {
            if original != resource || hash != sha {
                return Err(Error::Conflict(
                    "Operation identity belongs to a different private save",
                ));
            }
            Ok(ProjectPrivateSave {
                revision: unsigned(revision)?,
            })
        })
        .transpose()
    }
    /// Record an acknowledgement and compact receipts, preserving resource revisions.
    fn private_finish(
        &self,
        principal: &str,
        id: Uuid,
        resource: &str,
        sha: &str,
        revision: i64,
    ) -> Result<ProjectPrivateSave, Error> {
        self.private_quota()?;
        self.conn.execute(
            "INSERT INTO private_receipts VALUES(?1,?2,?3,?4,?5)",
            params![principal, uuid(id)?, resource, sha, revision],
        )?;
        // Older retries still fail their monotonic resource revision after receipt compaction.
        // Keep tombstone revisions so an old create cannot revive a discarded buffer.
        self.conn.execute("DELETE FROM private_receipts WHERE rowid IN (SELECT rowid FROM private_receipts ORDER BY rowid DESC LIMIT -1 OFFSET 4096)",[])?;
        Ok(ProjectPrivateSave {
            revision: unsigned(revision)?,
        })
    }
    /// Return only the verified caller’s retained view metadata.
    pub(super) fn views(&self, caller: Caller) -> Result<Vec<ProjectViewSummary>, Error> {
        self.check_owner()?;
        let mut q=self.conn.prepare("SELECT id,revision,updated FROM private_views WHERE principal=?1 ORDER BY updated DESC,id LIMIT 64")?;
        let rows = q
            .query_map([principal(caller)?], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, revision, time)| {
                Ok(ProjectViewSummary {
                    id: Uuid::parse_str(&id)
                        .map_err(|_| Error::Conflict("Invalid view identity"))?,
                    revision: unsigned(revision)?,
                    updated_at: unsigned(time)?,
                })
            })
            .collect()
    }
    /// Read one private document under the current owner epoch.
    pub(super) fn view(&self, caller: Caller, view: Uuid) -> Result<ProjectView, Error> {
        self.check_owner()?;
        let row = self
            .conn
            .query_row(
                "SELECT revision,document,updated FROM private_views WHERE principal=?1 AND id=?2",
                params![principal(caller)?, uuid(view)?],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or(Error::Missing)?;
        if row.1.len() > db::DOCUMENT_LIMIT {
            return Err(Error::Conflict("Private view exceeds its limit"));
        }
        Ok(ProjectView {
            id: view,
            revision: unsigned(row.0)?,
            document: serde_json::from_str(&row.1)?,
            updated_at: unsigned(row.2)?,
        })
    }
    /// Apply a compare-and-swap layout save and durable retry receipt together.
    pub(super) fn save_view(
        &self,
        caller: Caller,
        view: Uuid,
        request: &SaveProjectView,
    ) -> Result<ProjectPrivateSave, Error> {
        self.check_owner()?;
        let owner = principal(caller)?;
        let view = uuid(view)?;
        let resource = format!("view:{view}");
        let sha = digest(request)?;
        if let Some(saved) = self.private_receipt(&owner, request.operation_id, &resource, &sha)? {
            return Ok(saved);
        }
        let document = serde_json::to_string(&request.document)?;
        if !request.document.is_object() || document.len() > db::DOCUMENT_LIMIT {
            return Err(Error::Invalid(
                "A private view must be an object of at most 64 KiB",
            ));
        }
        let expected = integer(request.expected_revision)?;
        let revision = expected.checked_add(1).ok_or(Error::Busy)?;
        let time = integer(terminals_db::now()?)?;
        let tx = self.conn.unchecked_transaction()?;
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM private_views WHERE principal=?1",
            [&owner],
            |r| r.get(0),
        )?;
        let changed = if expected == 0 {
            if count >= 64 {
                return Err(Error::Conflict(
                    "A user can retain at most 64 views per project",
                ));
            }
            self.conn.execute(
                "INSERT INTO private_views VALUES(?1,?2,1,?3,?4) ON CONFLICT DO NOTHING",
                params![owner, view, document, time],
            )?
        } else {
            self.conn.execute("UPDATE private_views SET revision=?3,document=?4,updated=?5 WHERE principal=?1 AND id=?2 AND revision=?6",
                params![owner,view,revision,document,time,expected])?
        };
        if changed != 1 {
            return Err(Error::Conflict(
                "This private view changed; reload before saving",
            ));
        }
        let result =
            self.private_finish(&owner, request.operation_id, &resource, &sha, revision)?;
        tx.commit()?;
        Ok(result)
    }
    /// Return metadata for this caller and tab’s active buffers.
    pub(super) fn drafts(
        &self,
        caller: Caller,
        view: Uuid,
    ) -> Result<Vec<ProjectDraftSummary>, Error> {
        self.view(caller, view)?;
        let mut q=self.conn.prepare("SELECT path,revision,base,length(content) FROM private_drafts WHERE principal=?1 AND view=?2 AND content IS NOT NULL ORDER BY path LIMIT 33")?;
        let rows = q
            .query_map(params![principal(caller)?, uuid(view)?], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.len() > 32 {
            return Err(Error::Conflict("Private draft limit exceeded"));
        }
        rows.into_iter()
            .map(|(path, revision, base, bytes)| {
                Ok(ProjectDraftSummary {
                    path,
                    revision: unsigned(revision)?,
                    base_version: unsigned(base)?,
                    byte_length: unsigned(bytes)?,
                })
            })
            .collect()
    }
    /// Return bytes or a discard tombstone with its monotonic revision.
    pub(super) fn draft(
        &self,
        caller: Caller,
        view: Uuid,
        path: &str,
    ) -> Result<ProjectDraft, Error> {
        self.view(caller, view)?;
        files_guest::path(path, false)?;
        let row=self.conn.query_row("SELECT revision,base,content FROM private_drafts WHERE principal=?1 AND view=?2 AND path=?3",
            params![principal(caller)?,uuid(view)?,path],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,Option<Vec<u8>>>(2)?))).optional()?.ok_or(Error::Missing)?;
        if row.2.as_ref().is_some_and(|v| v.len() > 1024 * 1024) {
            return Err(Error::Conflict("Private draft exceeds its limit"));
        }
        Ok(ProjectDraft {
            path: path.into(),
            revision: unsigned(row.0)?,
            base_version: unsigned(row.1)?,
            content_base64: row.2.map(|v| STANDARD.encode(v)),
        })
    }
    /// Atomically save or discard one private buffer at its expected revision.
    pub(super) fn save_draft(
        &self,
        caller: Caller,
        view: Uuid,
        path: &str,
        request: &SaveProjectDraft,
        delete: bool,
    ) -> Result<ProjectPrivateSave, Error> {
        self.view(caller, view)?;
        files_guest::path(path, false)?;
        let owner = principal(caller)?;
        let view = uuid(view)?;
        let resource = format!("draft:{view}:{path}");
        let sha = digest(&(request, delete))?;
        if let Some(saved) = self.private_receipt(&owner, request.operation_id, &resource, &sha)? {
            return Ok(saved);
        }
        if request.content_base64.len() > 1398104 {
            return Err(Error::Invalid("Private drafts are limited to 1 MiB"));
        }
        let bytes = STANDARD
            .decode(&request.content_base64)
            .map_err(|_| Error::Invalid("Invalid draft content encoding"))?;
        if bytes.len() > 1024 * 1024 {
            return Err(Error::Invalid("Private drafts are limited to 1 MiB"));
        }
        if !delete && request.base_version != 0 {
            self.file_version(path, request.base_version)?;
        }
        let expected = integer(request.expected_revision)?;
        let tx = self.conn.unchecked_transaction()?;
        let current:Option<(i64,bool)>=self.conn.query_row("SELECT revision,content IS NOT NULL FROM private_drafts WHERE principal=?1 AND view=?2 AND path=?3",params![owner,view,path],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let previous = current.map_or(0, |v| v.0);
        if previous != expected {
            return Err(Error::Conflict(
                "This private draft changed; preserve both versions before saving",
            ));
        }
        let revision = previous.checked_add(1).ok_or(Error::Busy)?;
        let count:i64=self.conn.query_row("SELECT COUNT(*) FROM private_drafts WHERE principal=?1 AND view=?2 AND content IS NOT NULL",params![owner,view],|r|r.get(0))?;
        if !delete && count >= 32 && !current.is_some_and(|row| row.1) {
            return Err(Error::Conflict("A view can retain at most 32 open drafts"));
        }
        self.conn.execute("INSERT INTO private_drafts VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(principal,view,path) DO UPDATE SET revision=excluded.revision,base=excluded.base,content=excluded.content",
            params![owner,view,path,revision,integer(request.base_version)?,(!delete).then_some(bytes)])?;
        let result =
            self.private_finish(&owner, request.operation_id, &resource, &sha, revision)?;
        tx.commit()?;
        Ok(result)
    }
}

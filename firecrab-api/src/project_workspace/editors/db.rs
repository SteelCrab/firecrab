//! Editor ownership is private; native attempts are reserved against public PTY aliasing.
use super::*;
use rusqlite::{OptionalExtension, params};
pub(crate) const SCHEMA:&str="
CREATE TABLE private_editors(id TEXT PRIMARY KEY,principal TEXT NOT NULL,view TEXT NOT NULL,record TEXT NOT NULL,
 UNIQUE(principal,view),FOREIGN KEY(principal,view) REFERENCES private_views(principal,id)) STRICT;
CREATE TABLE private_editor_attempts(id TEXT PRIMARY KEY,editor TEXT NOT NULL REFERENCES private_editors(id)) STRICT;
";
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Editor {
    /// Stable resource identity.
    pub id: Uuid,
    /// Private browser tab identity.
    pub view: Uuid,
    /// Verified creator captured at admission.
    pub caller: Caller,
    /// Authorized workspace-relative editor root.
    pub directory: String,
    /// Last explicitly requested workspace-relative file.
    pub file: Option<String>,
    /// Stable identity for the latest file-opening request.
    pub open_request: Uuid,
    /// VM and runtime identities captured at admission.
    pub runtime: guest::Binding,
    /// Retained workspace binding generation.
    pub generation: u64,
    /// Reserved native execution scope; never exposed as a public terminal.
    pub attempt: Uuid,
    /// Confirmed native kernel and process identity.
    pub proof: Option<terminals_db::Proof>,
    /// Guest editor authentication secret; retained only in private controller storage.
    pub token: String,
    /// Validated dashboard origin permitted to frame this editor.
    pub parent_origin: String,
    /// Confirmed lifecycle state; recovery never replays a launch.
    pub status: String,
}
/// Key private editor ownership by the verified transport principal.
fn principal(caller: Caller) -> Result<String, Error> {
    Ok(serde_json::to_string(&caller.principal)?)
}
impl super::super::db::ProjectDb {
    /// Read an editor by physical identity or by this caller’s private view.
    pub(super) fn editor(&self, caller: Caller, id: Uuid, view: bool) -> Result<Editor, Error> {
        self.check_owner()?;
        let sql = if view {
            "SELECT record FROM private_editors WHERE principal=?1 AND view=?2"
        } else {
            "SELECT record FROM private_editors WHERE principal=?1 AND id=?2"
        };
        let raw: Option<String> = self
            .conn
            .query_row(sql, params![principal(caller)?, id.to_string()], |r| {
                r.get(0)
            })
            .optional()?;
        let raw = raw.ok_or(Error::Missing)?;
        if raw.len() > 16384 {
            return Err(Error::Conflict("Invalid private editor metadata"));
        }
        let editor: Editor = serde_json::from_str(&raw)?;
        if editor.caller.principal != caller.principal
            || if view {
                editor.view != id
            } else {
                editor.id != id
            }
        {
            return Err(Error::Denied);
        }
        Ok(editor)
    }
    /// Commit a private editor record and reserve its native attempt identity.
    pub(super) fn save_editor(&self, e: &Editor) -> Result<(), Error> {
        self.view(e.caller, e.view)?;
        let raw = serde_json::to_string(e)?;
        if raw.len() > 16384 {
            return Err(Error::Invalid("Editor metadata exceeds its limit"));
        }
        let tx = self.conn.unchecked_transaction()?;
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM private_editors", [], |r| r.get(0))?;
        if count >= 16 && self.editor(e.caller, e.id, false).is_err() {
            return Err(Error::Conflict(
                "A project can retain at most 16 private editors",
            ));
        }
        if self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM terminal_sessions WHERE id=?1)",
            [e.attempt.to_string()],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(Error::Conflict(
                "Editor attempt belongs to a public terminal",
            ));
        }
        let changed=self.conn.execute("INSERT INTO private_editors VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET record=excluded.record WHERE principal=excluded.principal AND view=excluded.view",
            params![e.id.to_string(),principal(e.caller)?,e.view.to_string(),raw])?;
        if changed != 1 {
            return Err(Error::Denied);
        }
        let attempts: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM private_editor_attempts", [], |r| {
                    r.get(0)
                })?;
        if attempts >= 4096 && !self.private_attempt(e.attempt)? {
            return Err(Error::Conflict(
                "Private editor attempt retention limit reached",
            ));
        }
        self.conn.execute(
            "INSERT INTO private_editor_attempts VALUES(?1,?2) ON CONFLICT(id) DO NOTHING",
            params![e.attempt.to_string(), e.id.to_string()],
        )?;
        let owner: String = self.conn.query_row(
            "SELECT editor FROM private_editor_attempts WHERE id=?1",
            [e.attempt.to_string()],
            |r| r.get(0),
        )?;
        if owner != e.id.to_string() {
            return Err(Error::Conflict("Editor attempt belongs to another editor"));
        }
        tx.commit()?;
        Ok(())
    }
    /// Load bounded records for trusted recovery without exposing tokens through an API.
    pub(super) fn editors(&self) -> Result<Vec<Editor>, Error> {
        self.check_owner()?;
        let mut q = self
            .conn
            .prepare("SELECT record FROM private_editors ORDER BY id LIMIT 17")?;
        let records = q
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if records.len() > 16 {
            return Err(Error::Conflict("Private editor limit exceeded"));
        }
        records
            .into_iter()
            .map(|s| {
                if s.len() > 16384 {
                    return Err(Error::Conflict("Invalid private editor metadata"));
                }
                Ok(serde_json::from_str(&s)?)
            })
            .collect()
    }
    /// Check the private namespace before admitting a public PTY UUID.
    pub(super) fn private_attempt(&self, id: Uuid) -> Result<bool, Error> {
        self.check_owner()?;
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM private_editor_attempts WHERE id=?1)",
            [id.to_string()],
            |r| r.get(0),
        )?)
    }
}

impl std::fmt::Debug for Editor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrivateEditor")
            .field("id", &self.id)
            .field("view", &self.view)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

//! Immutable close snapshots retain accepted bytes even if another client later changes its draft.
use super::{clients::*, *};
use rusqlite::{OptionalExtension, params};
pub(crate) const SCHEMA:&str="
CREATE TABLE private_editor_clients(id TEXT PRIMARY KEY,editor TEXT NOT NULL REFERENCES private_editors(id),principal TEXT NOT NULL,document TEXT NOT NULL CHECK(length(CAST(document AS BLOB))<=16384)) STRICT;
CREATE TABLE private_editor_snapshots(client TEXT NOT NULL REFERENCES private_editor_clients(id),revision INTEGER NOT NULL CHECK(revision>0),request_sha TEXT NOT NULL,document TEXT NOT NULL,PRIMARY KEY(client,revision)) STRICT;
";
fn principal(e: &Editor) -> Result<String, Error> {
    Ok(serde_json::to_string(&e.caller.principal)?)
}
impl super::super::db::ProjectDb {
    pub(super) fn editor_client(&self, e: &Editor, id: Uuid) -> Result<Client, Error> {
        self.check_owner()?;
        if id.is_nil() {
            return Err(Error::Invalid("Use a non-empty browser client identity"));
        }
        let raw:Option<String>=self.conn.query_row("SELECT document FROM private_editor_clients WHERE id=?1 AND editor=?2 AND principal=?3",params![id.to_string(),e.id.to_string(),principal(e)?],|r|r.get(0)).optional()?;
        let raw = raw.ok_or(Error::Missing)?;
        if raw.len() > 16384 {
            return Err(Error::Conflict("Invalid retained browser client"));
        }
        let c: Client = serde_json::from_str(&raw)?;
        if c.id != id
            || c.editor != e.id
            || !canonical_sha(&c.instance)
            || c.revision > i64::MAX as u64
        {
            return Err(Error::Conflict("Retained browser client identity changed"));
        }
        Ok(c)
    }
    fn store_editor_client(&self, e: &Editor, c: &Client) -> Result<(), Error> {
        self.check_owner()?;
        let raw = serde_json::to_string(c)?;
        if raw.len() > 16384 {
            return Err(Error::Invalid("Browser client metadata exceeds its limit"));
        }
        let changed = self.conn.execute("UPDATE private_editor_clients SET document=?2 WHERE id=?1 AND editor=?3 AND principal=?4",params![c.id.to_string(),raw,e.id.to_string(),principal(e)?])?;
        if changed != 1 {
            return Err(Error::Missing);
        }
        Ok(())
    }
    pub(super) fn register_editor_client(
        &self,
        e: &Editor,
        id: Uuid,
        expected: &str,
    ) -> Result<Client, Error> {
        self.check_owner()?;
        if id.is_nil() || !canonical_sha(expected) {
            return Err(Error::Invalid("Invalid browser client identity"));
        }
        match self.editor_client(e, id) {
            Ok(c) if c.instance == expected => return Ok(c),
            Ok(_) => return Err(Error::Conflict("Browser client execution changed")),
            Err(Error::Missing) => {}
            Err(error) => return Err(error),
        }
        if e.browser_protocol != Some(1) || e.status != "ready" || instance(e) != expected {
            return Err(Error::Conflict(
                "This editor does not support the browser close protocol",
            ));
        }
        let count: i64 =
            self.conn
                .query_row("SELECT count(*) FROM private_editor_clients", [], |r| {
                    r.get(0)
                })?;
        if count >= 4096 {
            return Err(Error::Conflict("Browser client retention quota reached"));
        }
        let c = Client {
            id,
            editor: e.id,
            instance: expected.into(),
            activations: vec![],
            window_proof: None,
            revision: 0,
            command: "active".into(),
            status: "opening".into(),
            operation: None,
            operation_sha: None,
            error: None,
        };
        let changed = self.conn.execute(
            "INSERT INTO private_editor_clients VALUES(?1,?2,?3,?4) ON CONFLICT DO NOTHING",
            params![
                id.to_string(),
                e.id.to_string(),
                principal(e)?,
                serde_json::to_string(&c)?
            ],
        )?;
        if changed != 1 {
            return Err(Error::Missing);
        }
        Ok(c)
    }
    /// Each workbench document receives a distinct proof; copying its URL cannot impersonate its parent-owned window.
    pub(super) fn activate_editor_client(
        &self,
        e: &Editor,
        id: Uuid,
        proof: Uuid,
    ) -> Result<(), Error> {
        self.check_owner()?;
        let mut c = self.editor_client(e, id)?;
        if c.instance != instance(e) || c.command == "detach" || proof.is_nil() {
            return Err(Error::Conflict("The browser editor execution ended"));
        }
        if c.activations.contains(&proof) {
            return Ok(());
        }
        if c.activations.len() >= 16 {
            return Err(Error::Conflict(
                "Browser activation quota reached; reopen from a new canvas client",
            ));
        }
        c.activations.push(proof);
        self.store_editor_client(e, &c)
    }
    pub(super) fn control_editor_client(
        &self,
        e: &Editor,
        id: Uuid,
        request: &Control,
    ) -> Result<Client, Error> {
        self.check_owner()?;
        if request.operation_id.is_nil()
            || request.window_proof.is_nil()
            || !matches!(request.action.as_str(), "close" | "cancel" | "detach")
        {
            return Err(Error::Invalid("Invalid browser close command"));
        }
        let sha = digest(request)?;
        let mut c = self.editor_client(e, id)?;
        if !c.activations.contains(&request.window_proof) {
            return Err(Error::Conflict(
                "The browser window proof was not registered by its workbench",
            ));
        }
        if c.operation == Some(request.operation_id) {
            return if c.operation_sha.as_deref() == Some(sha.as_str()) {
                Ok(c)
            } else {
                Err(Error::Conflict("Browser close operation identity changed"))
            };
        }
        if c.revision != request.expected_revision {
            return Err(Error::Conflict(
                "Browser close state changed; read its current revision",
            ));
        }
        if request.action == "detach"
            && (c.status != "ready" || c.window_proof != Some(request.window_proof))
        {
            return Err(Error::Conflict(
                "The private draft snapshot is not confirmed; keep the browser client",
            ));
        }
        if request.action == "detach" {
            self.closed_editor_snapshot(e, id, c.revision)?;
        }
        c.revision = c
            .revision
            .checked_add(1)
            .filter(|r| *r <= i64::MAX as u64)
            .ok_or(Error::Busy)?;
        c.window_proof = Some(request.window_proof);
        c.command = request.action.clone();
        c.operation = Some(request.operation_id);
        c.operation_sha = Some(sha);
        c.error = None;
        c.status = match request.action.as_str() {
            "close" => "flushing",
            "cancel" => "resuming",
            _ => "detached",
        }
        .into();
        self.store_editor_client(e, &c)?;
        Ok(c)
    }
    pub(super) fn report_editor_client(
        &self,
        e: &Editor,
        id: Uuid,
        request: &Report,
    ) -> Result<Client, Error> {
        self.check_owner()?;
        let mut c = self.editor_client(e, id)?;
        let revision = match request {
            Report::Active { revision, .. }
            | Report::Error { revision, .. }
            | Report::Flushed { revision, .. } => *revision,
        };
        let proof = match request {
            Report::Active { window_proof, .. }
            | Report::Error { window_proof, .. }
            | Report::Flushed { window_proof, .. } => *window_proof,
        };
        if !c.activations.contains(&proof) {
            return Err(Error::Conflict("Unregistered browser window report"));
        }
        let sha = digest(request)?;
        if matches!(request, Report::Flushed { .. }) {
            match self.closed_editor_snapshot(e, id, revision) {
                Ok((_, original)) if original == sha => return Ok(c),
                Ok(_) => return Err(Error::Conflict("The accepted close snapshot is immutable")),
                Err(Error::Missing) => {}
                Err(error) => return Err(error),
            }
        }
        if c.revision != revision || c.window_proof.is_some_and(|p| p != proof) {
            return Err(Error::Conflict("Browser close command or window changed"));
        }
        match request {
            Report::Active { .. } if matches!(c.command.as_str(), "active" | "cancel") => {
                c.status = "active".into();
                c.error = None;
                self.store_editor_client(e, &c)?;
            }
            Report::Error { .. } if c.status == "ready" => return Ok(c),
            Report::Error { message, .. } if c.command == "close" => {
                if message.len() > 512 {
                    return Err(Error::Invalid("Browser close error is too long"));
                }
                c.status = "error".into();
                c.error = Some(message.clone());
                self.store_editor_client(e, &c)?;
            }
            Report::Flushed {
                files,
                active_file,
                open_files,
                ..
            } if c.command == "close" => {
                if files.len() > 64 || open_files.len() > 32 {
                    return Err(Error::Invalid(
                        "Close snapshots support at most 32 open files and 64 retained paths",
                    ));
                }
                let mut paths = std::collections::HashSet::new();
                let mut retained = Vec::new();
                for file in files {
                    files_guest::path(&file.path, false)?;
                    if !paths.insert(&file.path) {
                        return Err(Error::Invalid("Duplicate file in browser close snapshot"));
                    }
                    let draft = match self.draft(e.caller, e.view, &file.path) {
                        Ok(d) => d,
                        Err(Error::Missing) => ProjectDraft {
                            path: file.path.clone(),
                            revision: 0,
                            base_version: 0,
                            content_base64: None,
                        },
                        Err(error) => return Err(error),
                    };
                    let hash = draft
                        .content_base64
                        .as_ref()
                        .map(|s| {
                            STANDARD
                                .decode(s)
                                .map(|b| format!("{:x}", Sha256::digest(b)))
                        })
                        .transpose()
                        .map_err(|_| Error::Conflict("Invalid retained private draft"))?;
                    if draft.revision != file.revision || hash != file.sha256 {
                        return Err(Error::Conflict(
                            "Private draft acknowledgement does not match the retained bytes",
                        ));
                    }
                    retained.push(ClosedFile {
                        path: draft.path,
                        revision: draft.revision,
                        base_version: draft.base_version,
                        content_base64: draft.content_base64,
                    });
                }
                let opened: std::collections::HashSet<_> = open_files.iter().collect();
                if opened.len() != open_files.len()
                    || open_files
                        .iter()
                        .any(|p| files_guest::path(p, false).is_err())
                {
                    return Err(Error::Invalid("Open files must be unique workspace paths"));
                }
                if active_file.as_ref().is_some_and(|p| !opened.contains(p)) {
                    return Err(Error::Invalid(
                        "Active file must be included in the close snapshot",
                    ));
                }
                let mut snapshot = Snapshot {
                    client: id,
                    revision,
                    window_proof: proof,
                    files: retained,
                    active_file: active_file.clone(),
                    open_files: open_files.clone(),
                    created_at: terminals_db::now()?,
                    sha256: String::new(),
                };
                snapshot.sha256 = snapshot_sha(&snapshot)?;
                let raw = serde_json::to_string(&snapshot)?;
                let (count,bytes):(i64,i64)=self.conn.query_row("SELECT count(*),coalesce(sum(length(CAST(document AS BLOB))),0) FROM private_editor_snapshots",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
                if raw.len() > 48 * 1024 * 1024
                    || count >= 1024
                    || bytes.saturating_add(raw.len() as i64) > 128 * 1024 * 1024
                {
                    return Err(Error::Conflict(
                        "Private close snapshot quota reached; retain the browser and export recovery data",
                    ));
                }
                let tx = self.conn.unchecked_transaction()?;
                self.conn.execute(
                    "INSERT INTO private_editor_snapshots VALUES(?1,?2,?3,?4)",
                    params![id.to_string(), revision as i64, sha, raw],
                )?;
                c.status = "ready".into();
                c.error = None;
                self.store_editor_client(e, &c)?;
                tx.commit()?;
            }
            _ => {
                return Err(Error::Conflict(
                    "This browser report does not match the current close command",
                ));
            }
        }
        Ok(c)
    }
    pub(super) fn closed_editor_snapshot(
        &self,
        e: &Editor,
        id: Uuid,
        revision: u64,
    ) -> Result<(Snapshot, String), Error> {
        self.editor_client(e, id)?;
        if revision > i64::MAX as u64 {
            return Err(Error::Invalid("Invalid close snapshot revision"));
        }
        let row:Option<(String,String)>=self.conn.query_row("SELECT document,request_sha FROM private_editor_snapshots WHERE client=?1 AND revision=?2",params![id.to_string(),revision as i64],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let (raw, request) = row.ok_or(Error::Missing)?;
        if raw.len() > 48 * 1024 * 1024 {
            return Err(Error::Conflict("Close snapshot exceeds its limit"));
        }
        let s: Snapshot = serde_json::from_str(&raw)?;
        if s.client != id
            || s.revision != revision
            || s.sha256 != snapshot_sha(&s)?
            || s.files.len() > 64
            || s.open_files.len() > 32
        {
            return Err(Error::Conflict(
                "Retained close snapshot identity or checksum changed",
            ));
        }
        Ok((s, request))
    }
    pub(super) fn closed_editor_snapshots(
        &self,
        e: &Editor,
    ) -> Result<Vec<serde_json::Value>, Error> {
        self.check_owner()?;
        let mut q=self.conn.prepare("SELECT s.client,s.revision,length(CAST(s.document AS BLOB)),json_extract(s.document,'$.activeFile'),json_extract(s.document,'$.createdAt') FROM private_editor_snapshots s JOIN private_editor_clients c ON c.id=s.client WHERE c.editor=?1 AND c.principal=?2 ORDER BY s.rowid DESC LIMIT 32")?;
        let rows=q.query_map(params![e.id.to_string(),principal(e)?],|r|Ok(serde_json::json!({"client":r.get::<_,String>(0)?,"revision":r.get::<_,i64>(1)?,"bytes":r.get::<_,i64>(2)?,"activeFile":r.get::<_,Option<String>>(3)?,"createdAt":r.get::<_,i64>(4)?})))?.collect::<Result<Vec<_>,_>>()?;
        Ok(rows)
    }
}

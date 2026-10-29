//! Canvas collaboration messages remain separate from Sandbox task records.
use super::{PersistenceError, Store};
use firecrab_api_types::{SandboxCanvasMessage, SandboxPrincipal};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS canvas_messages (
    project_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    id TEXT NOT NULL,
    author TEXT NOT NULL,
    text TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(project_id, sequence),
    UNIQUE(project_id, id)
) STRICT;
CREATE TABLE IF NOT EXISTS canvas_message_sequences (
    project_id TEXT PRIMARY KEY,
    last_sequence INTEGER NOT NULL
) STRICT;
";

const MAX_MESSAGES: i64 = 10_000;
const PAGE_SIZE: i64 = 200;

/// A repeated message identity must describe the same immutable message.
pub(crate) enum SaveMessage {
    Saved(SandboxCanvasMessage),
    Conflict,
    Full,
}

fn corrupt() -> PersistenceError {
    PersistenceError::CorruptRecord {
        id: "canvas_message".into(),
        reason: "invalid Canvas message".into(),
    }
}

fn message(
    project_id: Uuid,
    id: String,
    author: String,
    text: String,
    sequence: i64,
    created_at: i64,
) -> Result<SandboxCanvasMessage, PersistenceError> {
    Ok(SandboxCanvasMessage {
        id: Uuid::parse_str(&id).map_err(|_| corrupt())?,
        project_id,
        author: serde_json::from_str(&author).map_err(|_| corrupt())?,
        text,
        sequence: u64::try_from(sequence).map_err(|_| corrupt())?,
        created_at: u64::try_from(created_at).map_err(|_| corrupt())?,
    })
}

impl Store {
    /// Append once, or return the already persisted message after an identical retry.
    pub(crate) fn save_canvas_message(
        &self,
        project_id: Uuid,
        id: Uuid,
        author: SandboxPrincipal,
        text: &str,
        created_at: u64,
    ) -> Result<SaveMessage, PersistenceError> {
        let author = serde_json::to_string(&author).map_err(|_| corrupt())?;
        let created_at = i64::try_from(created_at).map_err(|_| corrupt())?;
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let existing: Option<(String, String, i64, i64)> = tx.query_row(
            "SELECT author,text,sequence,created_at FROM canvas_messages WHERE project_id=?1 AND id=?2",
            params![project_id.to_string(), id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).optional()?;
        if let Some((old_author, old_text, sequence, old_created_at)) = existing {
            if old_author != author || old_text != text {
                return Ok(SaveMessage::Conflict);
            }
            return Ok(SaveMessage::Saved(message(
                project_id,
                id.to_string(),
                old_author,
                old_text,
                sequence,
                old_created_at,
            )?));
        }
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM canvas_messages WHERE project_id=?1",
            [project_id.to_string()],
            |row| row.get(0),
        )?;
        if count >= MAX_MESSAGES {
            return Ok(SaveMessage::Full);
        }
        // Keep only the counter after project deletion so a recreated project
        // never reuses an old browser cursor. Other projects have no influence.
        let sequence: i64 = tx.query_row(
            "INSERT INTO canvas_message_sequences(project_id,last_sequence) VALUES(?1,1) ON CONFLICT(project_id) DO UPDATE SET last_sequence=last_sequence+1 RETURNING last_sequence",
            [project_id.to_string()],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO canvas_messages(project_id,sequence,id,author,text,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![project_id.to_string(), sequence, id.to_string(), author, text, created_at],
        )?;
        tx.commit()?;
        Ok(SaveMessage::Saved(message(
            project_id,
            id.to_string(),
            author,
            text.to_owned(),
            sequence,
            created_at,
        )?))
    }

    /// Read a bounded, ordered page after a previously observed cursor.
    pub(crate) fn canvas_messages(
        &self,
        project_id: Uuid,
        after: u64,
    ) -> Result<Vec<SandboxCanvasMessage>, PersistenceError> {
        let after = i64::try_from(after).map_err(|_| corrupt())?;
        let conn = self.lock();
        let mut query = conn.prepare(
            "SELECT id,author,text,sequence,created_at FROM canvas_messages WHERE project_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3",
        )?;
        let rows = query.query_map(params![project_id.to_string(), after, PAGE_SIZE], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?;
        rows.map(|row| {
            let (id, author, text, sequence, created_at) = row?;
            message(project_id, id, author, text, sequence, created_at)
        })
        .collect()
    }
}

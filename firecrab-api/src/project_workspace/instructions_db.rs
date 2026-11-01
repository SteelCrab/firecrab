//! Append-only caller-private saved-rule context in the project SQLite database.
use super::*;
use rusqlite::{OptionalExtension, params};
/// Additive schema; no instruction capture modifies a shared file or private editor buffer.
pub(super) const SCHEMA: &str = "CREATE TABLE instruction_snapshots (id TEXT PRIMARY KEY, principal TEXT NOT NULL, directory TEXT NOT NULL, document TEXT NOT NULL CHECK(length(CAST(document AS BLOB))<=262144)) STRICT;";
impl db::ProjectDb {
    /// Return only the verified caller's immutable saved context.
    pub(super) fn instruction_snapshot(
        &self,
        caller: Caller,
        id: Uuid,
    ) -> Result<ProjectInstructionSnapshot, Error> {
        let principal = serde_json::to_string(&caller.principal)
            .map_err(|_| Error::Invalid("Invalid snapshot caller"))?;
        let value: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT directory,document FROM instruction_snapshots WHERE id=?1 AND principal=?2",
                params![id.to_string(), principal],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (directory, document) = value.ok_or(Error::Missing)?;
        let snapshot: ProjectInstructionSnapshot = serde_json::from_str(&document)
            .map_err(|_| Error::Conflict("Retained instruction snapshot is invalid"))?;
        if snapshot.id != id || snapshot.directory != directory {
            return Err(Error::Conflict(
                "Retained instruction snapshot identity changed",
            ));
        }
        instructions::validate(&snapshot)?;
        Ok(snapshot)
    }
    /// Commit an exact history-backed context once; never overwrite another request or caller.
    pub(super) fn save_instruction_snapshot(
        &self,
        caller: Caller,
        snapshot: &ProjectInstructionSnapshot,
    ) -> Result<(), Error> {
        instructions::validate(snapshot)?;
        match self.instruction_snapshot(caller, snapshot.id) {
            Ok(existing) if existing == *snapshot => return Ok(()),
            Ok(_) => return Err(Error::Conflict("Instruction snapshot is immutable")),
            Err(Error::Missing) => {}
            Err(error) => return Err(error),
        }
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM instruction_snapshots WHERE id=?1)",
            [snapshot.id.to_string()],
            |row| row.get(0),
        )?;
        if exists {
            return Err(Error::Missing);
        }
        for file in &snapshot.files {
            let saved = self.file_version(&file.path, file.version)?;
            if saved.bytes != file.content.as_bytes() || saved.metadata.sha256 != file.sha256 {
                return Err(Error::Conflict(
                    "Instruction snapshot does not match retained saved files",
                ));
            }
        }
        let value = serde_json::to_string(snapshot)
            .map_err(|_| Error::Invalid("Invalid instruction snapshot"))?;
        let (count, bytes): (i64, i64) = self.conn.query_row("SELECT count(*),coalesce(sum(length(CAST(document AS BLOB))),0) FROM instruction_snapshots", [], |row| Ok((row.get(0)?,row.get(1)?)))?;
        if value.len() > 256 * 1024
            || count >= 1024
            || bytes.saturating_add(value.len() as i64) > 64 * 1024 * 1024
        {
            return Err(Error::Conflict(
                "Instruction snapshot storage quota reached",
            ));
        }
        let principal = serde_json::to_string(&caller.principal)
            .map_err(|_| Error::Invalid("Invalid snapshot caller"))?;
        self.conn.execute("INSERT INTO instruction_snapshots(id,principal,directory,document) VALUES(?1,?2,?3,?4)", params![snapshot.id.to_string(),principal,snapshot.directory,value])?;
        Ok(())
    }
}

//! Private snapshots prevent provider checks or token refreshes from reviving stale accounts.
use super::{PersistenceError, Store, code_workspaces};
use crate::{pipe_ai::SavedConnection, pipe_auth::SavedAuth};
use firecrab_api_types::CodeWorkspace;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

/// No Debug or Serialize: only the workspace projection can cross the API boundary.
#[derive(Clone)]
pub(crate) struct Snapshot {
    pub workspace: CodeWorkspace,
    pub github: Option<SavedAuth>,
    pub ai: Option<SavedConnection>,
    github_json: Option<String>,
    ai_json: Option<String>,
}

fn decode<T: serde::de::DeserializeOwned>(
    raw: &Option<String>,
) -> Result<Option<T>, PersistenceError> {
    raw.as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| PersistenceError::CorruptRecord {
            id: "code_workspace".into(),
            reason: "invalid account reference".into(),
        })
}

impl Store {
    /// Load configuration and just its referenced private accounts in one read transaction.
    pub(crate) fn code_verification_snapshot(
        &self,
        id: Uuid,
    ) -> Result<Option<Snapshot>, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let Some(workspace) = code_workspaces::read(&tx, id)? else {
            return Ok(None);
        };
        let github_json = tx
            .query_row(
                "SELECT credential FROM pipe_auth_credentials WHERE provider='github'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let ai_json = workspace
            .spec
            .agent
            .as_ref()
            .map(|binding| {
                tx.query_row(
                    "SELECT spec FROM pipe_ai_connections WHERE id=?1",
                    [binding.connection_id.to_string()],
                    |row| row.get(0),
                )
                .optional()
            })
            .transpose()?
            .flatten();
        Ok(Some(Snapshot {
            github: decode(&github_json)?,
            ai: decode(&ai_json)?,
            workspace,
            github_json,
            ai_json,
        }))
    }

    /// Check again after network IO. Identical account IDs with replaced tokens still invalidate it.
    pub(crate) fn code_verification_current(
        &self,
        snapshot: &Snapshot,
    ) -> Result<bool, PersistenceError> {
        Ok(self
            .code_verification_snapshot(snapshot.workspace.id)?
            .is_some_and(|current| {
                current.workspace == snapshot.workspace
                    && current.github_json == snapshot.github_json
                    && current.ai_json == snapshot.ai_json
            }))
    }

    /// Refresh only the exact account read before IO; never insert a disconnected account.
    /// Refresh keeps provider, registration, models and defaults unchanged.
    pub(crate) fn refresh_code_ai(
        &self,
        snapshot: &mut Snapshot,
        refreshed: SavedConnection,
    ) -> Result<bool, PersistenceError> {
        let raw =
            serde_json::to_string(&refreshed).map_err(|_| PersistenceError::CorruptRecord {
                id: "code_workspace".into(),
                reason: "invalid refreshed account".into(),
            })?;
        let changed = self.lock().execute(
            "UPDATE pipe_ai_connections SET spec=?1 WHERE id=?2 AND spec=?3",
            params![raw, refreshed.connection.id.to_string(), snapshot.ai_json],
        )?;
        if changed != 1 {
            return Ok(false);
        }
        snapshot.ai_json = Some(raw);
        snapshot.ai = Some(refreshed);
        Ok(true)
    }
}

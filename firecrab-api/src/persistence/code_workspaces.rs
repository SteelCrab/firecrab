//! Workspace metadata and account-reference checks share the private control database.
use super::{PersistenceError, Store};
use firecrab_api_types::{
    AiConnection, CodeConnectionState as Binding, CodeWorkspace, CodeWorkspaceConnections,
    CodeWorkspaceSpec, CodeWorkspaceWrite,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::collections::BTreeMap;
use uuid::Uuid;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS code_workspaces (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL COLLATE NOCASE UNIQUE,
    spec TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
    updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= created_at_ms)
) STRICT;";

/// Expected write outcomes, independent of database failures.
pub(crate) enum WriteResult {
    Written(Box<CodeWorkspace>),
    Conflict,
    NameTaken,
    Invalid(BTreeMap<String, String>),
}

/// A stale deletion cannot remove a newer configuration.
pub(crate) enum DeleteResult {
    Deleted,
    Missing,
    Conflict,
}

fn corrupt() -> PersistenceError {
    PersistenceError::CorruptRecord {
        id: "code_workspace".into(),
        reason: "invalid workspace metadata".into(),
    }
}

fn connections(
    conn: &Connection,
    spec: &CodeWorkspaceSpec,
) -> Result<CodeWorkspaceConnections, PersistenceError> {
    // Select public metadata only; never load or copy the account's stored secret.
    let account: Option<String> = conn.query_row(
        "SELECT json_extract(credential, '$.username') FROM pipe_auth_credentials WHERE provider='github'",
        [], |row| row.get(0),
    ).optional()?;
    let github = match account {
        None => Binding::Missing,
        Some(account) if account.eq_ignore_ascii_case(&spec.github.account) => {
            Binding::PresentUnverified
        }
        Some(_) => Binding::IdentityChanged,
    };
    let ai = match &spec.agent {
        None => Binding::NotRequired,
        Some(binding) => {
            let metadata: Option<String> = conn.query_row(
                "SELECT json_extract(spec, '$.connection') FROM pipe_ai_connections WHERE id=?1",
                [binding.connection_id.to_string()], |row| row.get(0),
            ).optional()?;
            match metadata {
                None => Binding::Missing,
                Some(metadata) => {
                    let connection: AiConnection =
                        serde_json::from_str(&metadata).map_err(|_| corrupt())?;
                    if connection.id != binding.connection_id {
                        return Err(corrupt());
                    }
                    if binding.agent.provider_compatible(&connection) {
                        Binding::PresentUnverified
                    } else {
                        Binding::Incompatible
                    }
                }
            }
        }
    };
    Ok(CodeWorkspaceConnections { github, ai })
}

pub(super) fn read(conn: &Connection, id: Uuid) -> Result<Option<CodeWorkspace>, PersistenceError> {
    let row = conn.query_row(
        "SELECT name, spec, revision, created_at_ms, updated_at_ms FROM code_workspaces WHERE id=?1",
        [id.to_string()], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?, row.get::<_, i64>(4)?)),
    ).optional()?;
    let Some((name, spec, revision, created_at_ms, updated_at_ms)) = row else {
        return Ok(None);
    };
    let spec: CodeWorkspaceSpec = serde_json::from_str(&spec).map_err(|_| corrupt())?;
    if name != spec.name
        || !spec.validate().is_empty()
        || revision <= 0
        || created_at_ms < 0
        || updated_at_ms < created_at_ms
    {
        return Err(corrupt());
    }
    let revision = u64::try_from(revision).map_err(|_| corrupt())?;
    let connections = connections(conn, &spec)?;
    Ok(Some(CodeWorkspace {
        id,
        revision,
        spec,
        created_at_ms,
        updated_at_ms,
        connections,
    }))
}

impl Store {
    /// Return account observations from one SQLite snapshot, including after credential removal.
    pub(crate) fn code_workspaces(&self) -> Result<Vec<CodeWorkspace>, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let ids = {
            let mut query =
                tx.prepare("SELECT id FROM code_workspaces ORDER BY name COLLATE NOCASE, id")?;
            query
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        ids.into_iter()
            .map(|id| {
                let id = Uuid::parse_str(&id).map_err(|_| corrupt())?;
                read(&tx, id)?.ok_or_else(corrupt)
            })
            .collect()
    }

    /// Read a configuration and its current account references atomically.
    pub(crate) fn code_workspace(
        &self,
        id: Uuid,
    ) -> Result<Option<CodeWorkspace>, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        read(&tx, id)
    }

    /// Save only a syntactically valid configuration with current eligible account references.
    /// Revision checks, uniqueness and credential metadata reads share the write transaction.
    pub(crate) fn write_code_workspace(
        &self,
        id: Uuid,
        input: CodeWorkspaceWrite,
        now: i64,
    ) -> Result<WriteResult, PersistenceError> {
        let mut fields = input.spec.validate();
        if id.is_nil() {
            fields.insert("id".into(), "use a non-nil workspace UUID".into());
        }
        if input.expected_revision >= i64::MAX as u64 {
            fields.insert(
                "expectedRevision".into(),
                "revision exceeds the supported range".into(),
            );
        }
        if !fields.is_empty() {
            return Ok(WriteResult::Invalid(fields));
        }
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = read(&tx, id)?;
        let current_revision = existing.as_ref().map_or(0, |workspace| workspace.revision);
        if current_revision != input.expected_revision {
            return Ok(WriteResult::Conflict);
        }
        let taken: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM code_workspaces WHERE name=?1 COLLATE NOCASE AND id<>?2)",
            params![input.spec.name, id.to_string()],
            |row| row.get(0),
        )?;
        if taken {
            return Ok(WriteResult::NameTaken);
        }
        let observed = connections(&tx, &input.spec)?;
        if observed.github != Binding::PresentUnverified {
            fields.insert(
                "spec.github.account".into(),
                "select the currently connected GitHub account".into(),
            );
        }
        if !matches!(
            observed.ai,
            Binding::PresentUnverified | Binding::NotRequired
        ) {
            fields.insert("spec.agent.connectionId".into(), "select an existing AI AUTH account with an eligible provider and authentication method".into());
        }
        if !fields.is_empty() {
            return Ok(WriteResult::Invalid(fields));
        }
        let created_at_ms = existing
            .as_ref()
            .map_or(now.max(0), |workspace| workspace.created_at_ms);
        let updated_at_ms = existing
            .as_ref()
            .map_or(created_at_ms, |workspace| now.max(workspace.updated_at_ms));
        let revision = current_revision + 1;
        let spec = serde_json::to_string(&input.spec).map_err(|_| corrupt())?;
        tx.execute(
            "INSERT INTO code_workspaces(id,name,spec,revision,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name,spec=excluded.spec,revision=excluded.revision,updated_at_ms=excluded.updated_at_ms",
            params![id.to_string(), input.spec.name, spec, revision as i64, created_at_ms, updated_at_ms],
        )?;
        let result = CodeWorkspace {
            id,
            revision,
            spec: input.spec,
            created_at_ms,
            updated_at_ms,
            connections: observed,
        };
        tx.commit()?;
        Ok(WriteResult::Written(Box::new(result)))
    }

    /// Remove metadata only; this phase has no VM, volume, editor or agent to tear down.
    pub(crate) fn delete_code_workspace(
        &self,
        id: Uuid,
        expected_revision: u64,
    ) -> Result<DeleteResult, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let revision: Option<i64> = tx
            .query_row(
                "SELECT revision FROM code_workspaces WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(revision) = revision else {
            return Ok(DeleteResult::Missing);
        };
        let revision = u64::try_from(revision).map_err(|_| corrupt())?;
        if revision != expected_revision {
            return Ok(DeleteResult::Conflict);
        }
        tx.execute("DELETE FROM code_workspaces WHERE id=?1", [id.to_string()])?;
        tx.commit()?;
        Ok(DeleteResult::Deleted)
    }
}

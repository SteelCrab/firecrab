//! Reserve VM ownership before side effects and fence every asynchronous journal update.
use super::{PersistenceError, Store, code_workspaces};
use firecrab_api_types::*;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS code_runtimes (
 workspace_id TEXT PRIMARY KEY REFERENCES code_workspaces(id) ON DELETE RESTRICT,
 vm_id TEXT NOT NULL UNIQUE, record TEXT NOT NULL
) STRICT;";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Record {
    pub runtime: CodeWorkspaceRuntime,
    pub spec: CodeWorkspaceSpec,
    /// Confirmed VM creation; once set, a missing VM is never silently recreated.
    pub vm_created: bool,
}

pub(crate) enum Admission {
    Accepted(Box<Record>),
    Missing,
    Conflict,
    Busy,
    PlacementChanged,
}

fn corrupt() -> PersistenceError {
    PersistenceError::CorruptRecord {
        id: "code_runtime".into(),
        reason: "invalid runtime journal".into(),
    }
}

fn read(conn: &Connection, id: Uuid) -> Result<Option<Record>, PersistenceError> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT vm_id,record FROM code_runtimes WHERE workspace_id=?1",
            [id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    row.map(|(vm, raw)| {
        let record: Record = serde_json::from_str(&raw).map_err(|_| corrupt())?;
        if record.runtime.workspace_id != id
            || record.runtime.vm_id.to_string() != vm
            || record.runtime.vm_id.is_nil()
            || record.runtime.operation_id.is_nil()
            || record.runtime.project_path != "/home/fc-code/project"
            || (record.runtime.phase == CodeRuntimePhase::Ready
                && (!record.vm_created
                    || record.runtime.runtime_id.is_none()
                    || record.runtime.initial_commit.is_none()))
            || !record.spec.validate().is_empty()
        {
            return Err(corrupt());
        }
        Ok(record)
    })
    .transpose()
}

fn encode(record: &Record) -> Result<String, PersistenceError> {
    serde_json::to_string(record).map_err(|_| corrupt())
}

/// File-affecting configuration cannot drift underneath a retained working folder.
pub(super) fn configuration_allowed(
    conn: &Connection,
    id: Uuid,
    spec: &CodeWorkspaceSpec,
) -> Result<bool, PersistenceError> {
    Ok(read(conn, id)?.is_none_or(|record| {
        !record.runtime.phase.pending()
            && record.spec.github.owner == spec.github.owner
            && record.spec.github.repository == spec.github.repository
            && record.spec.github.branch == spec.github.branch
            && record.spec.runtime == spec.runtime
            && record.spec.resources == spec.resources
    }))
}

impl Store {
    pub(crate) fn code_runtime(&self, id: Uuid) -> Result<Option<Record>, PersistenceError> {
        read(&self.lock(), id)
    }

    pub(crate) fn code_runtime_owner(&self, vm: Uuid) -> Result<Option<Uuid>, PersistenceError> {
        let id: Option<String> = self
            .lock()
            .query_row(
                "SELECT workspace_id FROM code_runtimes WHERE vm_id=?1",
                [vm.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        id.map(|id| Uuid::parse_str(&id).map_err(|_| corrupt()))
            .transpose()
    }

    pub(crate) fn code_runtime_pending(&self) -> Result<Vec<Record>, PersistenceError> {
        let conn = self.lock();
        let mut query =
            conn.prepare("SELECT workspace_id FROM code_runtimes ORDER BY workspace_id")?;
        let ids = query
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                read(&conn, Uuid::parse_str(&id).map_err(|_| corrupt())?)?.ok_or_else(corrupt)
            })
            .filter(|r| match r {
                Ok(r) => r.runtime.phase.pending(),
                Err(_) => true,
            })
            .collect()
    }

    /// Placement is required only for the first start. Duplicate active actions reuse their operation.
    pub(crate) fn admit_code_runtime(
        &self,
        id: Uuid,
        revision: u64,
        placement: Option<CodeRuntimePlacement>,
        action: CodeRuntimeAction,
    ) -> Result<Admission, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(workspace) = code_workspaces::read(&tx, id)? else {
            return Ok(Admission::Missing);
        };
        if revision == 0 || workspace.revision != revision {
            return Ok(Admission::Conflict);
        }
        let existing = read(&tx, id)?;
        let mut record = match existing {
            Some(record) => {
                if placement
                    .as_ref()
                    .is_some_and(|placement| placement != &record.runtime.placement)
                {
                    return Ok(Admission::PlacementChanged);
                }
                if record.runtime.phase.pending() {
                    return Ok(if record.runtime.action == action {
                        Admission::Accepted(Box::new(record))
                    } else {
                        Admission::Busy
                    });
                }
                record
            }
            None => {
                let Some(placement) = placement else {
                    return Ok(Admission::Missing);
                };
                if action != CodeRuntimeAction::Start {
                    return Ok(Admission::Missing);
                }
                Record {
                    spec: workspace.spec.clone(),
                    vm_created: false,
                    runtime: CodeWorkspaceRuntime {
                        workspace_id: id,
                        vm_id: Uuid::new_v4(),
                        configuration_revision: revision,
                        operation_id: Uuid::new_v4(),
                        action,
                        phase: CodeRuntimePhase::Queued,
                        placement,
                        project_path: "/home/fc-code/project".into(),
                        runtime_id: None,
                        initial_commit: None,
                        vm_state: None,
                        error: None,
                    },
                }
            }
        };
        record.runtime.configuration_revision = revision;
        record.runtime.operation_id = Uuid::new_v4();
        record.runtime.action = action;
        record.runtime.phase = CodeRuntimePhase::Queued;
        record.runtime.error = None;
        tx.execute("INSERT INTO code_runtimes(workspace_id,vm_id,record) VALUES(?1,?2,?3) ON CONFLICT(workspace_id) DO UPDATE SET record=excluded.record",params![id.to_string(),record.runtime.vm_id.to_string(),encode(&record)?])?;
        tx.commit()?;
        Ok(Admission::Accepted(Box::new(record)))
    }

    /// A stale worker cannot overwrite a later operation or recreate a detached binding.
    pub(crate) fn save_code_runtime(&self, record: &Record) -> Result<bool, PersistenceError> {
        Ok(self.lock().execute("UPDATE code_runtimes SET record=?1 WHERE workspace_id=?2 AND json_extract(record,'$.runtime.operationId')=?3",params![encode(record)?,record.runtime.workspace_id.to_string(),record.runtime.operation_id.to_string()])? == 1)
    }

    /// Caller holds the worker's process lock and checks the live VM is stopped or absent.
    pub(crate) fn detach_code_runtime(
        &self,
        id: Uuid,
        revision: u64,
    ) -> Result<bool, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(workspace) = code_workspaces::read(&tx, id)? else {
            return Ok(false);
        };
        if workspace.revision != revision
            || read(&tx, id)?.is_some_and(|r| r.runtime.phase.pending())
        {
            return Ok(false);
        }
        tx.execute(
            "DELETE FROM code_runtimes WHERE workspace_id=?1",
            [id.to_string()],
        )?;
        tx.commit()?;
        Ok(true)
    }
}

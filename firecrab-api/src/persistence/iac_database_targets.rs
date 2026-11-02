//! Serialize schema/configuration work by database target, including saved aliases.

use super::PersistenceError;
use crate::pipe_databases::maintenance::{Job, Kind};
use rusqlite::{Connection, params};
use uuid::Uuid;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS iac_database_targets (
 target_hash TEXT NOT NULL, operation_id TEXT NOT NULL REFERENCES iac_native_operations(operation_id),
 PRIMARY KEY(target_hash,operation_id)
) STRICT;
CREATE INDEX IF NOT EXISTS iac_database_target_operation ON iac_database_targets(operation_id);";

pub(super) fn active(status: &str) -> bool {
    !matches!(status, "succeeded" | "failed" | "recovered" | "expired")
}

pub(super) fn native_idle(conn: &Connection, target: &str) -> Result<(), PersistenceError> {
    let busy:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM iac_database_targets t JOIN iac_native_operations o ON o.operation_id=t.operation_id WHERE t.target_hash=?1 AND o.state IN ('running','needs_recovery'))", [target], |row|row.get(0))?;
    if busy {
        return Err(PersistenceError::IacResourceBusy);
    }
    Ok(())
}

/// Restore retains the existing explicit recovery exception for uncertain jobs.
pub(super) fn idle(
    conn: &Connection,
    target: &str,
    own_job: Option<Uuid>,
    restore: bool,
) -> Result<(), PersistenceError> {
    native_idle(conn, target)?;
    let mut stmt=conn.prepare("SELECT spec FROM database_jobs WHERE json_extract(spec,'$.targetHash')=?1 AND id<>COALESCE(?2,'')")?;
    for raw in stmt.query_map(params![target, own_job.map(|id| id.to_string())], |row| {
        row.get::<_, String>(0)
    })? {
        let job: Job =
            serde_json::from_str(&raw?).map_err(|_| PersistenceError::IacOperationIdentity)?;
        if active(&job.status) && !(restore && job.status == "needs_recovery") {
            return Err(PersistenceError::IacResourceBusy);
        }
    }
    Ok(())
}

pub(super) fn same_request(a: &Job, b: &Job) -> bool {
    a.database_id == b.database_id
        && a.kind == b.kind
        && a.target_hash == b.target_hash
        && a.version == b.version
        && a.sql == b.sql
        && a.backup_id == b.backup_id
        && a.parent == b.parent
}

pub(super) fn accept_job(
    conn: &Connection,
    before: Option<&Job>,
    job: &Job,
) -> Result<(), PersistenceError> {
    if let Some(before) = before {
        if before.execution_generation != job.execution_generation {
            return Err(PersistenceError::StaleIacOwner);
        }
        if !same_request(before, job) {
            return Err(PersistenceError::IacOperationIdentity);
        }
        if !active(&before.status)
            && before.status == job.status
            && serde_json::to_value(before).map_err(|_| PersistenceError::IacOperationIdentity)?
                != serde_json::to_value(job).map_err(|_| PersistenceError::IacOperationIdentity)?
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        if !active(&before.status)
            && before.status != job.status
            && !(before.kind == Kind::Backup && job.status == "expired")
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
    }
    if before.is_none() {
        super::iac_database_migrations::admit(conn, job)?;
    }
    let nested = job.parent.is_some();
    if let Some(scope) = &job.parent
        && job.status != "expired"
    {
        let parent = super::database_job_claims::read(conn, scope.id)?
            .ok_or(PersistenceError::StaleIacOwner)?;
        if parent.status != "running"
            || parent.execution_generation != scope.execution_generation
            || parent.kind == Kind::Backup
            || job.kind != Kind::Backup
            || parent.database_id != job.database_id
            || parent.target_hash != job.target_hash
            || parent.safety_backup_id != Some(job.id)
            || parent.id == job.id
        {
            return Err(PersistenceError::StaleIacOwner);
        }
    }
    if active(&job.status) && before.is_none_or(|old| !active(&old.status)) {
        if nested {
            return native_idle(conn, &job.target_hash);
        }
        idle(
            conn,
            &job.target_hash,
            Some(job.id),
            job.kind == Kind::Restore,
        )?;
    }
    Ok(())
}

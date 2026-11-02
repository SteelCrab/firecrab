//! Immutable schema versions, exact history ownership and explicit no-effect retries.
use super::{
    PersistenceError, Store, database_job_claims, iac_database_targets, iac_native, iac_owners,
};
use crate::pipe_databases::maintenance::{self, Job, Kind};
use firecrab_stack::ManagedResource;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

pub(super) fn resource(job: &Job) -> ManagedResource {
    ManagedResource {
        kind: "database_migration".into(),
        id: format!("{}:{}", job.database_id, job.id),
    }
}
fn definition(job: &Job) -> Result<(&str, &str), PersistenceError> {
    let version = job
        .version
        .as_deref()
        .ok_or(PersistenceError::InvalidStackResource)?;
    maintenance::validate_version(version).map_err(|_| PersistenceError::InvalidStackResource)?;
    let sql = job
        .sql
        .as_deref()
        .filter(|v| !v.trim().is_empty())
        .ok_or(PersistenceError::InvalidStackResource)?;
    if job.id.is_nil() || job.database_id.is_nil() || job.kind != Kind::Migration {
        return Err(PersistenceError::InvalidStackResource);
    }
    Ok((version, sql))
}
/// A version is one immutable definition across saved aliases of the same source.
pub(super) fn admit(conn: &Connection, job: &Job) -> Result<(), PersistenceError> {
    if job.kind != Kind::Migration {
        return Ok(());
    }
    let (version, _) = definition(job)?;
    if !iac_owners::public_write_allowed(conn, &resource(job))? {
        return Err(PersistenceError::IacOwnershipConflict);
    }
    let occupied: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM database_jobs WHERE id<>?1 AND json_extract(spec,'$.kind')='migration' AND json_extract(spec,'$.targetHash')=?2 AND json_extract(spec,'$.version')=?3)", params![job.id.to_string(),job.target_hash,version], |r|r.get(0))?;
    if occupied {
        return Err(PersistenceError::IacOperationIdentity);
    }
    Ok(())
}
/// Transfers retain the exact version, SQL checksum and original outcome without replaying SQL.
pub(super) fn validate(
    conn: &Connection,
    target: &ManagedResource,
    adopting: bool,
) -> Result<(), PersistenceError> {
    let (database, id) = target
        .id
        .split_once(':')
        .ok_or(PersistenceError::InvalidStackResource)?;
    let database = Uuid::parse_str(database).map_err(|_| PersistenceError::InvalidStackResource)?;
    let id = Uuid::parse_str(id).map_err(|_| PersistenceError::InvalidStackResource)?;
    if database.is_nil() || id.is_nil() || target.id != format!("{database}:{id}") {
        return Err(PersistenceError::InvalidStackResource);
    }
    let Some(job) = database_job_claims::read(conn, id)? else {
        return if adopting {
            Err(PersistenceError::InvalidStackResource)
        } else {
            Ok(())
        };
    };
    definition(&job)?;
    if resource(&job) != *target {
        return Err(PersistenceError::InvalidStackResource);
    }
    if iac_database_targets::active(&job.status) {
        return Err(PersistenceError::IacResourceBusy);
    }
    if adopting {
        let duplicate:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM database_jobs WHERE id<>?1 AND json_extract(spec,'$.kind')='migration' AND json_extract(spec,'$.targetHash')=?2 AND json_extract(spec,'$.version')=?3)",params![job.id.to_string(),job.target_hash,job.version],|r|r.get(0))?;
        if duplicate {
            return Err(PersistenceError::InvalidStackResource);
        }
    }
    iac_native::ensure_idle(conn, target)?;
    iac_database_targets::idle(conn, &job.target_hash, None, false)
}
impl Store {
    /// Perform the same final admission before read-only remote maintenance preflight.
    pub(crate) fn database_migration_admission(&self, job: &Job) -> Result<(), PersistenceError> {
        admit(&self.lock(), job)
    }
    /// Retry only a verified no-effect failure, preserving its original immutable version and UUID.
    pub(crate) fn retry_database_migration(
        &self,
        id: Uuid,
        expected_generation: u64,
    ) -> Result<Job, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut job =
            database_job_claims::read(&tx, id)?.ok_or(PersistenceError::InvalidStackResource)?;
        definition(&job)?;
        if job.execution_generation != expected_generation {
            return Err(PersistenceError::StaleIacOwner);
        }
        if job.status != "failed" || job.maintenance_started {
            return Err(PersistenceError::IacResourceBusy);
        }
        admit(&tx, &job)?;
        iac_database_targets::idle(&tx, &job.target_hash, Some(id), false)?;
        let raw: String = tx
            .query_row(
                "SELECT spec FROM pipe_databases WHERE id=?1",
                [job.database_id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(PersistenceError::InvalidStackResource)?;
        let record = crate::pipe_databases::DatabaseRecord {
            id: job.database_id,
            input: super::database_services::decode(&raw)?,
        };
        if maintenance::target_hash(&record).map_err(|_| PersistenceError::InvalidStackResource)?
            != job.target_hash
        {
            return Err(PersistenceError::StaleIacOwner);
        }
        job.execution_generation = job
            .execution_generation
            .checked_add(1)
            .filter(|v| *v <= i64::MAX as u64)
            .ok_or(PersistenceError::IacOperationIdentity)?;
        job.status = "queued".into();
        job.error = None;
        tx.execute(
            "UPDATE database_jobs SET spec=?1 WHERE id=?2",
            params![super::database_services::encode(&job)?, id.to_string()],
        )?;
        tx.commit()?;
        Ok(job)
    }
}

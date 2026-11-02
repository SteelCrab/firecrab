//! One durable executor per maintenance attempt; stale workers cannot publish results.

use super::{
    PersistenceError, Store,
    database_services::{decode, encode},
};
use crate::pipe_databases::maintenance::Job;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

pub(super) fn read(conn: &Connection, id: Uuid) -> Result<Option<Job>, PersistenceError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT spec FROM database_jobs WHERE id=?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    raw.as_deref().map(decode).transpose()
}

fn advance(job: &mut Job) -> Result<(), PersistenceError> {
    job.execution_generation = job
        .execution_generation
        .checked_add(1)
        .filter(|g| *g <= i64::MAX as u64)
        .ok_or(PersistenceError::IacOperationIdentity)?;
    Ok(())
}

impl Store {
    pub(crate) fn claim_database_job(
        &self,
        id: Uuid,
        expected_generation: u64,
    ) -> Result<Option<Job>, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(mut job) = read(&tx, id)? else {
            return Ok(None);
        };
        if job.status != "queued" || job.execution_generation != expected_generation {
            return Ok(None);
        }
        advance(&mut job)?;
        job.status = "running".into();
        tx.execute(
            "UPDATE database_jobs SET spec=?2 WHERE id=?1",
            params![id.to_string(), encode(&job)?],
        )?;
        tx.commit()?;
        Ok(Some(job))
    }

    pub(crate) fn interrupt_database_job(
        &self,
        id: Uuid,
        expected_generation: u64,
    ) -> Result<bool, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(mut job) = read(&tx, id)? else {
            return Ok(false);
        };
        if job.status != "running" || job.execution_generation != expected_generation {
            return Ok(false);
        }
        advance(&mut job)?;
        job.status = if job.maintenance_started {
            "needs_recovery"
        } else {
            "failed"
        }
        .into();
        job.error = Some(
            "The API restarted during this job; inspect its safety backup before retrying".into(),
        );
        tx.execute(
            "UPDATE database_jobs SET spec=?2 WHERE id=?1",
            params![id.to_string(), encode(&job)?],
        )?;
        tx.commit()?;
        Ok(true)
    }
}

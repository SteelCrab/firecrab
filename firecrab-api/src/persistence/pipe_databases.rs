//! Credentials live in the existing owner-only SQLite store, never in responses.

use super::{PersistenceError, Store};
use crate::pipe_databases::{DatabaseInput, DatabaseRecord};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

/// Store saved connection input in the existing owner-only SQLite database.
pub(super) const CREATE_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS pipe_databases (
    id TEXT PRIMARY KEY,
    spec TEXT NOT NULL
) STRICT";

impl Store {
    /// Capture authority before remote connection verification or namespace setup.
    pub(crate) fn pipe_database_write_authority(
        &self,
        id: Uuid,
    ) -> Result<firecrab_stack::ResourceOwnership, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let resource = firecrab_stack::ManagedResource {
            kind: "database_connection".into(),
            id: id.to_string(),
        };
        if !super::iac_owners::public_write_allowed(&tx, &resource)? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        let snapshot = super::iac_owners::read(&tx, &resource)?;
        tx.commit()?;
        Ok(snapshot)
    }

    #[cfg(test)]
    pub fn update_pipe_database(
        &self,
        id: Uuid,
        input: &DatabaseInput,
    ) -> Result<bool, PersistenceError> {
        let authority = self.pipe_database_write_authority(id)?;
        self.update_pipe_database_checked(id, input, &authority)
    }

    /// Late validation cannot replace credentials after an ownership release/reclaim.
    pub(crate) fn update_pipe_database_checked(
        &self,
        id: Uuid,
        input: &DatabaseInput,
        expected: &firecrab_stack::ResourceOwnership,
    ) -> Result<bool, PersistenceError> {
        let spec = serde_json::to_string(input).map_err(|_| PersistenceError::CorruptRecord {
            id: id.to_string(),
            reason: "invalid database configuration".into(),
        })?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let resource = firecrab_stack::ManagedResource {
            kind: "database_connection".into(),
            id: id.to_string(),
        };
        if super::iac_owners::read(&tx, &resource)? != *expected {
            return Err(PersistenceError::StaleIacOwner);
        }
        if !super::iac_owners::public_write_allowed(&tx, &resource)? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        let changed = tx.execute(
            "UPDATE pipe_databases SET spec=?2 WHERE id=?1",
            params![id.to_string(), spec],
        )?;
        tx.commit()?;
        Ok(changed != 0)
    }
    /// Return all saved connection inputs in insertion order.
    pub fn list_pipe_databases(&self) -> Result<Vec<DatabaseRecord>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare("SELECT id, spec FROM pipe_databases ORDER BY rowid")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.map(|row| {
            let (id, spec) = row?;
            decode(id, spec)
        })
        .collect()
    }

    /// Return only identifiers and names for security coverage; never load connection URIs.
    pub(crate) fn pipe_database_security_names(
        &self,
    ) -> Result<Vec<(Uuid, String)>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT id, json_extract(spec, '$.name') FROM pipe_databases ORDER BY rowid",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (id, name) = row?;
            let parsed = Uuid::parse_str(&id).map_err(|_| PersistenceError::CorruptRecord {
                id,
                reason: "invalid database identifier".into(),
            })?;
            Ok((parsed, name))
        })
        .collect()
    }

    /// List only operator-selected database-to-service associations.
    pub(crate) fn security_database_links(&self) -> Result<Vec<(Uuid, Uuid)>, PersistenceError> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT database_id, service_id FROM service_security_database_links")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (database, service) = row?;
            let bad = || PersistenceError::CorruptRecord {
                id: database.clone(),
                reason: "invalid database security link".into(),
            };
            Ok((
                Uuid::parse_str(&database).map_err(|_| bad())?,
                Uuid::parse_str(&service).map_err(|_| bad())?,
            ))
        })
        .collect()
    }

    /// Link an existing database connection to a deployed, locally managed service.
    pub(crate) fn set_security_database_link(
        &self,
        database: Uuid,
        service: Uuid,
    ) -> Result<bool, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let changed = tx.execute("INSERT INTO service_security_database_links(database_id,service_id) SELECT ?1,?2 WHERE EXISTS(SELECT 1 FROM pipe_databases WHERE id=?1) AND EXISTS(SELECT 1 FROM micro_services WHERE id=?2) AND EXISTS(SELECT 1 FROM micro_service_deployments WHERE service_id=?2) AND NOT EXISTS(SELECT 1 FROM cluster_service_workers WHERE id=?2) ON CONFLICT(database_id) DO UPDATE SET service_id=excluded.service_id",params![database.to_string(),service.to_string()])?;
        tx.commit()?;
        Ok(changed > 0)
    }

    /// Remove an association without changing its database or service.
    pub(crate) fn remove_security_database_link(
        &self,
        database: Uuid,
    ) -> Result<bool, PersistenceError> {
        Ok(self.lock().execute(
            "DELETE FROM service_security_database_links WHERE database_id=?1",
            [database.to_string()],
        )? > 0)
    }

    /// Load one connection input, returning None when the identifier is unknown.
    pub fn pipe_database(&self, id: Uuid) -> Result<Option<DatabaseRecord>, PersistenceError> {
        let row: Option<(String, String)> = self
            .lock()
            .query_row(
                "SELECT id, spec FROM pipe_databases WHERE id = ?1",
                [id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(id, spec)| decode(id, spec)).transpose()
    }

    /// Persist a connection after its credentials have been authenticated.
    pub fn save_pipe_database(
        &self,
        id: Uuid,
        input: &DatabaseInput,
    ) -> Result<(), PersistenceError> {
        let spec = serde_json::to_string(input).map_err(|_| PersistenceError::CorruptRecord {
            id: id.to_string(),
            reason: "invalid database configuration".into(),
        })?;
        self.lock().execute(
            "INSERT INTO pipe_databases (id, spec) VALUES (?1, ?2)",
            params![id.to_string(), spec],
        )?;
        Ok(())
    }

    /// Remove only saved credentials and report whether a record existed.
    pub fn delete_pipe_database(&self, id: Uuid) -> Result<bool, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM service_security_database_links WHERE database_id=?1",
            [id.to_string()],
        )?;
        let deleted = tx.execute("DELETE FROM pipe_databases WHERE id=?1", [id.to_string()])? > 0;
        tx.commit()?;
        Ok(deleted)
    }

    pub fn set_database_writes(&self, id: Uuid, enabled: bool) -> Result<bool, PersistenceError> {
        let Some(mut record) = self.pipe_database(id)? else {
            return Ok(false);
        };
        record.input.write_enabled = enabled;
        let spec =
            serde_json::to_string(&record.input).map_err(|_| PersistenceError::CorruptRecord {
                id: id.to_string(),
                reason: "invalid database configuration".into(),
            })?;
        Ok(self.lock().execute(
            "UPDATE pipe_databases SET spec=?2 WHERE id=?1",
            params![id.to_string(), spec],
        )? != 0)
    }
}

/// Decode stored input without exposing malformed credential data in errors.
fn decode(id: String, spec: String) -> Result<DatabaseRecord, PersistenceError> {
    let corrupt = || PersistenceError::CorruptRecord {
        id: id.clone(),
        reason: "invalid database configuration".into(),
    };
    Ok(DatabaseRecord {
        id: Uuid::parse_str(&id).map_err(|_| corrupt())?,
        input: serde_json::from_str(&spec).map_err(|_| corrupt())?,
    })
}

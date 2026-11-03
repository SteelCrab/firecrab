//! Per-service, credential-free OTLP endpoint settings.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use super::{PersistenceError, Store};

/// Configuration metadata only; application observations are never part of this view.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SettingsView {
    service_id: Uuid,
    endpoint: Option<String>,
    enabled_at_ms: Option<i64>,
    ownership: firecrab_stack::ResourceOwnership,
}

/// The endpoint is a current operator setting, independent of release snapshots.
pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS service_otlp_config (
    service_id TEXT PRIMARY KEY,
    endpoint TEXT NOT NULL,
    enabled_at_ms INTEGER NOT NULL DEFAULT 0
) STRICT";

/// Add the opt-in boundary to databases created before the column existed.
pub(super) fn migrate(conn: &Connection) -> Result<(), PersistenceError> {
    let tx = conn.unchecked_transaction()?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('service_otlp_config') WHERE name='enabled_at_ms')",
        [], |row| row.get(0),
    )?;
    if !exists {
        // Existing experimental settings have no trustworthy consent timestamp.
        // Start their export window at migration rather than exposing older logs.
        tx.execute(
            "ALTER TABLE service_otlp_config ADD COLUMN enabled_at_ms INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
        tx.execute(
            "UPDATE service_otlp_config SET enabled_at_ms=?1",
            [chrono::Utc::now().timestamp_millis().saturating_add(1)],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Remove a setting inside the service deletion transaction.
pub(super) fn delete(conn: &Connection, service_id: Uuid) -> Result<(), PersistenceError> {
    conn.execute(
        "DELETE FROM service_otlp_config WHERE service_id=?1",
        [service_id.to_string()],
    )?;
    Ok(())
}

/// The independently managed exporter slot belongs to one exact local service UUID.
pub(super) fn validate_owner(
    conn: &Connection,
    resource: &firecrab_stack::ManagedResource,
    adopting: bool,
) -> Result<(), PersistenceError> {
    let id = Uuid::parse_str(&resource.id).map_err(|_| PersistenceError::InvalidIacOwner)?;
    if adopting
        && !conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM micro_services WHERE id=?1)",
            [id.to_string()],
            |row| row.get::<_, bool>(0),
        )?
    {
        return Err(PersistenceError::InvalidStackResource);
    }
    if super::stacks::resource_apply_busy(conn, firecrab_stack::ResourceKind::Service, id)? {
        return Err(PersistenceError::StackApplyBusy);
    }
    super::iac_owners::ensure_resource_idle(conn, firecrab_stack::ResourceKind::Service, id)
}

pub(super) fn check_spec(
    endpoint: &str,
    spec: &firecrab_api_types::MicroServiceSpec,
) -> Result<(), PersistenceError> {
    crate::otlp::application_env(
        endpoint,
        &spec.name,
        &spec.env,
        &spec.secrets.keys().cloned().collect(),
    )
    .map(|_| ())
    .map_err(|_| PersistenceError::InvalidStackResource)
}

/// Check both saved and active service environments in the setting's commit transaction.
pub(super) fn validate_configuration(
    conn: &Connection,
    service: Uuid,
    endpoint: Option<&str>,
) -> Result<(), PersistenceError> {
    let local: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM micro_services WHERE id=?1)
        AND NOT EXISTS(SELECT 1 FROM cluster_services WHERE id=?1)
        AND NOT EXISTS(SELECT 1 FROM cluster_service_workers WHERE id=?1)",
        [service.to_string()],
        |row| row.get(0),
    )?;
    if !local {
        return Err(PersistenceError::InvalidStackResource);
    }
    let Some(endpoint) = endpoint else {
        return Ok(());
    };
    let raw: String = conn.query_row(
        "SELECT spec FROM micro_services WHERE id=?1",
        [service.to_string()],
        |row| row.get(0),
    )?;
    let spec = serde_json::from_str(&raw).map_err(|_| PersistenceError::IacOperationIdentity)?;
    check_spec(endpoint, &spec)?;
    let active: Option<String> = conn
        .query_row(
            "SELECT deployment FROM micro_service_deployments WHERE service_id=?1",
            [service.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(active) = active {
        let active: firecrab_api_types::MicroServiceDeployment =
            serde_json::from_str(&active).map_err(|_| PersistenceError::IacOperationIdentity)?;
        check_spec(endpoint, &active.spec)?;
    }
    Ok(())
}

/// Service writers recheck the current setting instead of trusting an earlier HTTP preflight.
pub(super) fn validate_persisted_spec(
    conn: &Connection,
    service: Uuid,
    spec: &firecrab_api_types::MicroServiceSpec,
) -> Result<(), PersistenceError> {
    let endpoint: Option<String> = conn
        .query_row(
            "SELECT endpoint FROM service_otlp_config WHERE service_id=?1",
            [service.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(endpoint) = endpoint {
        check_spec(&endpoint, spec)?;
    }
    Ok(())
}

impl Store {
    /// Read configuration and authority in one snapshot without reading application data.
    pub(crate) fn otlp_settings(
        &self,
        service: Uuid,
    ) -> Result<Option<SettingsView>, PersistenceError> {
        let conn = self.lock();
        let tx = rusqlite::Transaction::new_unchecked(&conn, TransactionBehavior::Deferred)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM micro_services WHERE id=?1)",
            [service.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(None);
        }
        let config: Option<(String, i64)> = tx
            .query_row(
                "SELECT endpoint,enabled_at_ms FROM service_otlp_config WHERE service_id=?1",
                [service.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let ownership = super::iac_owners::read(
            &tx,
            &firecrab_stack::ManagedResource {
                kind: "service_otlp".into(),
                id: service.to_string(),
            },
        )?;
        let (endpoint, enabled_at_ms) = match config {
            Some((endpoint, boundary)) => (Some(endpoint), Some(boundary)),
            None => (None, None),
        };
        Ok(Some(SettingsView {
            service_id: service,
            endpoint,
            enabled_at_ms,
            ownership,
        }))
    }

    /// Return the current endpoint, or `None` when export is disabled.
    pub(crate) fn otlp_endpoint(&self, service: Uuid) -> Result<Option<String>, PersistenceError> {
        Ok(self
            .otlp_configuration(service)?
            .map(|(endpoint, _)| endpoint))
    }

    /// Return the endpoint and its durable opt-in boundary.
    pub(crate) fn otlp_configuration(
        &self,
        service: Uuid,
    ) -> Result<Option<(String, i64)>, PersistenceError> {
        self.lock()
            .query_row(
                "SELECT endpoint, enabled_at_ms FROM service_otlp_config WHERE service_id=?1",
                [service.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Store a validated endpoint, or disable export by clearing the setting.
    /// The owner check and update use the same connection lock as service deletion.
    #[cfg(test)]
    pub(crate) fn set_otlp_endpoint(
        &self,
        service: Uuid,
        endpoint: Option<&str>,
    ) -> Result<i64, PersistenceError> {
        let authority = self.iac_resource_owner(&firecrab_stack::ManagedResource {
            kind: "service_otlp".into(),
            id: service.to_string(),
        })?;
        self.set_otlp_endpoint_checked(service, endpoint, &authority)
    }

    /// Compare the authority captured before validation inside the configuration commit.
    pub(crate) fn set_otlp_endpoint_checked(
        &self,
        service: Uuid,
        endpoint: Option<&str>,
        expected: &firecrab_stack::ResourceOwnership,
    ) -> Result<i64, PersistenceError> {
        let endpoint = endpoint
            .map(|value| {
                crate::otlp::validate_endpoint(value)
                    .map(|url| url.to_string())
                    .map_err(|_| PersistenceError::CorruptRecord {
                        id: service.to_string(),
                        reason: "invalid OTLP endpoint".into(),
                    })
            })
            .transpose()?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM micro_services WHERE id=?1)",
            [service.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(rusqlite::Error::QueryReturnedNoRows.into());
        }
        let resource = firecrab_stack::ManagedResource {
            kind: "service_otlp".into(),
            id: service.to_string(),
        };
        if super::iac_owners::read(&tx, &resource)? != *expected {
            return Err(PersistenceError::StaleIacOwner);
        }
        validate_owner(&tx, &resource, false)?;
        if !super::iac_owners::public_write_allowed(&tx, &resource)? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        validate_configuration(&tx, service, endpoint.as_deref())?;
        let enabled_at_ms = chrono::Utc::now().timestamp_millis().saturating_add(1);
        if let Some(endpoint) = endpoint {
            tx.execute(
                "INSERT INTO service_otlp_config(service_id, endpoint, enabled_at_ms) VALUES (?1, ?2, ?3)
                 ON CONFLICT(service_id) DO UPDATE SET endpoint=excluded.endpoint, enabled_at_ms=excluded.enabled_at_ms",
                params![service.to_string(), endpoint, enabled_at_ms],
            )?;
        } else {
            delete(&tx, service)?;
        }
        tx.commit()?;
        Ok(enabled_at_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_excludes_preexisting_buffer_and_preserves_the_boundary_on_reopen() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE service_otlp_config (service_id TEXT PRIMARY KEY, endpoint TEXT NOT NULL) STRICT", []).unwrap();
        conn.execute(
            "INSERT INTO service_otlp_config VALUES ('service', 'http://collector.test/')",
            [],
        )
        .unwrap();
        let before = chrono::Utc::now().timestamp_millis();
        migrate(&conn).unwrap();
        let (endpoint, boundary): (String, i64) = conn
            .query_row(
                "SELECT endpoint, enabled_at_ms FROM service_otlp_config",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(endpoint, "http://collector.test/");
        assert!(boundary > before);
        migrate(&conn).unwrap();
        let after: i64 = conn
            .query_row("SELECT enabled_at_ms FROM service_otlp_config", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(after, boundary);
    }
}

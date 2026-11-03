//! Local collector consent is independent of a service release and its application data.

use super::*;
use firecrab_stack::ServiceOtlpConfiguration;

#[cfg(test)]
mod tests;

/// Private preparation for a changed plan; it contains no telemetry records.
pub(crate) struct OtlpInput {
    pub service: Uuid,
    pub name: String,
    pub endpoint: Option<String>,
}

impl Store {
    /// A completed receipt or no-op never allocates or restarts an exporter.
    pub(crate) fn resource_plan_otlp_input(
        &self,
        id: Uuid,
        input: &ApplyResourcePlan,
        now: u64,
    ) -> Result<Option<OtlpInput>, PersistenceError> {
        let conn = self.lock();
        let tx = rusqlite::Transaction::new_unchecked(&conn, TransactionBehavior::Deferred)?;
        if iac_owners::endpoint(&tx)? != input.expected_endpoint_id {
            return Err(PersistenceError::IacEndpointConflict);
        }
        let frozen = read(&tx, id)?.ok_or(PersistenceError::IacOperationIdentity)?;
        let summary = frozen.summary()?;
        if summary.ownership.resource.kind != "service_otlp"
            || summary.plan_sha256 != input.plan_sha256
            || frozen.request.expected_endpoint_id != input.expected_endpoint_id
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let completed: bool = tx.query_row(
            "SELECT receipt IS NOT NULL FROM iac_resource_plans WHERE operation_id=?1",
            [id.to_string()],
            |row| row.get(0),
        )?;
        if completed || summary.action == ResourcePlanAction::Noop {
            return Ok(None);
        }
        if now >= frozen.expires {
            return Err(PersistenceError::StaleIacExecution);
        }
        authority(&tx, &frozen, None)?;
        if super::observe(&tx, &frozen.ownership.resource, None)? != frozen.before {
            return Err(PersistenceError::StaleIacExecution);
        }
        let service: Uuid = frozen
            .ownership
            .resource
            .id
            .parse()
            .map_err(|_| PersistenceError::IacOperationIdentity)?;
        let endpoint = match &frozen.request.desired {
            Some(ResourceConfiguration::ServiceOtlp(config)) => config.endpoint.clone(),
            None => None,
            _ => return Err(PersistenceError::IacOperationIdentity),
        };
        let name = if endpoint.is_some() {
            tx.query_row(
                "SELECT name FROM micro_services WHERE id=?1",
                [service.to_string()],
                |row| row.get(0),
            )?
        } else {
            String::new()
        };
        Ok(Some(OtlpInput {
            service,
            name,
            endpoint,
        }))
    }

    /// Never install an old endpoint over a later setting or authority generation.
    pub(crate) fn resource_plan_otlp_activation(
        &self,
        receipt: &ResourcePlanReceipt,
    ) -> Result<Option<Option<i64>>, PersistenceError> {
        let conn = self.lock();
        let tx = rusqlite::Transaction::new_unchecked(&conn, TransactionBehavior::Deferred)?;
        let frozen =
            read(&tx, receipt.plan.operation_id)?.ok_or(PersistenceError::IacOperationIdentity)?;
        if frozen.ownership.resource.kind != "service_otlp" || frozen.summary()? != receipt.plan {
            return Err(PersistenceError::IacOperationIdentity);
        }
        if iac_owners::read(&tx, &receipt.ownership.resource)? != receipt.ownership {
            return Ok(None);
        }
        let operation = iac_native::read(&tx, receipt.plan.operation_id)?
            .ok_or(PersistenceError::IacOperationIdentity)?;
        if operation.state != NativeOperationState::Succeeded {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let config: Option<(String, i64)> = tx
            .query_row(
                "SELECT endpoint,enabled_at_ms FROM service_otlp_config WHERE service_id=?1",
                [&receipt.ownership.resource.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let desired = match &frozen.request.desired {
            Some(ResourceConfiguration::ServiceOtlp(config)) => config.endpoint.as_deref(),
            None => None,
            _ => return Err(PersistenceError::IacOperationIdentity),
        };
        let boundary = config.as_ref().map(|(_, value)| *value);
        if config.as_ref().map(|(endpoint, _)| endpoint.as_str()) != desired
            || boundary.and_then(|value| u64::try_from(value).ok())
                != operation.configuration_generation
        {
            return Ok(None);
        }
        Ok(Some(boundary))
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OtlpObservation {
    service_id: Uuid,
    endpoint: Option<String>,
    enabled_at_ms: Option<i64>,
}

pub(super) fn validate(config: &mut ServiceOtlpConfiguration) -> Result<(), PersistenceError> {
    if config.service_id.is_nil() {
        return Err(PersistenceError::InvalidIacOwner);
    }
    config.endpoint = config
        .endpoint
        .as_deref()
        .map(|endpoint| {
            crate::otlp::validate_endpoint(endpoint)
                .map(|url| url.to_string())
                .map_err(|_| PersistenceError::InvalidIacOwner)
        })
        .transpose()?;
    Ok(())
}

pub(super) fn observe(
    conn: &Connection,
    target: &ManagedResource,
) -> Result<Option<OtlpObservation>, PersistenceError> {
    let service_id = target
        .id
        .parse()
        .map_err(|_| PersistenceError::InvalidIacOwner)?;
    let config: Option<(String, i64)> = conn
        .query_row(
            "SELECT endpoint,enabled_at_ms FROM service_otlp_config WHERE service_id=?1",
            [&target.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if config.is_none() && iac_owners::read(conn, target)?.owner.is_none() {
        return Ok(None);
    }
    if config.as_ref().is_some_and(|(_, boundary)| *boundary <= 0) {
        return Err(PersistenceError::IacOperationIdentity);
    }
    Ok(Some(OtlpObservation {
        service_id,
        endpoint: config.as_ref().map(|(endpoint, _)| endpoint.clone()),
        enabled_at_ms: config.map(|(_, boundary)| boundary),
    }))
}

pub(super) fn dependencies(
    conn: &Connection,
    config: &ServiceOtlpConfiguration,
) -> Result<Vec<ResourcePlanDependency>, PersistenceError> {
    let local: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM micro_services WHERE id=?1)
        AND NOT EXISTS(SELECT 1 FROM cluster_services WHERE id=?1)
        AND NOT EXISTS(SELECT 1 FROM cluster_service_workers WHERE id=?1)",
        [config.service_id.to_string()],
        |row| row.get(0),
    )?;
    if !local {
        return Err(PersistenceError::InvalidStackResource);
    }
    let target = ManagedResource {
        kind: "service".into(),
        id: config.service_id.to_string(),
    };
    iac_native::ensure_idle(conn, &target)?;
    super::super::iac_host::ensure_resource_idle(conn, &target)?;
    super::super::otlp_config::validate_owner(
        conn,
        &ManagedResource {
            kind: "service_otlp".into(),
            id: target.id.clone(),
        },
        false,
    )?;
    preflight(conn, config)?;
    // The reference binds service identity and authority, not its mutable application specification.
    Ok(vec![ResourcePlanDependency {
        ownership: iac_owners::read(conn, &target)?,
        configuration_generation: 0,
    }])
}

fn preflight(conn: &Connection, config: &ServiceOtlpConfiguration) -> Result<(), PersistenceError> {
    super::super::otlp_config::validate_configuration(
        conn,
        config.service_id,
        config.endpoint.as_deref(),
    )
}

pub(super) fn differences(
    before: &OtlpObservation,
    desired: &ServiceOtlpConfiguration,
) -> Vec<String> {
    if before.endpoint == desired.endpoint {
        Vec::new()
    } else {
        vec!["endpoint".into()]
    }
}

pub(super) fn write(
    conn: &Connection,
    target: &ManagedResource,
    config: &ServiceOtlpConfiguration,
    now: u64,
) -> Result<(), PersistenceError> {
    preflight(conn, config)?;
    if let Some(endpoint) = &config.endpoint {
        let previous: Option<i64> = conn
            .query_row(
                "SELECT enabled_at_ms FROM service_otlp_config WHERE service_id=?1",
                [&target.id],
                |row| row.get(0),
            )
            .optional()?;
        let boundary = (now as i64)
            .max(previous.unwrap_or(0))
            .checked_add(1)
            .ok_or(PersistenceError::IacOperationIdentity)?;
        conn.execute("INSERT INTO service_otlp_config(service_id,endpoint,enabled_at_ms) VALUES(?1,?2,?3)
            ON CONFLICT(service_id) DO UPDATE SET endpoint=excluded.endpoint,enabled_at_ms=excluded.enabled_at_ms",
            params![target.id,endpoint,boundary])?;
    } else {
        remove(conn, target)?;
    }
    Ok(())
}

pub(super) fn remove(conn: &Connection, target: &ManagedResource) -> Result<(), PersistenceError> {
    conn.execute(
        "DELETE FROM service_otlp_config WHERE service_id=?1",
        [&target.id],
    )?;
    Ok(())
}

pub(super) fn generation(
    conn: &Connection,
    target: &ManagedResource,
) -> Result<Option<u64>, PersistenceError> {
    let value: Option<i64> = conn
        .query_row(
            "SELECT enabled_at_ms FROM service_otlp_config WHERE service_id=?1",
            [&target.id],
            |row| row.get(0),
        )
        .optional()?;
    value
        .map(|value| u64::try_from(value).map_err(|_| PersistenceError::IacOperationIdentity))
        .transpose()
}

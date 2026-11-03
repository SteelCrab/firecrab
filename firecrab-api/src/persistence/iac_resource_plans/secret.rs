//! Encrypted Secret inputs and durable acknowledgement across Vault and SQLite.

use super::*;
use crate::secrets::{Contents, IacPlanInput, IacPlanScope, SecretError, Vault};
use firecrab_stack::{CredentialInput, SecretConfiguration, UploadResourceValue};

mod cleanup;
mod execute;

#[cfg(test)]
mod tests;

pub(super) fn preflight(contents: &Contents, frozen: &Frozen) -> Result<(), PersistenceError> {
    let Some(ResourceConfiguration::Secret(config)) = &frozen.request.desired else {
        return Ok(());
    };
    let id: Uuid = frozen
        .ownership
        .resource
        .id
        .parse()
        .map_err(|_| PersistenceError::IacOperationIdentity)?;
    if contents
        .secrets
        .iter()
        .any(|secret| secret.summary.id != id && secret.summary.name == config.name)
        || (frozen.before.is_none() && contents.secrets.len() >= crate::secrets::MAX_SECRETS)
    {
        return Err(PersistenceError::InvalidStackResource);
    }
    Ok(())
}

fn scope(frozen: &Frozen) -> Result<IacPlanScope, PersistenceError> {
    Ok(IacPlanScope {
        operation_id: frozen.request.operation_id,
        plan_sha256: frozen.summary()?.plan_sha256,
        ownership: frozen.ownership.clone(),
        owner: frozen.request.owner.clone(),
    })
}

fn checked_plan(
    conn: &Connection,
    id: Uuid,
    endpoint: Uuid,
    digest: &str,
) -> Result<Frozen, PersistenceError> {
    if iac_owners::endpoint(conn)? != endpoint {
        return Err(PersistenceError::IacEndpointConflict);
    }
    let frozen = read(conn, id)?.ok_or(PersistenceError::IacOperationIdentity)?;
    if frozen.ownership.resource.kind != "secret"
        || frozen.request.expected_endpoint_id != endpoint
        || frozen.summary()?.plan_sha256 != digest
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    Ok(frozen)
}

/// The public version fences value drift without putting a value/hash in a plan.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SecretObservation {
    id: Uuid,
    name: String,
    description: String,
    version: u64,
    value_source: Option<CredentialInput>,
    last_source: Option<CredentialInput>,
}

pub(super) fn validate(config: &SecretConfiguration) -> Result<(), PersistenceError> {
    crate::secrets::validate_name(&config.name).map_err(|_| PersistenceError::InvalidIacOwner)?;
    if config.description.len() > 1000 {
        return Err(PersistenceError::InvalidIacOwner);
    }
    match &config.value {
        Some(CredentialInput::Secret { id, version })
            if id.is_nil() || *version == 0 || *version > i64::MAX as u64 =>
        {
            Err(PersistenceError::InvalidIacOwner)
        }
        Some(CredentialInput::WriteOnly { version })
            if *version == 0 || *version > i64::MAX as u64 =>
        {
            Err(PersistenceError::InvalidIacOwner)
        }
        _ => Ok(()),
    }
}

pub(super) fn observe(
    conn: &Connection,
    target: &ManagedResource,
    contents: &Contents,
) -> Result<Option<SecretObservation>, PersistenceError> {
    let id: Uuid = target
        .id
        .parse()
        .map_err(|_| PersistenceError::InvalidIacOwner)?;
    let Some(secret) = contents
        .secrets
        .iter()
        .find(|secret| secret.summary.id == id)
    else {
        return Ok(None);
    };
    if secret.summary.version == 0 || secret.summary.version > i64::MAX as u64 {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let row: Option<(String, Option<i64>)> = conn
        .query_row(
            "SELECT value_source,vault_version FROM iac_secret_value_sources WHERE secret_id=?1",
            [&target.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let last_source = row
        .as_ref()
        .map(|(value, _)| decode::<CredentialInput>(value))
        .transpose()?;
    if row
        .as_ref()
        .is_some_and(|(_, version)| version.is_some_and(|version| version < 0))
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let value_source = if row.is_some_and(|(_, version)| {
        version.and_then(|value| u64::try_from(value).ok()) == Some(secret.summary.version)
    }) {
        last_source.clone()
    } else {
        None
    };
    Ok(Some(SecretObservation {
        id,
        name: secret.summary.name.clone(),
        description: secret.summary.description.clone(),
        version: secret.summary.version,
        value_source,
        last_source,
    }))
}

fn needs_value(
    before: Option<&SecretObservation>,
    desired: &SecretConfiguration,
) -> Result<bool, PersistenceError> {
    let Some(input) = &desired.value else {
        return Ok(false);
    };
    let Some(before) = before else {
        return Ok(true);
    };
    if before.value_source.as_ref() == Some(input) {
        return Ok(false);
    }
    if before.last_source.as_ref() == Some(input) {
        return Err(PersistenceError::IacCredentialVersionRequired);
    }
    Ok(true)
}

pub(super) fn differences(
    before: &SecretObservation,
    desired: &SecretConfiguration,
) -> Result<Vec<String>, PersistenceError> {
    let mut fields = Vec::new();
    if before.name != desired.name {
        fields.push("name".into());
    }
    if before.description != desired.description {
        fields.push("description".into());
    }
    if needs_value(Some(before), desired)? {
        fields.push("valueVersion".into());
    }
    Ok(fields)
}

pub(super) fn dependencies(
    conn: &Connection,
    desired: &SecretConfiguration,
    before: Option<&Observation>,
    contents: &Contents,
) -> Result<Vec<ResourcePlanDependency>, PersistenceError> {
    let before = match before {
        Some(Observation::Secret(before)) => Some(before),
        None => None,
        _ => return Err(PersistenceError::InvalidIacOwner),
    };
    if !needs_value(before, desired)? {
        return Ok(Vec::new());
    }
    let Some(CredentialInput::Secret { id, version }) = &desired.value else {
        return Ok(Vec::new());
    };
    if !contents
        .secrets
        .iter()
        .any(|secret| secret.summary.id == *id && secret.summary.version == *version)
    {
        return Err(PersistenceError::InvalidStackResource);
    }
    let resource = ManagedResource {
        kind: "secret".into(),
        id: id.to_string(),
    };
    iac_native::ensure_idle(conn, &resource)?;
    super::super::iac_host::ensure_resource_idle(conn, &resource)?;
    Ok(vec![ResourcePlanDependency {
        ownership: iac_owners::read(conn, &resource)?,
        configuration_generation: *version,
    }])
}

pub(super) fn deletable(
    conn: &Connection,
    target: &ManagedResource,
) -> Result<(), PersistenceError> {
    let referenced:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM micro_services,json_each(spec,'$.secrets') s WHERE s.value=?1)",
        [&target.id],|row|row.get(0))?;
    if referenced {
        return Err(PersistenceError::IacManagedDependent);
    }
    Ok(())
}

impl Store {
    /// A retry validates the same private value under the same encrypted scope.
    pub(crate) fn upload_resource_value(
        &self,
        vault: &Vault,
        id: Uuid,
        input: UploadResourceValue,
        now: u64,
    ) -> Result<(), PersistenceError> {
        crate::secrets::validate_value(&input.value)
            .map_err(|_| PersistenceError::InvalidIacOwner)?;
        self.prune_resource_plan_inputs(vault, now)?;
        vault
            .exclusive(|locked| {
                let result = (|| {
                    let conn = self.lock();
                    let frozen =
                        checked_plan(&conn, id, input.expected_endpoint_id, &input.plan_sha256)?;
                    let Some(ResourceConfiguration::Secret(config)) = &frozen.request.desired
                    else {
                        return Err(PersistenceError::IacOperationIdentity);
                    };
                    if config.value
                        != Some(CredentialInput::WriteOnly {
                            version: input.version,
                        })
                    {
                        return Err(PersistenceError::IacOperationIdentity);
                    }
                    let expected = scope(&frozen)?;
                    if let Some(original) = locked.contents.iac_plan_inputs.get(&id) {
                        if original.scope == expected
                            && original.version == input.version
                            && original.value == input.value
                        {
                            return Ok(false);
                        }
                        return Err(PersistenceError::IacOperationIdentity);
                    }
                    if now >= frozen.expires
                        || locked.contents.iac_plan_executions.contains_key(&id)
                    {
                        return Err(PersistenceError::StaleIacExecution);
                    }
                    let complete: bool = conn.query_row(
                        "SELECT receipt IS NOT NULL FROM iac_resource_plans WHERE operation_id=?1",
                        [id.to_string()],
                        |row| row.get(0),
                    )?;
                    if complete {
                        return Err(PersistenceError::IacOperationIdentity);
                    }
                    authority(&conn, &frozen, Some(&locked.contents))?;
                    if super::observe(&conn, &frozen.ownership.resource, Some(&locked.contents))?
                        != frozen.before
                    {
                        return Err(PersistenceError::StaleIacExecution);
                    }
                    let before = match &frozen.before {
                        Some(Observation::Secret(before)) => Some(before),
                        None => None,
                        _ => return Err(PersistenceError::IacOperationIdentity),
                    };
                    if !needs_value(before, config)? {
                        return Err(PersistenceError::IacCredentialVersionRequired);
                    }
                    locked.contents.iac_plan_inputs.insert(
                        id,
                        IacPlanInput {
                            scope: expected,
                            version: input.version,
                            value: input.value,
                        },
                    );
                    Ok(true)
                })();
                match result {
                    Ok(true) => {
                        locked.commit()?;
                        Ok(Ok(()))
                    }
                    Ok(false) => Ok(Ok(())),
                    Err(error) => Ok(Err(error)),
                }
            })
            .map_err(|_| PersistenceError::IacVaultUnavailable)?
    }

    /// Original plan retries return before loading any current encrypted data.
    pub(crate) fn plan_vault_resource(
        &self,
        vault: &Vault,
        request: PlanResource,
        now: u64,
    ) -> Result<ResourcePlan, PersistenceError> {
        match self.plan_resource(request.clone(), now) {
            Err(PersistenceError::IacVaultUnavailable) => {}
            result => return result,
        }
        vault
            .read(|contents| Ok(self.plan_resource_with_vault(request, now, Some(contents))))
            .map_err(|_| PersistenceError::IacVaultUnavailable)?
    }

    /// Native metadata edits preserve acknowledged input provenance; value writes invalidate it.
    pub(crate) fn note_native_secret_write(
        &self,
        id: Uuid,
        version: u64,
        value_changed: bool,
    ) -> Result<(), SecretError> {
        let version = i64::try_from(version).map_err(|_| SecretError::Storage)?;
        let conn = self.lock();
        conn.execute("UPDATE iac_secret_value_sources SET vault_version=CASE WHEN ?2=0 AND vault_version=?3 THEN ?3+1 ELSE NULL END WHERE secret_id=?1",
            params![id.to_string(),value_changed,version]).map_err(|_|SecretError::Storage)?;
        Ok(())
    }
}

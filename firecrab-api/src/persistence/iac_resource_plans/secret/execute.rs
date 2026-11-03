//! File effects commit once; a lost SQLite acknowledgement retains exact recovery authority.

use super::*;
use crate::secrets::{IacPlanEffect, IacPlanExecution, LockedVault, Secret, SecretSummary};

fn operation(conn: &Connection, id: Uuid) -> Result<Option<NativeOperation>, PersistenceError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT status FROM iac_native_operations WHERE operation_id=?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    raw.map(|raw| decode(&raw)).transpose()
}

fn completed(conn: &Connection, id: Uuid) -> Result<Option<ResourcePlanReceipt>, PersistenceError> {
    let raw: Option<String> = conn.query_row(
        "SELECT receipt FROM iac_resource_plans WHERE operation_id=?1",
        [id.to_string()],
        |row| row.get(0),
    )?;
    raw.map(|raw| decode(&raw)).transpose()
}

fn before(frozen: &Frozen) -> Result<Option<&SecretObservation>, PersistenceError> {
    match &frozen.before {
        Some(Observation::Secret(value)) => Ok(Some(value)),
        None => Ok(None),
        _ => Err(PersistenceError::IacOperationIdentity),
    }
}

fn capture(locked: &LockedVault<'_>, frozen: &Frozen) -> Result<Option<String>, PersistenceError> {
    let Some(ResourceConfiguration::Secret(desired)) = &frozen.request.desired else {
        return Ok(None);
    };
    if !needs_value(before(frozen)?, desired)? {
        return Ok(None);
    }
    let value = match &desired.value {
        Some(CredentialInput::Secret { id, version }) => locked
            .contents
            .secrets
            .iter()
            .find(|secret| secret.summary.id == *id && secret.summary.version == *version)
            .map(|secret| secret.value.clone())
            .ok_or(PersistenceError::StaleIacExecution)?,
        Some(CredentialInput::WriteOnly { version }) => {
            let original = locked
                .contents
                .iac_plan_inputs
                .get(&frozen.request.operation_id)
                .ok_or(PersistenceError::IacCredentialVersionRequired)?;
            if original.scope != scope(frozen)? || original.version != *version {
                return Err(PersistenceError::IacOperationIdentity);
            }
            original.value.clone()
        }
        None => return Err(PersistenceError::IacCredentialVersionRequired),
    };
    crate::secrets::validate_value(&value).map_err(|_| PersistenceError::InvalidIacOwner)?;
    Ok(Some(value))
}

fn guard(conn: &Connection, frozen: &Frozen, contents: &Contents) -> Result<(), PersistenceError> {
    if iac_owners::read(conn, &frozen.ownership.resource)? != frozen.ownership {
        return Err(PersistenceError::StaleIacOwner);
    }
    if super::super::observe(conn, &frozen.ownership.resource, Some(contents))? != frozen.before {
        return Err(PersistenceError::StaleIacExecution);
    }
    preflight(contents, frozen)?;
    if frozen.request.desired.is_none()
        && frozen
            .request
            .removal
            .is_some_and(|policy| policy.delete_data)
    {
        deletable(conn, &frozen.ownership.resource)?;
    }
    Ok(())
}

fn lease(
    store: &Store,
    frozen: &Frozen,
    contents: &Contents,
    now: u64,
) -> Result<(), PersistenceError> {
    let mut conn = store.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let id = frozen.request.operation_id;
    if let Some(existing) = operation(&tx, id)? {
        let reserved: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM iac_resource_plan_executions WHERE operation_id=?1 AND kind=?2 AND resource_id=?3 AND manager=?4 AND owner_id=?5 AND logical_key=?6)",
            params![id.to_string(),frozen.ownership.resource.kind,frozen.ownership.resource.id,
                if frozen.request.owner.manager==Manager::Stack {"stack"}else{"terraform"},
                frozen.request.owner.id.to_string(),frozen.request.owner.key], |row|row.get(0))?;
        if !reserved
            || existing.ownership != frozen.ownership
            || existing.action != NativeAction::ApplyResourcePlan
            || !matches!(
                existing.state,
                NativeOperationState::Running | NativeOperationState::NeedsRecovery
            )
            || iac_owners::read(&tx, &frozen.ownership.resource)? != frozen.ownership
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        return Ok(());
    }
    if now >= frozen.expires || now > i64::MAX as u64 {
        return Err(PersistenceError::StaleIacExecution);
    }
    authority(&tx, frozen, Some(contents))?;
    guard(&tx, frozen, contents)?;
    tx.execute("INSERT INTO iac_resource_plan_executions(operation_id,kind,resource_id,manager,owner_id,logical_key) VALUES(?1,?2,?3,?4,?5,?6)",
        params![id.to_string(),frozen.ownership.resource.kind,frozen.ownership.resource.id,
            if frozen.request.owner.manager==Manager::Stack {"stack"}else{"terraform"},
            frozen.request.owner.id.to_string(),frozen.request.owner.key])?;
    iac_native::save(
        &tx,
        &NativeOperation {
            operation_id: id,
            ownership: frozen.ownership.clone(),
            action: NativeAction::ApplyResourcePlan,
            state: NativeOperationState::Running,
            error_code: None,
            execution_generation: 0,
            configuration_generation: None,
            retain_disks: None,
        },
    )?;
    tx.commit()?;
    Ok(())
}

fn effect(
    store: &Store,
    locked: &mut LockedVault<'_>,
    frozen: &Frozen,
) -> Result<(), PersistenceError> {
    let id = frozen.request.operation_id;
    let original = locked
        .contents
        .iac_plan_executions
        .get(&id)
        .ok_or(PersistenceError::IacOperationIdentity)?
        .clone();
    if original.scope != scope(frozen)? {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let IacPlanEffect::Prepared { value, at_ms } = original.effect else {
        return Ok(());
    };
    guard(&store.lock(), frozen, &locked.contents)?;
    let target: Uuid = frozen
        .ownership
        .resource
        .id
        .parse()
        .map_err(|_| PersistenceError::IacOperationIdentity)?;
    let prior = before(frozen)?;
    let mut value_source = prior.and_then(|value| value.value_source.clone());
    let mut last_source = prior.and_then(|value| value.last_source.clone());
    let version = match &frozen.request.desired {
        Some(ResourceConfiguration::Secret(desired)) => {
            if frozen.summary()?.action == ResourcePlanAction::Noop {
                Some(prior.ok_or(PersistenceError::IacOperationIdentity)?.version)
            } else {
                let version = prior
                    .map_or(Some(1), |prior| prior.version.checked_add(1))
                    .filter(|value| *value <= i64::MAX as u64)
                    .ok_or(PersistenceError::IacOperationIdentity)?;
                if value.is_some() {
                    value_source = desired.value.clone();
                    last_source = desired.value.clone();
                }
                if let Some(existing) = locked
                    .contents
                    .secrets
                    .iter_mut()
                    .find(|secret| secret.summary.id == target)
                {
                    existing.summary.name = desired.name.clone();
                    existing.summary.description = desired.description.clone();
                    existing.summary.version = version;
                    existing.summary.updated_at = at_ms;
                    if let Some(value) = value {
                        existing.value = value;
                        existing.summary.source = None;
                    }
                } else {
                    locked.contents.secrets.push(Secret {
                        summary: SecretSummary {
                            id: target,
                            name: desired.name.clone(),
                            description: desired.description.clone(),
                            version,
                            created_at: at_ms,
                            updated_at: at_ms,
                            source: None,
                        },
                        value: value.ok_or(PersistenceError::IacCredentialVersionRequired)?,
                    });
                }
                Some(version)
            }
        }
        None if frozen
            .request
            .removal
            .is_some_and(|policy| policy.delete_data) =>
        {
            locked
                .contents
                .secrets
                .retain(|secret| secret.summary.id != target);
            value_source = None;
            None
        }
        None => Some(prior.ok_or(PersistenceError::IacOperationIdentity)?.version),
        _ => return Err(PersistenceError::IacOperationIdentity),
    };
    locked.contents.iac_plan_inputs.remove(&id);
    locked.contents.iac_plan_executions.insert(
        id,
        IacPlanExecution {
            scope: original.scope,
            effect: IacPlanEffect::Applied {
                version,
                value_source,
                last_source,
            },
        },
    );
    locked
        .commit()
        .map_err(|_| PersistenceError::IacVaultUnavailable)
}

fn acknowledge(
    store: &Store,
    locked: &LockedVault<'_>,
    frozen: &Frozen,
) -> Result<ResourcePlanReceipt, PersistenceError> {
    let id = frozen.request.operation_id;
    let mut conn = store.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(receipt) = completed(&tx, id)? {
        return Ok(receipt);
    }
    let original = locked
        .contents
        .iac_plan_executions
        .get(&id)
        .ok_or(PersistenceError::IacOperationIdentity)?;
    if original.scope != scope(frozen)? {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let IacPlanEffect::Applied {
        version,
        value_source,
        last_source,
    } = &original.effect
    else {
        return Err(PersistenceError::IacOperationIdentity);
    };
    let mut operation = operation(&tx, id)?.ok_or(PersistenceError::IacOperationIdentity)?;
    if operation.ownership != frozen.ownership
        || operation.action != NativeAction::ApplyResourcePlan
        || !matches!(
            operation.state,
            NativeOperationState::Running | NativeOperationState::NeedsRecovery
        )
        || iac_owners::read(&tx, &frozen.ownership.resource)? != frozen.ownership
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let actual = locked
        .contents
        .secrets
        .iter()
        .find(|secret| secret.summary.id.to_string() == frozen.ownership.resource.id);
    if actual.map(|secret| secret.summary.version) != *version {
        return Err(PersistenceError::StaleIacExecution);
    }
    if let Some(actual) = actual {
        let metadata_matches = match &frozen.request.desired {
            Some(ResourceConfiguration::Secret(desired)) => {
                actual.summary.name == desired.name
                    && actual.summary.description == desired.description
            }
            None => before(frozen)?.is_some_and(|prior| {
                actual.summary.name == prior.name && actual.summary.description == prior.description
            }),
            _ => false,
        };
        if !metadata_matches {
            return Err(PersistenceError::StaleIacExecution);
        }
    }
    if let Some(last) = last_source {
        tx.execute("INSERT INTO iac_secret_value_sources(secret_id,value_source,vault_version) VALUES(?1,?2,?3) ON CONFLICT(secret_id) DO UPDATE SET value_source=excluded.value_source,vault_version=excluded.vault_version",
            params![frozen.ownership.resource.id,encode(last)?,if value_source.is_some(){version.map(|value|value as i64)}else{None}])?;
    }
    let ownership = save_owner(
        &tx,
        &frozen.ownership,
        frozen
            .request
            .desired
            .as_ref()
            .map(|_| &frozen.request.owner),
    )?;
    let receipt = ResourcePlanReceipt {
        plan: frozen.summary()?,
        ownership,
        revision_id: None,
    };
    operation.state = NativeOperationState::Succeeded;
    operation.error_code = None;
    operation.configuration_generation = *version;
    iac_native::save(&tx, &operation)?;
    tx.execute(
        "UPDATE iac_resource_plans SET receipt=?2 WHERE operation_id=?1 AND receipt IS NULL",
        params![id.to_string(), encode(&receipt)?],
    )?;
    tx.execute(
        "DELETE FROM iac_resource_plan_executions WHERE operation_id=?1",
        [id.to_string()],
    )?;
    tx.commit()?;
    Ok(receipt)
}

fn run(
    store: &Store,
    locked: &mut LockedVault<'_>,
    frozen: &Frozen,
    now: u64,
) -> Result<ResourcePlanReceipt, PersistenceError> {
    let id = frozen.request.operation_id;
    // A second process can have completed while this caller waited for the vault lock.
    if let Some(receipt) = completed(&store.lock(), id)? {
        return Ok(receipt);
    }
    if !locked.contents.iac_plan_executions.contains_key(&id) {
        if now >= frozen.expires {
            return Err(PersistenceError::StaleIacExecution);
        }
        {
            let conn = store.lock();
            authority(&conn, frozen, Some(&locked.contents))?;
            guard(&conn, frozen, &locked.contents)?;
        }
        let value = capture(locked, frozen)?;
        locked.contents.iac_plan_executions.insert(
            id,
            IacPlanExecution {
                scope: scope(frozen)?,
                effect: IacPlanEffect::Prepared { value, at_ms: now },
            },
        );
        locked
            .commit()
            .map_err(|_| PersistenceError::IacVaultUnavailable)?;
    }
    lease(store, frozen, &locked.contents, now)?;
    effect(store, locked, frozen)?;
    let receipt = acknowledge(store, locked, frozen)?;
    locked.contents.iac_plan_inputs.remove(&id);
    locked.contents.iac_plan_executions.remove(&id);
    // The immutable public receipt is already durable even if private metadata pruning fails.
    let _ = locked.commit();
    Ok(receipt)
}

impl Store {
    pub(crate) fn apply_vault_resource_plan(
        &self,
        vault: &Vault,
        id: Uuid,
        input: &ApplyResourcePlan,
        now: u64,
    ) -> Result<ResourcePlanReceipt, PersistenceError> {
        let frozen = {
            let conn = self.lock();
            let frozen = checked_plan(&conn, id, input.expected_endpoint_id, &input.plan_sha256)?;
            if let Some(receipt) = completed(&conn, id)? {
                return Ok(receipt);
            }
            frozen
        };
        let result = vault
            .exclusive(|locked| Ok(run(self, locked, &frozen, now)))
            .map_err(|_| PersistenceError::IacVaultUnavailable)?;
        if result.is_err() {
            let conn = self.lock();
            if let Some(mut operation) = operation(&conn, id)?
                && matches!(
                    operation.state,
                    NativeOperationState::Running | NativeOperationState::NeedsRecovery
                )
            {
                operation.state = NativeOperationState::NeedsRecovery;
                operation.error_code = Some("iac_private_effect_unconfirmed".into());
                iac_native::save(&conn, &operation)?;
            }
        }
        result
    }
}

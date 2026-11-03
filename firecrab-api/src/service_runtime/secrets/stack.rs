//! Freeze Stack Secret references before admission; retries never resolve newer values.

use super::*;
use crate::persistence::{PersistenceError, Store};
use firecrab_stack::{Action, ApplyStatus, Inventory, ResourceKind, SecretInputPin, Stack};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Private original inputs. The public journal contains only reference identities.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StackServiceCredentials {
    apply: Uuid,
    stack: Uuid,
    service: Uuid,
    desired_hash: String,
    spec_sha256: String,
    references: BTreeMap<String, SecretInputPin>,
    /// Cleanup removes values but retains the identity so an old worker cannot recapture.
    pub(super) values: Option<BTreeMap<String, String>>,
}

fn requested(stack: &Stack) -> bool {
    stack
        .micro_services
        .values()
        .any(|service| !service.config.secrets.is_empty())
}

fn reference(
    store: &Store,
    contents: &Contents,
    id: Uuid,
) -> Result<Option<SecretInputPin>, PersistenceError> {
    contents
        .secrets
        .iter()
        .find(|secret| secret.summary.id == id)
        .map(|secret| {
            Ok(SecretInputPin {
                id,
                version: secret.summary.version,
                ownership: store.iac_resource_owner(&firecrab_stack::ManagedResource {
                    kind: "secret".into(),
                    id: id.to_string(),
                })?,
            })
        })
        .transpose()
}

/// Lock order is always Vault then Store, including the final admission transaction.
pub(crate) fn with_inventory<T>(
    state: &AppState,
    stack: &Stack,
    operation: impl FnOnce(Inventory) -> Result<T, PersistenceError>,
) -> Result<T, PersistenceError> {
    let observe = |contents: Option<&Contents>| {
        let mut inventory = crate::handlers::stacks::inventory(&state.store, &state.templates)?;
        if let Some(contents) = contents {
            for service in &inventory.services {
                if let Some(deployment) = &service.deployment
                    && let Some(scope) = contents.stack_service_credentials.get(&deployment.id)
                {
                    inventory
                        .deployment_secret_inputs
                        .insert(deployment.id, scope.references.clone());
                }
            }
            for id in stack
                .micro_services
                .values()
                .flat_map(|service| service.config.secrets.values())
            {
                if let Some(pin) = reference(&state.store, contents, *id)? {
                    inventory.secret_inputs.insert(*id, pin);
                }
            }
        }
        operation(inventory)
    };
    if !requested(stack) {
        return observe(None);
    }
    state
        .secrets
        .read(|contents| Ok(observe(Some(contents))))
        .map_err(|_| PersistenceError::IacVaultUnavailable)?
}

fn spec_hash(spec: &MicroServiceSpec) -> Result<String, SecretError> {
    let value = serde_json::to_value(spec).map_err(|_| SecretError::Storage)?;
    let bytes = serde_json::to_vec(&value).map_err(|_| SecretError::Storage)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// Persist private snapshots before publishing the parent journal. An unaccepted
/// snapshot cannot grant execution authority and cannot change its original inputs.
pub(crate) fn prepare(
    state: &AppState,
    stack: &Stack,
    status: &ApplyStatus,
) -> Result<(), PersistenceError> {
    if !status.changes.iter().any(|step| {
        step.change.kind == ResourceKind::Service
            && matches!(
                step.change.action,
                Action::Create | Action::Update | Action::Deploy
            )
            && stack
                .micro_services
                .get(&step.change.key)
                .is_some_and(|service| !service.config.secrets.is_empty())
    }) {
        return Ok(());
    }
    state
        .secrets
        .write(|contents| {
            for step in &status.changes {
                if step.change.kind != ResourceKind::Service
                    || !matches!(
                        step.change.action,
                        Action::Create | Action::Update | Action::Deploy
                    )
                {
                    continue;
                }
                let declared = &stack.micro_services[&step.change.key].config;
                if declared.secrets.is_empty() {
                    continue;
                }
                let service = step.change.resource_id.ok_or(SecretError::Conflict)?;
                let current = state
                    .store
                    .service_deployment(service)
                    .map_err(|_| SecretError::Storage)?;
                let spec = firecrab_stack::effective_service_config(declared, current.as_ref());
                let references = status
                    .inputs
                    .service_secrets
                    .get(&step.change.key)
                    .ok_or(SecretError::Conflict)?;
                if !references.keys().eq(spec.secrets.keys()) {
                    return Err(SecretError::Conflict);
                }
                for (name, id) in &spec.secrets {
                    if reference(&state.store, contents, *id)
                        .map_err(|_| SecretError::Storage)?
                        .as_ref()
                        != references.get(name)
                    {
                        return Err(SecretError::Conflict);
                    }
                }
                let child =
                    crate::service_runtime::stack_deployment_id(status.operation_id, service);
                let captured = StackServiceCredentials {
                    apply: status.operation_id,
                    stack: status.stack_id,
                    service,
                    desired_hash: firecrab_stack::desired_hash(stack),
                    spec_sha256: spec_hash(&spec)?,
                    references: references.clone(),
                    values: Some(resolve(contents, &spec)?),
                };
                if let Some(original) = contents.stack_service_credentials.get(&child) {
                    if original != &captured {
                        return Err(SecretError::Conflict);
                    }
                } else {
                    contents.stack_service_credentials.insert(child, captured);
                }
            }
            Ok(())
        })
        .map_err(|error| match error {
            SecretError::Conflict => PersistenceError::StaleStackPlan,
            _ => PersistenceError::IacVaultUnavailable,
        })
}

fn authorized(
    store: &Store,
    scope: &StackServiceCredentials,
    child: Uuid,
    service: Uuid,
    spec: &MicroServiceSpec,
) -> Result<(), SecretError> {
    if scope.service != service
        || crate::service_runtime::stack_deployment_id(scope.apply, service) != child
        || spec_hash(spec)? != scope.spec_sha256
        || !scope
            .references
            .iter()
            .all(|(name, pin)| spec.secrets.get(name) == Some(&pin.id))
        || scope.references.len() != spec.secrets.len()
        || scope.values.is_none()
    {
        return Err(SecretError::Conflict);
    }
    let parent = store
        .stored_stack_apply(scope.apply)
        .map_err(|_| SecretError::Storage)?
        .ok_or(SecretError::Conflict)?;
    if parent.status.stack_id != scope.stack
        || parent.desired_hash != scope.desired_hash
        || !parent.status.changes.iter().any(|step| {
            step.change.kind == ResourceKind::Service
                && step.change.resource_id == Some(service)
                && parent.status.inputs.service_secrets.get(&step.change.key)
                    == Some(&scope.references)
        })
        || !store
            .stack_service_mutation_allowed(service, Some(scope.apply))
            .map_err(|_| SecretError::Storage)?
    {
        return Err(SecretError::Conflict);
    }
    Ok(())
}

/// The private caller must present the accepted parent; HTTP IDs are insufficient.
pub(crate) async fn preflight(
    state: &AppState,
    apply: Uuid,
    service: Uuid,
    child: Uuid,
    spec: &MicroServiceSpec,
    request: Uuid,
) -> Result<(), AppError> {
    if spec.secrets.is_empty() {
        return Ok(());
    }
    let store = state.store.clone();
    let spec = spec.clone();
    vault(state, move |vault| {
        vault.read(|contents| {
            let scope = contents
                .stack_service_credentials
                .get(&child)
                .ok_or(SecretError::Conflict)?;
            if scope.apply != apply {
                return Err(SecretError::Conflict);
            }
            authorized(&store, scope, child, service, &spec)
        })
    })
    .await
    .map_err(|_| {
        AppError::conflict(
            "stack_credentials_unavailable",
            "original accepted deployment credentials are unavailable or do not match",
            request,
        )
    })
}

/// Return false only for a native/controller deployment with no Stack scope.
pub(super) fn capture(
    store: &Store,
    contents: &mut Contents,
    deployment: &MicroServiceDeployment,
) -> Result<bool, SecretError> {
    let Some(scope) = contents.stack_service_credentials.get(&deployment.id) else {
        return Ok(false);
    };
    authorized(
        store,
        scope,
        deployment.id,
        deployment.service_id,
        &deployment.spec,
    )?;
    let values = scope.values.as_ref().ok_or(SecretError::Conflict)?;
    if let Some(existing) = contents.service_environments.get(&deployment.id) {
        if existing != values {
            return Err(SecretError::Conflict);
        }
    } else {
        contents
            .service_environments
            .insert(deployment.id, values.clone());
    }
    Ok(true)
}

#[cfg(test)]
mod tests;

//! Keep deployment Secrets encrypted on the host and resolve them only for guest boot.

use std::collections::BTreeMap;

use firecrab_api_types::{MicroServiceDeployment, MicroServiceSpec};
use uuid::Uuid;

use crate::error::AppError;
use crate::handlers::vms::validate_vm_env;
use crate::model::VmRecord;
use crate::secrets::{Contents, SecretError, Vault};
use crate::state::AppState;

/// Reject missing or incompatible Secrets before accepting deployment work.
pub(super) async fn preflight(
    state: &AppState,
    spec: &MicroServiceSpec,
    request_id: Uuid,
) -> Result<(), AppError> {
    if spec.secrets.is_empty() {
        return Ok(());
    }
    let spec = spec.clone();
    vault(state, move |vault| {
        vault.read(|contents| resolve(contents, &spec).map(|_| ()))
    })
    .await
    .map_err(|error| match error {
        SecretError::NotFound | SecretError::Invalid(_) => AppError::validation(
            BTreeMap::from([("secrets".into(), error.to_string())]),
            request_id,
        ),
        _ => AppError::unavailable(
            "PipeSecrets storage or its encryption key is unavailable.",
            request_id,
        ),
    })
}

/// Capture one atomic set after queue admission and before any deployment side effect.
pub(super) async fn capture(
    state: &AppState,
    deployment: &MicroServiceDeployment,
) -> Result<(), String> {
    if deployment.spec.secrets.is_empty() {
        return Ok(());
    }
    let spec = deployment.spec.clone();
    let id = deployment.id;
    vault(state, move |vault| {
        vault.write(|contents| {
            if contents.service_environments.contains_key(&id) {
                return Ok(());
            }
            let env = resolve(contents, &spec)?;
            contents.service_environments.insert(id, env);
            Ok(())
        })
    })
    .await
    .map_err(|_| {
        "Could not prepare deployment Secrets; check references, values and the vault.".into()
    })
}

/// Release the private snapshot only after every owned replica has been removed.
pub(super) async fn remove(
    state: &AppState,
    deployment: &MicroServiceDeployment,
) -> Result<(), String> {
    if deployment.spec.secrets.is_empty() {
        return Ok(());
    }
    let id = deployment.id;
    vault(state, move |vault| {
        // Most calls follow an already completed cleanup; avoid rewriting the vault.
        if !vault.read(|contents| Ok(contents.service_environments.contains_key(&id)))? {
            return Ok(());
        }
        vault.write(|contents| {
            contents.service_environments.remove(&id);
            Ok(())
        })
    })
    .await
    .map_err(|_| "Could not clear deployment Secrets; retry Stop.".into())
}

/// Merge private values for boot without changing the VM record or its public response.
pub(crate) async fn guest_env(
    state: &AppState,
    vm: &VmRecord,
) -> Result<BTreeMap<String, String>, String> {
    let store = state.store.clone();
    let id = vm.id;
    let deployment = tokio::task::spawn_blocking(move || store.service_deployment_for_vm(id))
        .await
        .map_err(|_| "Could not read service ownership")?
        .map_err(|_| "Could not read service ownership")?;
    let Some(deployment) = deployment.filter(|record| !record.spec.secrets.is_empty()) else {
        return Ok(vm.env.clone());
    };
    let mut env = vm.env.clone();
    vault(state, move |vault| {
        vault.read(|contents| {
            let secrets =
                contents
                    .service_environments
                    .get(&deployment.id)
                    .ok_or(SecretError::Invalid(
                        "deployment Secrets are unavailable; stop and redeploy the service",
                    ))?;
            if !secrets.keys().eq(deployment.spec.secrets.keys()) {
                return Err(SecretError::Invalid(
                    "deployment Secret references do not match",
                ));
            }
            env.extend(secrets.clone());
            if validate_vm_env(&env).is_some() {
                return Err(SecretError::Invalid(
                    "deployment Secret environment is invalid",
                ));
            }
            Ok(env)
        })
    })
    .await
    .map_err(|_| {
        "Could not load deployment Secrets; check the vault or stop and redeploy the service."
            .into()
    })
}

/// Resolve all references under one vault lock and validate the combined guest environment.
pub(super) fn resolve(
    contents: &Contents,
    spec: &MicroServiceSpec,
) -> Result<BTreeMap<String, String>, SecretError> {
    let mut secrets = BTreeMap::new();
    for (key, id) in &spec.secrets {
        if spec.env.contains_key(key) {
            return Err(SecretError::Invalid(
                "an environment name must not appear in both env and secrets",
            ));
        }
        let secret = contents
            .secrets
            .iter()
            .find(|secret| secret.summary.id == *id)
            .ok_or(SecretError::Invalid(
                "a referenced PipeSecret does not exist",
            ))?;
        secrets.insert(key.clone(), secret.value.clone());
    }
    let mut combined = spec.env.clone();
    combined.extend(secrets.clone());
    if validate_vm_env(&combined).is_some() {
        return Err(SecretError::Invalid(
            "env and secrets must contain at most 64 valid variables; each value must fit 4096 bytes and contain no NUL",
        ));
    }
    Ok(secrets)
}

/// Keep locked file I/O and encryption off the async executor.
pub(super) async fn vault<T: Send + 'static>(
    state: &AppState,
    operation: impl FnOnce(&Vault) -> Result<T, SecretError> + Send + 'static,
) -> Result<T, SecretError> {
    let vault = state.secrets.clone();
    tokio::task::spawn_blocking(move || operation(&vault))
        .await
        .map_err(|_| SecretError::Storage)?
}

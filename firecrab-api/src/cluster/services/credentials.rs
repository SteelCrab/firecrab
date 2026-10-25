//! Capture controller credentials once; send only the scoped environment over mutual TLS.
use super::*;
use crate::secrets::{ClusterCredentials, SecretError};
use crate::service_runtime::secrets::{resolve, vault};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Private RPC body. Deliberately has no Debug implementation and is never returned.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct WorkerRequest {
    pub intent: ServiceWorkerIntent,
    #[serde(default)]
    pub credential_values: BTreeMap<String, String>,
}

fn snapshot_key(
    references: &BTreeMap<String, ClusterServiceCredential>,
) -> Result<String, ClusterError> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(references)?)
    ))
}

/// External state must use a remote service, never a guest path or loopback endpoint.
pub(super) fn validate_external(
    state: &ClusterServiceState,
    env: &BTreeMap<String, String>,
) -> Result<(), ClusterError> {
    for key in [&state.sessions, &state.uploads, &state.database]
        .into_iter()
        .flatten()
    {
        let Some(value) = env.get(key) else { continue };
        let valid = reqwest::Url::parse(value).ok().is_some_and(|url| {
            matches!(
                url.scheme(),
                "http" | "https" | "redis" | "rediss" | "postgres" | "postgresql" | "mysql" | "s3"
            ) && url.host_str().is_some_and(|host| {
                let host = host
                    .trim_matches(['[', ']'])
                    .trim_end_matches('.')
                    .to_ascii_lowercase();
                if host == "localhost" || host.ends_with(".localhost") {
                    return false;
                }
                match host.parse::<std::net::IpAddr>() {
                    Ok(std::net::IpAddr::V4(ip)) => !ip.is_loopback() && !ip.is_unspecified(),
                    Ok(std::net::IpAddr::V6(ip)) => {
                        !ip.is_loopback()
                            && !ip.is_unspecified()
                            && ip.to_ipv4_mapped().is_none_or(|mapped| {
                                !mapped.is_loopback() && !mapped.is_unspecified()
                            })
                    }
                    Err(_) => !host.contains(':'),
                }
            })
        });
        if !valid {
            return Err(ClusterError::Invalid("external state requires a remote HTTP, object-store, session-store or database URL; host-local durable state is unsupported".into()));
        }
    }
    Ok(())
}

/// All references resolve before committing any host deployment intent.
pub(super) async fn prepare(
    state: &AppState,
    spec: &ClusterServiceSpec,
) -> Result<(), ClusterError> {
    if spec.credentials.is_empty() {
        return Ok(());
    }
    let key = snapshot_key(&spec.credentials)?;
    let spec = spec.clone();
    vault(state, move |vault| {
        vault.write(|contents| {
            let values = if let Some(snapshot) = contents.cluster_credentials.get(&key) {
                if snapshot.references != spec.credentials {
                    return Err(SecretError::Invalid("credential snapshot references changed"));
                }
                snapshot.values.clone()
            } else {
                for reference in spec.credentials.values() {
                    if !contents.secrets.iter().any(|secret| {
                        secret.summary.id == reference.secret_id
                            && secret.summary.version == reference.version
                    }) {
                        return Err(SecretError::Invalid("controller credential is missing or its version changed"));
                    }
                }
                let mut local = spec.service.clone();
                local.secrets = spec.credentials.iter()
                    .map(|(name, reference)| (name.clone(), reference.secret_id)).collect();
                resolve(contents, &local)?
            };
            let mut env = spec.service.env.clone();
            env.extend(values.clone());
            if crate::handlers::vms::validate_vm_env(&env).is_some()
                || validate_external(&spec.state, &env).is_err()
            {
                return Err(SecretError::Invalid("invalid credential environment or external-state URL"));
            }
            contents.cluster_credentials.entry(key).or_insert(ClusterCredentials {
                references: spec.credentials,
                values,
            });
            Ok(())
        })
    }).await.map_err(|error| match error {
        SecretError::Invalid(_) | SecretError::NotFound => ClusterError::Invalid("controller credentials are missing, stale or incompatible with the service state contract".into()),
        _ => ClusterError::Task,
    })
}

pub(super) async fn values(
    state: &AppState,
    references: &BTreeMap<String, ClusterServiceCredential>,
) -> Result<BTreeMap<String, String>, ClusterError> {
    if references.is_empty() {
        return Ok(BTreeMap::new());
    }
    let key = snapshot_key(references)?;
    let references = references.clone();
    vault(state, move |vault| {
        vault.read(|contents| {
            let snapshot = contents
                .cluster_credentials
                .get(&key)
                .filter(|snapshot| snapshot.references == references)
                .ok_or(SecretError::Invalid(
                    "pinned controller credentials are unavailable",
                ))?;
            Ok(snapshot.values.clone())
        })
    })
    .await
    .map_err(|_| ClusterError::Task)
}

/// Preserve the existing host-local encrypted deployment injection path.
pub(super) fn validate_values(request: &WorkerRequest) -> Result<(), ClusterError> {
    let intent = &request.intent;
    if !intent
        .credentials
        .keys()
        .eq(request.credential_values.keys())
    {
        return Err(ClusterError::Invalid(
            "worker credential names do not match their scope".into(),
        ));
    }
    let mut env = intent.spec.env.clone();
    env.extend(request.credential_values.clone());
    if crate::handlers::vms::validate_vm_env(&env).is_some() {
        return Err(ClusterError::Invalid(
            "invalid worker credential environment".into(),
        ));
    }
    validate_external(&intent.state, &env)
}

pub(super) async fn install(state: &AppState, request: &WorkerRequest) -> Result<(), ClusterError> {
    let intent = &request.intent;
    if intent.credentials.is_empty() {
        return Ok(());
    }
    let id = intent.deployment_id;
    let values = request.credential_values.clone();
    vault(state, move |vault| {
        vault.write(|contents| {
            if let Some(existing) = contents.service_environments.get(&id) {
                if existing != &values {
                    return Err(SecretError::Invalid("deployment credentials are immutable"));
                }
            } else {
                contents.service_environments.insert(id, values);
            }
            Ok(())
        })
    })
    .await
    .map_err(|_| ClusterError::Task)
}

/// Include snapshots staged before a failed local admission, which have no deployment row.
pub(super) async fn remove_stopped(
    state: &AppState,
    receipt: &ServiceWorkerReceipt,
) -> Result<(), ClusterError> {
    if !state.secrets.is_initialized() {
        return Ok(());
    }
    let ids = receipt.deployment_ids.clone();
    vault(state, move |vault| {
        if !vault.read(|contents| {
            Ok(ids
                .iter()
                .any(|id| contents.service_environments.contains_key(id)))
        })? {
            return Ok(());
        }
        vault.write(|contents| {
            for id in ids {
                contents.service_environments.remove(&id);
            }
            Ok(())
        })
    })
    .await
    .map_err(|_| ClusterError::Task)
}

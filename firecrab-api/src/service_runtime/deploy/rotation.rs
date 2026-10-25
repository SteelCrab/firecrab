//! Reconcile changed private values through the existing, capacity-limited Deploy worker.

use super::*;
use crate::secrets::{SecretError, ServiceRotation};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const MAX_ATTEMPTS: u8 = 5;
const ADMISSION_FAILED: &str = "Secret rotation could not enter Deploy; check Auth, Secret references and Nginx. Automatic retries are limited to five attempts.";
const PAUSED: &str =
    "Secret rotation paused after five failed attempts; fix the cause and use Deploy to retry.";

struct Observation {
    deployment: MicroServiceDeployment,
    rotation: Option<ServiceRotation>,
    change: Change,
}

enum Change {
    Current,
    Invalid(&'static str),
    Changed(String),
}

enum Action {
    Submit(ServiceRotation),
    Pause,
}

/// Secret edits and imports need no transient event: desired values and live snapshots are durable.
pub(crate) fn start(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut warned = false;
        loop {
            interval.tick().await;
            match reconcile(&state, crate::secrets::now_ms()).await {
                Ok(_) => warned = false,
                Err(_) if !warned => {
                    tracing::warn!(
                        "Could not reconcile Secret rotation; check the vault and service database"
                    );
                    warned = true;
                }
                Err(_) => {}
            }
        }
    });
}

async fn reconcile(state: &AppState, now: u64) -> Result<usize, String> {
    if !state.secrets.is_initialized() {
        return Ok(0);
    }
    let store = state.store.clone();
    let deployments = database(Uuid::new_v4(), move || {
        let mut records = Vec::new();
        for service in store.list_micro_services()? {
            if let Some(record) = store.service_deployment(service.id)? {
                records.push(record);
            }
        }
        Ok(records)
    })
    .await
    .map_err(|_| "Could not read Secret rotation owners")?;
    let (observations, stale) = secrets::vault(state, move |vault| {
        vault.read(|contents| {
            let owners = deployments.iter().filter(|record| {
                !record.spec.secrets.is_empty() && record.phase != ServicePhase::Stopped
            }).map(|record| record.service_id).collect::<BTreeSet<_>>();
            let stale = contents.service_rotations.keys().filter(|id| !owners.contains(id)).copied().collect::<Vec<_>>();
            let mut observations = Vec::new();
            for record in deployments {
                if record.spec.secrets.is_empty()
                    || !matches!(record.phase, ServicePhase::Running | ServicePhase::Degraded)
                    || record.instances.is_empty()
                    || record.update.as_ref().is_some_and(|update| !update.completed)
                {
                    continue;
                }
                let change = match secrets::resolve(contents, &record.spec) {
                    Err(_) => Change::Invalid("Secret rotation requires valid, available Secret references and guest environment values."),
                    Ok(desired) => match contents.service_environments.get(&record.id) {
                        None => Change::Invalid("Secret rotation requires the current encrypted environment; stop and redeploy this service."),
                        Some(current) if *current == desired => Change::Current,
                        Some(_) if record.spec.domain.is_none() => Change::Invalid("Secret rotation requires an Nginx-backed service domain; the previous service is retained."),
                        Some(_) => Change::Changed(fingerprint(&desired)?),
                    },
                };
                observations.push(Observation {
                    rotation: contents.service_rotations.get(&record.service_id).cloned(),
                    deployment: record,
                    change,
                });
            }
            Ok((observations, stale))
        })
    }).await.map_err(|_| "Could not inspect Secret rotation inputs")?;
    if !stale.is_empty() {
        secrets::vault(state, move |vault| {
            vault.write(|contents| {
                for id in stale {
                    contents.service_rotations.remove(&id);
                }
                Ok(())
            })
        })
        .await
        .map_err(|_| "Could not clear retired Secret rotations")?;
    }
    let mut submitted = 0;
    for observation in observations {
        match &observation.change {
            Change::Current => {
                if observation.rotation.is_some() {
                    let id = observation.deployment.service_id;
                    secrets::vault(state, move |vault| {
                        vault.write(|contents| {
                            contents.service_rotations.remove(&id);
                            Ok(())
                        })
                    })
                    .await
                    .map_err(|_| "Could not complete Secret rotation tracking")?;
                }
                if observation
                    .deployment
                    .error
                    .as_deref()
                    .is_some_and(|error| error.starts_with("Secret rotation"))
                {
                    notice(state, &observation.deployment, None).await?;
                }
            }
            Change::Invalid(message) => {
                notice(state, &observation.deployment, Some(message)).await?
            }
            Change::Changed(_) => {
                // Give admitted/manual work priority and do not spend retry budget on its drain.
                if state.service_deploy_permits.available_permits() == 0 {
                    continue;
                }
                match reserve(state, &observation, now).await? {
                    None => {}
                    Some(Action::Pause) => {
                        notice(state, &observation.deployment, Some(PAUSED)).await?
                    }
                    Some(Action::Submit(rotation)) => {
                        match rotate(
                            state.clone(),
                            &observation.deployment,
                            rotation.operation_id,
                        )
                        .await
                        {
                            Ok(_) => submitted += 1,
                            Err(error)
                                if matches!(
                                    error.code(),
                                    "service_busy" | "service_not_running" | "release_changed"
                                ) => {}
                            Err(_) => {
                                notice(state, &observation.deployment, Some(ADMISSION_FAILED))
                                    .await?
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(submitted)
}

fn fingerprint(values: &BTreeMap<String, String>) -> Result<String, SecretError> {
    let bytes = serde_json::to_vec(values).map_err(|_| SecretError::Storage)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

async fn reserve(
    state: &AppState,
    observation: &Observation,
    now: u64,
) -> Result<Option<Action>, String> {
    let Change::Changed(fingerprint) = &observation.change else {
        return Ok(None);
    };
    let previous = &observation.deployment;
    let existing = observation.rotation.as_ref().filter(|rotation| {
        rotation.release_id == previous.id && rotation.fingerprint == *fingerprint
    });
    let mut rotation = existing.cloned().unwrap_or_else(|| ServiceRotation {
        release_id: previous.id,
        operation_id: Uuid::new_v4(),
        fingerprint: fingerprint.clone(),
        attempts: 0,
        retry_after: 0,
        paused: false,
    });
    if let Some(existing) = existing {
        let store = state.store.clone();
        let operation = existing.operation_id;
        let attempt = database(operation, move || store.service_attempt(operation))
            .await
            .map_err(|_| "Could not inspect a Secret rotation attempt")?;
        if attempt.as_ref().is_some_and(|record| {
            record.phase.is_busy()
                || record.phase == ServicePhase::Running
                || record
                    .update
                    .as_ref()
                    .is_some_and(|update| !update.completed)
        }) || rotation.paused
        {
            return Ok(None);
        }
        if rotation.attempts < MAX_ATTEMPTS {
            let completed = attempt
                .as_ref()
                .and_then(|record| record.finished_at)
                .and_then(|time| u64::try_from(time).ok())
                .unwrap_or(0)
                .saturating_mul(1_000);
            let delay = 5_000_u64.saturating_mul(1 << rotation.attempts.saturating_sub(1));
            if now < rotation.retry_after.max(completed.saturating_add(delay)) {
                return Ok(None);
            }
        }
        if attempt.is_some() {
            rotation.operation_id = Uuid::new_v4();
        }
    }
    let pause = rotation.attempts >= MAX_ATTEMPTS;
    if pause {
        rotation.paused = true;
    } else {
        rotation.attempts += 1;
        let delay = 5_000_u64.saturating_mul(1 << (rotation.attempts - 1));
        rotation.retry_after = now.saturating_add(delay.min(300_000));
    }
    let expected = observation.rotation.clone();
    let id = previous.service_id;
    let spec = previous.spec.clone();
    let next = rotation.clone();
    let recorded = secrets::vault(state, move |vault| {
        vault.write(|contents| {
            // A concurrent edit or another reconciler cannot overwrite a newer reservation.
            if contents.service_rotations.get(&id) != expected.as_ref()
                || fingerprint_of_spec(contents, &spec)? != next.fingerprint
            {
                return Ok(false);
            }
            contents.service_rotations.insert(id, next);
            Ok(true)
        })
    })
    .await
    .map_err(|_| "Could not reserve Secret rotation")?;
    Ok(recorded.then_some(if pause {
        Action::Pause
    } else {
        Action::Submit(rotation)
    }))
}

fn fingerprint_of_spec(
    contents: &crate::secrets::Contents,
    spec: &MicroServiceSpec,
) -> Result<String, SecretError> {
    fingerprint(&secrets::resolve(contents, spec)?)
}

/// Publish only static guidance through existing deployment status, fenced against manual work.
async fn notice(
    state: &AppState,
    observed: &MicroServiceDeployment,
    message: Option<&'static str>,
) -> Result<(), String> {
    let _claim = state.service_mutations.lock().await;
    let id = observed.service_id;
    let store = state.store.clone();
    let Some(mut current) = database(observed.id, move || store.service_deployment(id))
        .await
        .map_err(|_| "Could not read Secret rotation status")?
    else {
        return Ok(());
    };
    if current.id != observed.id
        || !matches!(
            current.phase,
            ServicePhase::Running | ServicePhase::Degraded
        )
        || current
            .update
            .as_ref()
            .is_some_and(|update| !update.completed)
        || current.error.as_deref() == message
    {
        return Ok(());
    }
    if message.is_none()
        && !current
            .error
            .as_deref()
            .is_some_and(|error| error.starts_with("Secret rotation"))
    {
        return Ok(());
    }
    current.error = message.map(str::to_owned);
    if current.log.len() >= 128 {
        current.log.remove(0);
    }
    current.log.push(
        message
            .unwrap_or("Secret rotation matches the running release")
            .into(),
    );
    save(state, &current).await
}

#[cfg(test)]
mod tests;

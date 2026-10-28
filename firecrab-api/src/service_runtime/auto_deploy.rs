//! Host-local registry monitoring submits pinned images through the normal Deploy queue.

use super::*;
use crate::persistence::auto_deploy::{Pending, Watch};
use firecrab_api_types::{AutoDeployPause, AutoDeployPolicy, AutoDeployStatus};

mod controls;
mod rules;
pub(crate) use controls::validate_auth;
pub use controls::{configure, get, pause, resume};

pub(crate) fn start(state: AppState) {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(5));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            if reconcile(&state).await.is_err() {
                tracing::warn!("Could not reconcile automatic Deploy; check the service database");
            }
        }
    });
}

fn ready(record: &MicroServiceDeployment) -> bool {
    matches!(record.phase, ServicePhase::Running | ServicePhase::Degraded)
        && !record.instances.is_empty()
        && record.domain_route_owned
        && record
            .recovery
            .as_ref()
            .is_none_or(|health| health.repair.is_none())
        && record.update.as_ref().is_none_or(|u| u.completed)
}

async fn reconcile(state: &AppState) -> Result<(), AppError> {
    let store = state.store.clone();
    let records = database(Uuid::new_v4(), move || {
        let mut records = Vec::new();
        for service in store.list_micro_services()? {
            if service.spec.auto_deploy.is_none()
                || store.service_is_controller_owned(service.id)?
            {
                continue;
            }
            if let Some(record) = store.service_deployment(service.id)? {
                records.push(record);
            }
        }
        Ok(records)
    })
    .await?;
    // Sequential reads bound registry traffic and keep one metadata request active per host.
    for record in records {
        check(state, record).await?;
    }
    Ok(())
}

async fn check(state: &AppState, record: MicroServiceDeployment) -> Result<(), AppError> {
    let id = record.service_id;
    let request = Uuid::new_v4();
    let guard = state.service_mutations.lock().await;
    if !ready(&record) {
        return Ok(());
    }
    let Some(policy) = record.spec.auto_deploy.clone() else {
        return Ok(());
    };
    let store = state.store.clone();
    let Some(mut watch) = database(request, move || store.tag_watch(id)).await? else {
        return Ok(());
    };
    if watch.status.policy.as_ref() != Some(&policy)
        || watch.status.pause == Some(AutoDeployPause::Manual)
    {
        return Ok(());
    }
    if watch.pending.is_some() {
        drop(guard);
        return dispatch(state, &record, watch).await;
    }
    let now = chrono::Utc::now().timestamp();
    if watch.status.next_check_at > now {
        return Ok(());
    }
    watch.status.next_check_at = now + i64::from(policy.interval_seconds);
    persist(state, id, watch.clone()).await?;
    drop(guard);
    let observation = rules::discover(state, &policy).await;
    finish_observation(state, &record, &watch, observation, now).await
}

async fn finish_observation(
    state: &AppState,
    record: &MicroServiceDeployment,
    watch: &Watch,
    observation: Result<(String, String), &'static str>,
    now: i64,
) -> Result<(), AppError> {
    let id = record.service_id;
    let request = record.id;
    let guard = state.service_mutations.lock().await;
    let store = state.store.clone();
    let current = database(request, move || {
        Ok((store.service_deployment(id)?, store.tag_watch(id)?))
    })
    .await?;
    let (Some(current), Some(mut saved)) = current else {
        return Ok(());
    };
    if saved.generation != watch.generation
        || current.id != record.id
        || current.spec != record.spec
        || !ready(&current)
    {
        return Ok(());
    }
    observe(&mut saved, &current, observation, now);
    persist(state, id, saved.clone()).await?;
    drop(guard);
    if saved.pending.is_some() {
        dispatch(state, &current, saved).await?;
    }
    Ok(())
}

fn observe(
    watch: &mut Watch,
    current: &MicroServiceDeployment,
    observation: Result<(String, String), &'static str>,
    now: i64,
) {
    if watch.status.pause == Some(AutoDeployPause::Manual) {
        return;
    }
    match observation {
        Err(message) => {
            watch.status.failures = watch.status.failures.saturating_add(1).min(6);
            watch.status.next_check_at = now
                + (i64::from(
                    watch
                        .status
                        .policy
                        .as_ref()
                        .expect("enabled policy")
                        .interval_seconds,
                ) * (1_i64 << watch.status.failures))
                    .min(3600);
            watch.status.error = Some(message.into());
        }
        Ok((tag, digest)) => {
            watch.status.failures = 0;
            watch.status.error = None;
            watch.status.observed_tag = Some(tag);
            watch.status.observed_digest = Some(digest.clone());
            if watch.status.pause.is_some() && watch.blocked.as_ref() != Some(&digest) {
                watch.status.pause = None;
                watch.blocked = None;
            }
            if watch.status.pause.is_none() && watch.handled.as_ref() != Some(&digest) {
                if current.digest.as_ref() == Some(&digest) {
                    watch.handled = Some(digest);
                } else {
                    let pending = Pending {
                        operation: Uuid::new_v4(),
                        previous: current.id,
                        digest,
                    };
                    watch.status.operation_id = Some(pending.operation);
                    watch.pending = Some(pending);
                }
            }
        }
    }
}

async fn dispatch(
    state: &AppState,
    record: &MicroServiceDeployment,
    watch: Watch,
) -> Result<(), AppError> {
    let Some(pending) = watch.pending.as_ref() else {
        return Ok(());
    };
    if deploy::automatic(state.clone(), record, &watch)
        .await
        .is_ok()
    {
        return Ok(());
    }
    let _guard = state.service_mutations.lock().await;
    let id = record.service_id;
    let operation = pending.operation;
    let store = state.store.clone();
    let (saved, accepted, current, allowed) = database(operation, move || {
        Ok((
            store.tag_watch(id)?,
            store.service_attempt(operation)?,
            store.service_deployment(id)?,
            store.stack_service_auto_allowed(id)?,
        ))
    })
    .await?;
    let Some(mut saved) = saved else {
        return Ok(());
    };
    // An acknowledgement can be lost after durable queue admission. Keep its exact identity.
    if saved.generation != watch.generation || accepted.is_some() {
        return Ok(());
    }
    saved.pending = None;
    if !allowed
        || current.as_ref().is_none_or(|current| {
            current.id != pending.previous || current.spec != record.spec || !ready(current)
        })
    {
        saved.status.operation_id = None;
        return persist(state, id, saved).await;
    }
    saved.handled = Some(pending.digest.clone());
    saved.pause(AutoDeployPause::Failed, Some(pending.digest.clone()));
    saved.status.error = Some(
        "Automatic Deploy could not enter the queue; check Auth, Nginx and service ownership"
            .into(),
    );
    persist(state, id, saved).await
}

async fn persist(state: &AppState, id: Uuid, watch: Watch) -> Result<(), AppError> {
    let store = state.store.clone();
    database(id, move || store.save_tag_watch(id, &watch)).await
}

/// The private claim cannot be supplied by an HTTP request or another Stack operation.
pub(super) async fn authorize(
    state: &AppState,
    previous: &MicroServiceDeployment,
    spec: &MicroServiceSpec,
    claim: &Watch,
    operation: Uuid,
) -> Result<(), AppError> {
    let id = previous.service_id;
    let store = state.store.clone();
    let (saved, allowed, owned) = database(operation, move || {
        Ok((
            store.tag_watch(id)?,
            store.stack_service_auto_allowed(id)?,
            store.service_is_controller_owned(id)?,
        ))
    })
    .await?;
    let mut expected = previous.spec.clone();
    expected.image = spec.image.clone();
    let valid = saved.as_ref().is_some_and(|s| {
        s.generation == claim.generation
            && s.status.pause.is_none()
            && s.pending.as_ref().is_some_and(|p| {
                p.operation == operation
                    && p.previous == previous.id
                    && claim.pending.as_ref().is_some_and(|c| c.digest == p.digest)
            })
    });
    let pinned = claim
        .status
        .policy
        .as_ref()
        .and_then(|p| p.source())
        .zip(claim.pending.as_ref())
        .is_some_and(|((repository, _), pending)| {
            spec.image == format!("{repository}@{}", pending.digest)
        });
    if !valid
        || !ready(previous)
        || !pinned
        || !allowed
        || owned
        || expected != *spec
        || claim.status.policy != previous.spec.auto_deploy
    {
        return Err(AppError::conflict(
            "auto_deploy_changed",
            "Automatic Deploy inputs or ownership changed",
            operation,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;

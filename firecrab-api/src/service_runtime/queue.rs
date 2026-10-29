//! SQLite determines priority/FIFO order; the existing permit bounds live surge.

use super::*;
use crate::persistence::deploy_queue::{self, Claim};
use tokio::sync::OwnedSemaphorePermit;

/// Register, wait, then claim the current durable operation before any side effects.
pub(super) async fn admit(
    state: &AppState,
    record: &MicroServiceDeployment,
) -> Option<(OwnedSemaphorePermit, MicroServiceDeployment)> {
    let store = state.store.clone();
    let saved = record.clone();
    if database(record.id, move || {
        store.enqueue_service_deployment(&saved, false)
    })
    .await
    .is_err()
    {
        tracing::error!(service_id = %record.service_id, "Could not enqueue Deploy");
        return None;
    }
    state.service_deploy_changed.notify_waiters();
    let id = deploy_queue::operation(record);
    loop {
        let changed = state.service_deploy_changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let store = state.store.clone();
        let Ok((waiting, first)) = database(id, move || store.service_deploy_turn(id)).await else {
            return None;
        };
        if !waiting {
            return None;
        }
        if !first {
            changed.await;
            continue;
        }
        let permit = state
            .service_deploy_permits
            .clone()
            .acquire_owned()
            .await
            .ok()?;
        let claim = state.service_mutations.lock().await;
        let store = state.store.clone();
        let service = record.service_id;
        let release = record.id;
        let result = database(id, move || {
            store.claim_service_deployment(service, release, id)
        })
        .await;
        drop(claim);
        state.service_deploy_changed.notify_waiters();
        match result {
            Ok(Claim::Admitted(record)) => return Some((permit, *record)),
            Ok(Claim::Waiting) => drop(permit),
            _ => return None,
        }
    }
}

/// Cancellation records the exact attempt and never withdraws a serving route.
pub async fn cancel(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path((service, operation)): Path<(String, String)>,
) -> Result<Json<MicroServiceDeployment>, AppError> {
    let service = vms::parse_id(&service, request.0)?;
    let operation = vms::parse_id(&operation, request.0)?;
    tokio::spawn(cancel_owned(state, request.0, service, operation))
        .await
        .map_err(|_| AppError::internal(request.0))?
}

async fn cancel_owned(
    state: AppState,
    request: Uuid,
    service: Uuid,
    operation: Uuid,
) -> Result<Json<MicroServiceDeployment>, AppError> {
    let _claim = state.service_mutations.lock().await;
    crate::handlers::micro_services::ensure_stack_writer(&state, service, None, request).await?;
    let store = state.store.clone();
    if crate::cluster::stored(move || store.cluster_service_worker(service))
        .await
        .map_err(|_| AppError::internal(request))?
        .is_some()
    {
        return Err(AppError::conflict(
            "controller_managed",
            "Cancel this service through its controller",
            request,
        ));
    }
    let store = state.store.clone();
    let mut attempt = database(request, move || store.service_attempt(operation))
        .await?
        .filter(|attempt| attempt.service_id == service)
        .ok_or_else(|| AppError::not_found(request))?;
    if attempt.phase == ServicePhase::Cancelled {
        secrets::remove(&state, &attempt).await.map_err(|_| {
            AppError::unavailable(
                "Could not clear cancelled deployment Secrets; retry cancellation",
                request,
            )
        })?;
        return Ok(Json(attempt));
    }
    let store = state.store.clone();
    let current = database(request, move || store.service_deployment(service))
        .await?
        .ok_or_else(|| AppError::not_found(request))?;
    if current.id != operation || !deploy_queue::cancellable(&current) {
        return Err(AppError::conflict(
            "deploy_not_cancellable",
            "Only a waiting Deploy operation without resource changes can be cancelled",
            request,
        ));
    }
    attempt = current;
    attempt.phase = ServicePhase::Cancelled;
    attempt.finished_at = Some(chrono::Utc::now().timestamp());
    attempt.error = None;
    if attempt.log.len() >= 128 {
        attempt.log.remove(0);
    }
    attempt
        .log
        .push("Deploy cancelled before queue admission".into());
    if let Some(update) = &mut attempt.update {
        update.completed = true;
    }
    let saved = attempt.clone();
    let store = state.store.clone();
    database(request, move || {
        if let Some(update) = &saved.update {
            store.restore_service_release(&saved, &update.previous)
        } else {
            store.save_service_deployment(&saved)
        }
    })
    .await?;
    state.service_deploy_changed.notify_waiters();
    secrets::remove(&state, &attempt).await.map_err(|_| {
        AppError::unavailable(
            "Could not clear cancelled deployment Secrets; retry cancellation",
            request,
        )
    })?;
    Ok(Json(attempt))
}

#[cfg(test)]
mod tests;

//! Enrolled worker adapter. The existing MicroService APIs own import, boot and cleanup.
use super::super::worker::{Peer, controller};
use super::*;
use crate::{error::AppError, extract::ValidatedJson, server::RequestId};
use axum::{
    Extension, Json,
    extract::{Path, State},
};

pub(crate) async fn get(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    peer: Peer,
    Path(id): Path<Uuid>,
) -> Result<Json<ServiceWorkerReceipt>, AppError> {
    let owner = controller(&state, peer, request)?;
    let _lock = state.cluster.lock_worker(id).await;
    let store = state.store.clone();
    let mut receipt = stored(move || store.cluster_service_worker(id))
        .await
        .map_err(|e| super::super::handlers::error(e, request))?
        .filter(|r| r.controller_id == owner)
        .ok_or_else(|| AppError::not_found(request.0))?;
    observe(&state, &mut receipt)
        .await
        .map_err(|_| AppError::internal(request.0))?;
    Ok(Json(receipt))
}

pub(crate) async fn put(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    peer: Peer,
    Path(id): Path<Uuid>,
    ValidatedJson(input): ValidatedJson<credentials::WorkerRequest>,
) -> Result<Json<ServiceWorkerReceipt>, AppError> {
    let owner = controller(&state, peer, request)?;
    let host = state
        .cluster
        .identity
        .ok_or_else(|| AppError::internal(request.0))?;
    let intent = &input.intent;
    let mut local = intent.spec.clone();
    let scoped = intent
        .credentials
        .iter()
        .map(|(key, r)| (key.clone(), r.secret_id))
        .collect();
    if local.secrets != scoped {
        return Err(AppError::conflict(
            "invalid_credential_scope",
            "worker credential references do not match the scoped environment",
            request.0,
        ));
    }
    local.secrets.clear();
    let mut spec = ClusterServiceSpec {
        service: local,
        state: intent.state.clone(),
        credentials: intent.credentials.clone(),
        mode: ClusterServiceMode::Replicated,
        targets: vec![ClusterServiceTarget {
            host_id: host,
            replicas: intent.spec.replicas,
            request: intent.request.clone(),
        }],
    };
    validate_spec(&mut spec).map_err(|e| super::super::handlers::error(e, request))?;
    if intent.desired == ClusterServiceDesired::Running {
        credentials::validate_values(&input)
            .map_err(|e| super::super::handlers::error(e, request))?;
    } else if !input.credential_values.is_empty() {
        return Err(AppError::conflict(
            "invalid_credential_scope",
            "Stop must not include credential values",
            request.0,
        ));
    }
    if id.is_nil()
        || intent.deployment_id.is_nil()
        || intent.generation == 0
        || intent.generation > i64::MAX as u64
        || intent.release_generation == 0
        || intent.release_generation > intent.generation
    {
        return Err(AppError::conflict(
            "invalid_service_intent",
            "invalid service identity or generation",
            request.0,
        ));
    }
    let permit = state
        .cluster
        .worker_permits
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(16)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::too_many_requests(request.0))?;
    // Detached execution survives the HTTP request's timeout or cancellation.
    tokio::spawn(async move {
        let _permit = permit;
        let _lock = state.cluster.lock_worker(id).await;
        let store = state.store.clone();
        let intent = input.intent.clone();
        let mut receipt = stored(move || store.cluster_service_worker_admit(id, owner, intent))
            .await
            .map_err(|e| super::super::handlers::error(e, request))?;
        if receipt.intent.desired == ClusterServiceDesired::Running {
            credentials::install(&state, &input).await.map_err(|_| {
                AppError::unavailable(
                    "Pinned service credentials are unavailable; ownership is retained.",
                    request.0,
                )
            })?;
        }
        receipt.error = apply(&state, &receipt, request).await.err().map(|error| {
            format!(
                "host-local service operation failed ({}); ownership is retained",
                error.code()
            )
        });
        observe(&state, &mut receipt)
            .await
            .map_err(|_| AppError::internal(request.0))?;
        let store = state.store.clone();
        let record = receipt.clone();
        stored(move || store.cluster_service_worker_finish(&record))
            .await
            .map_err(|e| super::super::handlers::error(e, request))?;
        Ok(Json(receipt))
    })
    .await
    .map_err(|_| AppError::internal(request.0))?
}

async fn observe(state: &AppState, receipt: &mut ServiceWorkerReceipt) -> Result<(), ClusterError> {
    let store = state.store.clone();
    let id = receipt.id;
    let deployment = stored(move || Ok(store.service_deployment(id)?)).await?;
    if let Some(d) = &deployment {
        if d.service_id != id || !receipt.deployment_ids.contains(&d.id) {
            return Err(ClusterError::Conflict(
                "foreign deployment ownership".into(),
            ));
        }
        let Json(live) = crate::service_runtime::get(
            State(state.clone()),
            Extension(RequestId(Uuid::new_v4())),
            Path(id.to_string()),
        )
        .await
        .map_err(|_| ClusterError::Task)?;
        receipt.deployment = live;
    } else {
        receipt.deployment = None;
    }
    if receipt.intent.desired == ClusterServiceDesired::Stopped
        && receipt.deployment.as_ref().is_none_or(|d| {
            d.phase == ServicePhase::Stopped
                && d.instances.is_empty()
                && !d.domain_route_owned
                && !d.internal_route_owned
        })
    {
        credentials::remove_stopped(state, receipt).await?;
    }
    if receipt
        .deployment
        .as_ref()
        .is_some_and(|d| matches!(d.phase, ServicePhase::Failed | ServicePhase::Degraded))
    {
        receipt.error = Some("host-local deployment is unhealthy or needs cleanup".into());
    }
    Ok(())
}

async fn apply(
    state: &AppState,
    receipt: &ServiceWorkerReceipt,
    request: RequestId,
) -> Result<(), AppError> {
    let store = state.store.clone();
    let id = receipt.id;
    let (service, deployment) = crate::handlers::micro_services::database(request.0, move || {
        Ok((store.micro_service(id)?, store.service_deployment(id)?))
    })
    .await?;
    if let Some(deployment) = &deployment {
        if deployment.service_id != id || !receipt.deployment_ids.contains(&deployment.id) {
            return Err(AppError::conflict(
                "foreign_deployment",
                "service deployment belongs to another owner",
                request.0,
            ));
        }
        if deployment.phase.is_busy() {
            return Ok(());
        }
        let desired_running = receipt.intent.desired == ClusterServiceDesired::Running;
        if desired_running
            && deployment.id == receipt.intent.deployment_id
            && matches!(
                deployment.phase,
                ServicePhase::Running | ServicePhase::Degraded
            )
        {
            return Ok(());
        }
        if deployment.phase != ServicePhase::Stopped
            || !deployment.instances.is_empty()
            || deployment.domain_route_owned
            || deployment.internal_route_owned
        {
            let _ =
                crate::service_runtime::stop_from_stack(state.clone(), request.0, id, None).await?;
            return Ok(());
        }
    }
    if receipt.intent.desired == ClusterServiceDesired::Stopped {
        return Ok(());
    }
    if let Some(service) = service {
        if service.spec != receipt.intent.spec {
            let _ = crate::handlers::micro_services::update_from_stack(
                state.clone(),
                request.0,
                id,
                receipt.intent.spec.clone(),
                None,
            )
            .await?;
        }
    } else {
        let _ = crate::handlers::micro_services::create_with_id(
            state.clone(),
            request,
            receipt.intent.spec.clone(),
            id,
        )
        .await?;
    }
    let _ = crate::service_runtime::deploy_reserved(
        state.clone(),
        request.0,
        id,
        receipt.intent.request.clone(),
        receipt.intent.deployment_id,
    )
    .await?;
    Ok(())
}

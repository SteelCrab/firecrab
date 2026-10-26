//! Deploy reuses the MicroVM pipeline and the existing Nginx configuration transaction.

use super::*;
use firecrab_api_types::{
    DeployStrategy, RetryServiceRequest, RollbackServiceRequest, ServiceRouteTransition,
    ServiceUpdate, UpdateServiceRequest,
};
use firecrab_helper_protocol::pipe::{
    IngressWorker, Operation, ServiceRouteOwner, ServiceRouteRequest, Snapshot,
};
use std::net::SocketAddr;

pub(crate) mod cache;
pub(super) mod replicas;
pub(crate) mod rotation;
pub(super) mod routes;
use replicas::*;
use routes::*;

/// Exact operation status remains available after recovery restores an older active release.
pub async fn operation(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path((service, operation)): Path<(String, String)>,
) -> Result<Json<MicroServiceDeployment>, AppError> {
    let service = vms::parse_id(&service, request.0)?;
    let operation = vms::parse_id(&operation, request.0)?;
    let store = state.store.clone();
    let record = database(request.0, move || store.service_attempt(operation)).await?;
    record
        .filter(|record| record.service_id == service)
        .map(Json)
        .ok_or_else(|| AppError::not_found(request.0))
}

/// List only successful releases owned by this service.
pub async fn releases(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<Vec<MicroServiceDeployment>>, AppError> {
    let id = vms::parse_id(&id, request.0)?;
    let store = state.store.clone();
    if database(request.0, move || store.micro_service(id))
        .await?
        .is_none()
    {
        return Err(AppError::not_found(request.0));
    }
    let store = state.store.clone();
    database(request.0, move || store.service_releases(id))
        .await
        .map(Json)
}

/// Accept one idempotent update without mutating the configuration that is serving.
pub async fn update(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(body): ValidatedJson<UpdateServiceRequest>,
) -> Result<(StatusCode, Json<MicroServiceDeployment>), AppError> {
    update_from_stack(state, request, vms::parse_id(&id, request.0)?, body, None).await
}

pub(crate) async fn update_from_stack(
    state: AppState,
    request: RequestId,
    id: Uuid,
    body: UpdateServiceRequest,
    operation: Option<Uuid>,
) -> Result<(StatusCode, Json<MicroServiceDeployment>), AppError> {
    accept(
        state,
        request,
        id,
        body,
        None,
        None,
        Writer::Stack(operation),
    )
    .await
}

/// Rollback chooses immutable inputs from a successful release of the same service.
pub async fn rollback(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(body): ValidatedJson<RollbackServiceRequest>,
) -> Result<(StatusCode, Json<MicroServiceDeployment>), AppError> {
    let id = vms::parse_id(&id, request.0)?;
    crate::handlers::micro_services::ensure_stack_writer(&state, id, None, request.0).await?;
    let operation_id = body.operation_id;
    let store = state.store.clone();
    if let Some(saved) = database(request.0, move || store.service_attempt(operation_id)).await? {
        if saved.service_id == id
            && saved.update.as_ref().is_some_and(|update| {
                update.rollback_of == Some(body.release_id)
                    && update.strategy == body.strategy
                    && body
                        .expected_release_id
                        .is_none_or(|id| id == update.previous.id)
            })
        {
            return Ok((StatusCode::ACCEPTED, Json(saved)));
        }
        return Err(AppError::conflict(
            "operation_conflict",
            "operationId already belongs to a different Deploy request",
            request.0,
        ));
    }
    let store = state.store.clone();
    let release_id = body.release_id;
    let releases = database(request.0, move || store.service_releases(id)).await?;
    let release = releases
        .into_iter()
        .find(|release| release.id == release_id)
        .ok_or_else(|| AppError::not_found(request.0))?;
    let mut spec = release.spec.clone();
    spec.image = pinned_reference(&release).map_err(|_| {
        AppError::unavailable(
            "The selected release has no immutable image identity.",
            request.0,
        )
    })?;
    accept(
        state,
        request,
        id,
        UpdateServiceRequest {
            expected_release_id: body.expected_release_id,
            operation_id: body.operation_id,
            spec,
            strategy: body.strategy,
        },
        Some(release_id),
        None,
        Writer::Stack(None),
    )
    .await
}

/// Explicit retry resumes the existing journal and keeps its reserved VM identities.
pub async fn retry(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(body): ValidatedJson<RetryServiceRequest>,
) -> Result<(StatusCode, Json<MicroServiceDeployment>), AppError> {
    let id = vms::parse_id(&id, request.0)?;
    let claim = state.service_mutations.clone().lock_owned().await;
    crate::handlers::micro_services::ensure_stack_writer(&state, id, None, request.0).await?;
    let store = state.store.clone();
    let mut deployment = database(request.0, move || store.service_deployment(id))
        .await?
        .ok_or_else(|| AppError::not_found(request.0))?;
    if deployment.id != body.operation_id
        || deployment
            .update
            .as_ref()
            .is_none_or(|update| update.completed)
    {
        return Err(AppError::conflict(
            "deploy_not_retryable",
            "Deploy has no interrupted work to retry",
            request.0,
        ));
    }
    if deployment.phase.is_busy() {
        return Ok((StatusCode::ACCEPTED, Json(deployment)));
    }
    deployment.phase = ServicePhase::Queued;
    let deployment = submit(state.clone(), deployment, claim, request.0).await?;
    Ok((StatusCode::ACCEPTED, Json(deployment)))
}

enum Writer {
    Stack(Option<Uuid>),
    Tag(Box<crate::persistence::auto_deploy::Watch>),
}

async fn accept(
    state: AppState,
    request: RequestId,
    id: Uuid,
    mut body: UpdateServiceRequest,
    rollback_of: Option<Uuid>,
    pinned_digest: Option<String>,
    writer: Writer,
) -> Result<(StatusCode, Json<MicroServiceDeployment>), AppError> {
    crate::handlers::micro_services::validate(&body.spec, request.0)?;
    if body.operation_id.is_nil() {
        return Err(AppError::validation(
            BTreeMap::from([("operationId".into(), "must be a non-nil UUID".into())]),
            request.0,
        ));
    }
    let claim = state.service_mutations.clone().lock_owned().await;
    if let Writer::Stack(operation) = &writer {
        crate::handlers::micro_services::ensure_stack_writer(&state, id, *operation, request.0)
            .await?;
    }
    let store = state.store.clone();
    let operation_id = body.operation_id;
    if let Some(saved) = database(request.0, move || store.service_attempt(operation_id)).await? {
        if saved.service_id == id
            && saved.update.as_ref().is_some_and(|update| {
                update.requested_spec == body.spec
                    && update.strategy == body.strategy
                    && update.rollback_of == rollback_of
                    && body
                        .expected_release_id
                        .is_none_or(|id| id == update.previous.id)
            })
        {
            return Ok((StatusCode::ACCEPTED, Json(saved)));
        }
        return Err(AppError::conflict(
            "operation_conflict",
            "operationId already belongs to a different Deploy request",
            request.0,
        ));
    }
    let store = state.store.clone();
    let mut previous = database(request.0, move || store.service_deployment(id))
        .await?
        .ok_or_else(|| AppError::not_found(request.0))?;
    if rollback_of.is_some() {
        body.spec.auto_deploy = previous.spec.auto_deploy.clone();
    }
    if let Writer::Tag(watch) = &writer {
        super::auto_deploy::authorize(&state, &previous, &body.spec, watch, body.operation_id)
            .await?;
    }
    if let Some(policy) = &body.spec.auto_deploy {
        super::auto_deploy::validate_auth(&state, policy, request.0).await?;
    }
    if body
        .expected_release_id
        .is_some_and(|expected| expected != previous.id)
    {
        return Err(AppError::conflict(
            "release_changed",
            "The active release changed; reload the service before Deploy",
            request.0,
        ));
    }
    if previous.phase.is_busy()
        || previous
            .update
            .as_ref()
            .is_some_and(|update| !update.completed)
    {
        return Err(AppError::conflict(
            "service_busy",
            "wait for Deploy or retry its recorded recovery",
            request.0,
        ));
    }
    if !matches!(
        previous.phase,
        ServicePhase::Running | ServicePhase::Degraded
    ) || previous.instances.is_empty()
    {
        return Err(AppError::conflict(
            "service_not_running",
            "start the service before requesting an update",
            request.0,
        ));
    }
    if body.spec.name != previous.spec.name
        || body.spec.domain != previous.spec.domain
        || body.spec.domain.is_none()
        || !previous.domain_route_owned
    {
        return Err(AppError::validation(BTreeMap::from([("domain".into(), "Deploy requires the same Nginx-backed domain and service name; direct replica port forwards cannot switch traffic without interruption".into())]), request.0));
    }
    if previous.internal_route_owned
        && body.spec.security_group_id != previous.spec.security_group_id
    {
        return Err(AppError::validation(
            BTreeMap::from([(
                "securityGroupId".into(),
                "Stop the service before changing its internal routing security group".into(),
            )]),
            request.0,
        ));
    }
    validate_database_bindings(&state, id, request.0)?;
    require_image_auth(&state, &body.spec.image, request.0).await?;
    secrets::preflight(&state, &body.spec, request.0).await?;
    crate::handlers::pipe_storage::validate_binding(&state, &body.spec, request.0).await?;
    crate::queues::validate_bindings(&state, &body.spec, request.0).await?;
    let mut placement = previous.request.clone();
    crate::queues::placement(&state, &body.spec, &mut placement, request.0)?;
    if placement.micro_network_id != previous.request.micro_network_id {
        return Err(AppError::validation(
            BTreeMap::from([(
                "queues".into(),
                "Deploy must keep the existing MicroNetwork".into(),
            )]),
            request.0,
        ));
    }
    validate_placement(&state, &body.spec, &previous.request, request.0).await?;
    domain::preflight(&state, id, &body.spec, request.0).await?;
    let snapshot = inspect(&state)
        .await
        .map_err(|_| AppError::unavailable("Cannot inspect Nginx before Deploy.", request.0))?;
    if !snapshot.ingress_graceful_drain {
        return Err(AppError::unavailable(
            "Apply the current Pipe configuration before Deploy so Nginx does not force old workers to exit.",
            request.0,
        ));
    }
    let store = state.store.clone();
    let waiting = database(request.0, move || store.service_deploy_waiting()).await?;
    let host_busy = state.service_deploy_permits.available_permits() == 0 || waiting;
    if !host_busy
        && snapshot
            .ingress_workers
            .as_ref()
            .is_some_and(|workers| workers.iter().any(|worker| worker.draining))
    {
        return Err(AppError::unavailable(
            "An earlier Nginx reload is still draining. Wait for its existing connections before starting Deploy.",
            request.0,
        ));
    }
    if snapshot.ingress_workers.as_ref().is_none_or(Vec::is_empty) {
        return Err(AppError::unavailable(
            "Update the host bundle and start Nginx before Deploy; worker drain verification is unavailable.",
            request.0,
        ));
    }
    // Seed pre-upgrade releases before replacing their mutable latest snapshot.
    save(&state, &previous)
        .await
        .map_err(|_| AppError::internal(request.0))?;
    previous.update = None;
    let message = if matches!(writer, Writer::Tag(_)) {
        "Automatic Deploy accepted; verified registry digest is pinned"
    } else if pinned_digest.is_some() {
        "Secret rotation accepted; the current image digest is retained"
    } else {
        "Deploy accepted; previous release continues serving"
    };
    let deployment = MicroServiceDeployment {
        recovery: None,
        started_at: chrono::Utc::now().timestamp(),
        finished_at: None,
        update: Some(Box::new(ServiceUpdate {
            failure: None,
            cancelled: false,
            previous: Box::new(previous.clone()),
            requested_spec: body.spec.clone(),
            strategy: body.strategy,
            rollback_of,
            restoring: false,
            routed_new: false,
            transition: None,
            retiring: vec![],
            completed: false,
        })),
        id: body.operation_id,
        service_id: id,
        phase: ServicePhase::Queued,
        spec: body.spec,
        request: previous.request,
        digest: pinned_digest,
        instances: vec![],
        log: vec![message.into()],
        domain_route_owned: false,
        internal_route_owned: false,
        error: None,
    };
    let deployment = submit(state.clone(), deployment, claim, request.0).await?;
    Ok((StatusCode::ACCEPTED, Json(deployment)))
}

pub(super) async fn automatic(
    state: AppState,
    previous: &MicroServiceDeployment,
    watch: &crate::persistence::auto_deploy::Watch,
) -> Result<MicroServiceDeployment, AppError> {
    let pending = watch
        .pending
        .as_ref()
        .ok_or_else(|| AppError::internal(previous.id))?;
    let policy = watch
        .status
        .policy
        .as_ref()
        .ok_or_else(|| AppError::internal(previous.id))?;
    let (repository, _) = policy
        .source()
        .ok_or_else(|| AppError::internal(previous.id))?;
    let mut spec = previous.spec.clone();
    spec.image = format!("{repository}@{}", pending.digest);
    accept(
        state,
        RequestId(pending.operation),
        previous.service_id,
        UpdateServiceRequest {
            expected_release_id: Some(pending.previous),
            operation_id: pending.operation,
            spec,
            strategy: DeployStrategy::Rolling,
        },
        None,
        None,
        Writer::Tag(Box::new(watch.clone())),
    )
    .await
    .map(|(_, Json(record))| record)
}

/// Rotate private inputs without resolving a mutable image tag or changing service configuration.
async fn rotate(
    state: AppState,
    previous: &MicroServiceDeployment,
    operation_id: Uuid,
) -> Result<MicroServiceDeployment, AppError> {
    pinned_reference(previous).map_err(|_| {
        AppError::unavailable(
            "Secret rotation requires a pinned running release.",
            operation_id,
        )
    })?;
    accept(
        state,
        RequestId(operation_id),
        previous.service_id,
        UpdateServiceRequest {
            expected_release_id: Some(previous.id),
            operation_id,
            spec: previous.spec.clone(),
            strategy: DeployStrategy::Rolling,
        },
        None,
        previous.digest.clone(),
        Writer::Stack(None),
    )
    .await
    .map(|(_, Json(record))| record)
}

pub(super) fn resume_worker(state: AppState, deployment: MicroServiceDeployment) {
    let fresh = crate::persistence::deploy_queue::cancellable(&deployment);
    let cancelled = deployment
        .update
        .as_ref()
        .is_some_and(|update| update.cancelled);
    tokio::spawn(async move {
        let (_permit, deployment) = if cancelled {
            (None, deployment)
        } else {
            let Some((permit, current)) = queue::admit(&state, &deployment).await else {
                return;
            };
            (Some(permit), current)
        };
        let service = deployment.service_id;
        let store = state.store.clone();
        let Ok(Some(current)) =
            database(deployment.id, move || store.service_deployment(service)).await
        else {
            return;
        };
        if current.id != deployment.id
            || !current.phase.is_busy()
            || current
                .update
                .as_ref()
                .is_none_or(|update| update.completed)
        {
            return;
        }
        let deployment = current;
        if deployment
            .update
            .as_ref()
            .is_some_and(|update| update.cancelled)
        {
            let mut deployment = deployment;
            if let Err(error) = finish_stop(&state, &mut deployment).await {
                retain_failure(&state, &mut deployment, error).await;
            }
            return;
        }
        let mut deployment = deployment;
        if fresh && let Err(error) = fresh_ingress(&state, &deployment).await {
            retain_failure(&state, &mut deployment, error).await;
            return;
        }
        if deployment
            .update
            .as_ref()
            .is_some_and(|update| update.restoring)
        {
            if let Err(error) = restore(&state, &mut deployment).await {
                retain_failure(&state, &mut deployment, error).await;
            }
            return;
        }
        if let Err(error) = execute_update(&state, &mut deployment).await {
            // A drain failure cannot justify deleting VMs still serving existing clients.
            if deployment.phase == ServicePhase::Draining
                && deployment
                    .update
                    .as_ref()
                    .and_then(|update| update.transition.as_ref())
                    .is_some_and(|transition| transition.confirmed)
            {
                retain_failure(&state, &mut deployment, error).await;
                return;
            }
            deployment.error = Some(error.clone());
            let update = deployment.update.as_mut().expect("update journal");
            update.failure = Some(error);
            update.restoring = true;
            update.transition = None;
            update.retiring.clear();
            if save(&state, &deployment).await.is_err() {
                return;
            }
            if let Err(error) = restore(&state, &mut deployment).await {
                retain_failure(&state, &mut deployment, error).await;
            }
        }
    });
}

/// Queued admission may outlive another Deploy's drain or an external Pipe edit.
async fn fresh_ingress(
    state: &AppState,
    deployment: &MicroServiceDeployment,
) -> Result<(), String> {
    let snapshot = inspect(state).await?;
    if !snapshot.ingress_graceful_drain {
        return Err("Apply the current Pipe configuration before retrying Deploy".into());
    }
    let workers = snapshot
        .ingress_workers
        .as_ref()
        .filter(|workers| !workers.is_empty())
        .ok_or("Cannot verify Nginx workers before Deploy; retry after starting ingress")?;
    if workers.iter().any(|worker| worker.draining) {
        return Err(
            "An earlier Nginx reload is still draining; retry after its requests finish".into(),
        );
    }
    current_route(&snapshot, deployment)?;
    Ok(())
}

/// The commit and worker dispatch outlive a cancelled HTTP request. Keep the
/// mutation claim in the task so another request cannot publish a second owner.
pub(super) async fn submit(
    state: AppState,
    deployment: MicroServiceDeployment,
    claim: tokio::sync::OwnedMutexGuard<()>,
    request: Uuid,
) -> Result<MicroServiceDeployment, AppError> {
    tokio::spawn(async move {
        let _claim = claim;
        let store = state.store.clone();
        let record = deployment.clone();
        database(request, move || store.save_service_deployment(&record)).await?;
        state.service_deploy_changed.notify_waiters();
        resume_worker(state.clone(), deployment.clone());
        Ok::<_, AppError>(deployment)
    })
    .await
    .map_err(|_| AppError::internal(request))?
}

async fn retain_failure(state: &AppState, deployment: &mut MicroServiceDeployment, error: String) {
    deployment.update.as_mut().expect("journal").completed = false;
    let original = deployment
        .update
        .as_ref()
        .and_then(|update| update.failure.as_deref());
    deployment.error = Some(match original {
        Some(original) if original != error => format!("{original}. Recovery pending: {error}"),
        _ => error.clone(),
    });
    if let Err(error) = phase(
        state,
        deployment,
        ServicePhase::Failed,
        format!("{error}; owned VMs retained. Retry Deploy to reconcile."),
    )
    .await
    {
        tracing::error!(%error, "could not persist Deploy failure");
    }
}

async fn execute_update(
    state: &AppState,
    deployment: &mut MicroServiceDeployment,
) -> Result<(), String> {
    if deployment
        .update
        .as_ref()
        .expect("journal")
        .transition
        .is_some()
    {
        finish_transition(state, deployment).await?;
    }
    let needs_instances = deployment.instances.len() < usize::from(deployment.spec.replicas)
        || deployment
            .instances
            .iter()
            .any(|instance| !instance.healthy);
    if needs_instances {
        require_image_auth(state, &deployment.spec.image, deployment.id)
            .await
            .map_err(|_| "Image account is no longer connected in Auth")?;
        secrets::capture(state, deployment).await?;
        prepare_template(state, deployment, false).await?;
    }
    let strategy = deployment.update.as_ref().expect("journal").strategy;
    if deployment.instances.is_empty() {
        check_update_capacity(state, deployment).await?;
    }
    loop {
        let snapshot = inspect(state).await?;
        let route = current_route(&snapshot, deployment)?;
        let ready = endpoints(deployment)?;
        let missing_route = ready
            .iter()
            .any(|address| !route.upstreams.contains(address));
        if strategy == DeployStrategy::Rolling && missing_route {
            let previous = &deployment.update.as_ref().expect("journal").previous;
            let retiring = previous
                .instances
                .first()
                .map(|instance| vec![instance.vm_id])
                .unwrap_or_default();
            let mut next = ready;
            next.extend(endpoints_skipping(previous, &retiring)?);
            begin_transition(state, deployment, next, retiring, false).await?;
            finish_transition(state, deployment).await?;
            continue;
        }
        if let Some(index) = deployment
            .instances
            .iter()
            .position(|instance| !instance.healthy)
        {
            start_replica(state, deployment, false, index).await?;
            continue;
        }
        if deployment.instances.len() < usize::from(deployment.spec.replicas) {
            check_surge(state, &deployment.spec, 1).await?;
            let index = deployment.instances.len();
            start_replica(state, deployment, false, index).await?;
            continue;
        }
        let previous = &deployment.update.as_ref().expect("journal").previous;
        if strategy == DeployStrategy::BlueGreen
            && (!previous.instances.is_empty() || missing_route)
        {
            let retiring = previous
                .instances
                .iter()
                .map(|instance| instance.vm_id)
                .collect();
            begin_transition(state, deployment, ready, retiring, false).await?;
            finish_transition(state, deployment).await?;
            continue;
        }
        if !previous.instances.is_empty() {
            let retiring = vec![previous.instances[0].vm_id];
            let mut next = ready;
            next.extend(endpoints_skipping(previous, &retiring)?);
            begin_transition(state, deployment, next, retiring, false).await?;
            finish_transition(state, deployment).await?;
            continue;
        }
        break;
    }
    let previous = deployment
        .update
        .as_ref()
        .expect("journal")
        .previous
        .clone();
    secrets::remove(state, &previous).await?;
    deployment.update.as_mut().expect("journal").completed = true;
    deployment.error = None;
    phase(
        state,
        deployment,
        ServicePhase::Running,
        "Deploy complete; Nginx serves the new release",
    )
    .await
}

async fn restore(state: &AppState, deployment: &mut MicroServiceDeployment) -> Result<(), String> {
    phase(
        state,
        deployment,
        ServicePhase::RollingBack,
        "Restoring the previous release after Deploy failed",
    )
    .await?;
    if deployment
        .update
        .as_ref()
        .expect("journal")
        .transition
        .is_some()
    {
        finish_transition(state, deployment).await?;
    }
    // Release failed candidates that never entered the current route. Reapply
    // the observed route first so a lost helper reply cannot hide live clients.
    let snapshot = inspect(state).await?;
    let route = current_route(&snapshot, deployment)?;
    let unadmitted = deployment
        .instances
        .iter()
        .filter(|instance| {
            let address =
                format!("{}:{}", instance.ipv4, deployment.spec.port).parse::<SocketAddr>();
            address.is_err() || !route.upstreams.contains(&address.expect("checked address"))
        })
        .map(|instance| instance.vm_id)
        .collect::<Vec<_>>();
    if !unadmitted.is_empty() {
        begin_transition(state, deployment, route.upstreams.clone(), unadmitted, true).await?;
        finish_transition(state, deployment).await?;
    }
    let count = usize::from(target(deployment, true).spec.replicas);
    for index in 0..count {
        let existing = target(deployment, true).instances.get(index).cloned();
        let running = existing
            .as_ref()
            .and_then(|instance| {
                state
                    .vms
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .get(&instance.vm_id)
                    .cloned()
            })
            .filter(|vm| vm.state == VmState::Running);
        if let (Some(instance), Some(vm)) = (existing, running) {
            let previous = target(deployment, true);
            if previous.digest.as_ref() != Some(&vm.template_version) || previous.spec.env != vm.env
            {
                return Err(
                    "Previous Deploy VM no longer matches its release; VMs retained".into(),
                );
            }
            wait_healthy(&previous.spec, &instance).await?;
            target_mut(deployment, true).instances[index].healthy = true;
            save(state, deployment).await?;
            continue;
        }
        prepare_template(state, deployment, true).await?;
        check_surge(state, &target(deployment, true).spec, 1).await?;
        start_replica(state, deployment, true, index).await?;
        let retiring = deployment
            .instances
            .first()
            .map(|instance| vec![instance.vm_id])
            .unwrap_or_default();
        let mut next = endpoints(target(deployment, true))?;
        next.extend(endpoints_skipping(deployment, &retiring)?);
        begin_transition(state, deployment, next, retiring, true).await?;
        finish_transition(state, deployment).await?;
    }
    if !deployment.instances.is_empty() || deployment.update.as_ref().expect("journal").routed_new {
        let next = endpoints(target(deployment, true))?;
        let retiring = deployment
            .instances
            .iter()
            .map(|instance| instance.vm_id)
            .collect();
        begin_transition(state, deployment, next, retiring, true).await?;
        finish_transition(state, deployment).await?;
    }
    deployment.error = deployment
        .update
        .as_ref()
        .expect("journal")
        .failure
        .clone()
        .or_else(|| deployment.error.clone());
    secrets::remove(state, deployment).await?;
    deployment.phase = ServicePhase::Failed;
    deployment.finished_at = Some(chrono::Utc::now().timestamp());
    deployment.update.as_mut().expect("journal").completed = true;
    deployment
        .log
        .push("Previous release restored; the attempted Deploy failed".into());
    let mut previous = (*deployment.update.as_ref().expect("journal").previous).clone();
    previous.phase = ServicePhase::Running;
    previous.domain_route_owned = true;
    previous.error = Some(format!(
        "Last Deploy {} failed; this previous release was restored. {}",
        deployment.id,
        deployment
            .error
            .as_deref()
            .unwrap_or("Inspect the failed operation for details.")
    ));
    previous.log.push(format!(
        "Deploy {} failed; restored this release",
        deployment.id
    ));
    let store = state.store.clone();
    let failed = deployment.clone();
    let restored = previous.clone();
    database(deployment.id, move || {
        store.restore_service_release(&failed, &restored)
    })
    .await
    .map_err(|_| "Could not commit the restored Deploy release")?;
    *deployment = previous;
    cache::schedule(state.clone());
    Ok(())
}

#[cfg(test)]
pub(super) mod tests;

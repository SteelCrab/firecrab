//! Policy edits do not restart VMs or rewrite immutable release history.

use super::*;

pub async fn get(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<AutoDeployStatus>, AppError> {
    let id = vms::parse_id(&id, request.0)?;
    let store = state.store.clone();
    let (service, watch) = database(request.0, move || {
        Ok((store.micro_service(id)?, store.tag_watch(id)?))
    })
    .await?;
    let service = service.ok_or_else(|| AppError::not_found(request.0))?;
    Ok(Json(watch.map(|w| w.status).unwrap_or(AutoDeployStatus {
        policy: service.spec.auto_deploy,
        pause: None,
        observed_digest: None,
        observed_tag: None,
        next_check_at: 0,
        operation_id: None,
        failures: 0,
        error: None,
    })))
}

pub async fn configure(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
    ValidatedJson(policy): ValidatedJson<Option<AutoDeployPolicy>>,
) -> Result<Json<AutoDeployStatus>, AppError> {
    let id = vms::parse_id(&id, request.0)?;
    let guard = state.service_mutations.clone().lock_owned().await;
    crate::handlers::micro_services::ensure_stack_writer(&state, id, None, request.0).await?;
    let store = state.store.clone();
    let (service, mut deployment, watch, owned) = database(request.0, move || {
        Ok((
            store.micro_service(id)?,
            store.service_deployment(id)?,
            store.tag_watch(id)?,
            store.service_is_controller_owned(id)?,
        ))
    })
    .await?;
    let mut service = service.ok_or_else(|| AppError::not_found(request.0))?;
    if owned {
        return Err(AppError::conflict(
            "controller_managed",
            "Configure automatic Deploy on its local host service",
            request.0,
        ));
    }
    if deployment
        .as_ref()
        .is_some_and(|d| d.phase.is_busy() || d.update.as_ref().is_some_and(|u| !u.completed))
    {
        return Err(AppError::conflict(
            "service_busy",
            "Wait for Deploy or finish its recorded recovery",
            request.0,
        ));
    }
    service.spec.auto_deploy = policy.clone();
    crate::handlers::micro_services::validate(&service.spec, request.0)?;
    if let Some(policy) = &policy {
        validate_auth(&state, policy, request.0).await?;
    }
    let watch = policy.map(|policy| {
        watch
            .filter(|w| w.status.policy.as_ref() == Some(&policy))
            .unwrap_or_else(|| Watch::new(policy))
    });
    if let Some(d) = &mut deployment {
        d.spec.auto_deploy = service.spec.auto_deploy.clone();
    }
    let store = state.store.clone();
    tokio::spawn(async move {
        let _guard = guard;
        database(request.0, move || {
            store.set_tag_policy(&service, deployment.as_ref(), watch.as_ref())
        })
        .await
    })
    .await
    .map_err(|_| AppError::internal(request.0))??;
    get(
        State(state.clone()),
        Extension(request),
        Path(id.to_string()),
    )
    .await
}

pub async fn pause(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<AutoDeployStatus>, AppError> {
    control(state, request, id, true).await
}

pub async fn resume(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<String>,
) -> Result<Json<AutoDeployStatus>, AppError> {
    control(state, request, id, false).await
}

async fn control(
    state: AppState,
    request: RequestId,
    id: String,
    pause: bool,
) -> Result<Json<AutoDeployStatus>, AppError> {
    let id = vms::parse_id(&id, request.0)?;
    let guard = state.service_mutations.clone().lock_owned().await;
    let store = state.store.clone();
    let (watch, owned) = database(request.0, move || {
        Ok((store.tag_watch(id)?, store.service_is_controller_owned(id)?))
    })
    .await?;
    if owned {
        return Err(AppError::conflict(
            "controller_managed",
            "Configure automatic Deploy on its local host service",
            request.0,
        ));
    }
    let mut watch = watch.ok_or_else(|| {
        AppError::conflict(
            "auto_deploy_disabled",
            "Enable automatic Deploy before changing its monitoring state",
            request.0,
        )
    })?;
    // Accepted operations retain their identity; pause controls future observations only.
    if let Some(p) = &watch.pending {
        let operation = p.operation;
        let store = state.store.clone();
        if database(request.0, move || store.service_attempt(operation))
            .await?
            .is_none()
        {
            watch.pending = None;
            watch.status.operation_id = None;
        }
    }
    watch.generation = Uuid::new_v4();
    watch.status.pause = pause.then_some(AutoDeployPause::Manual);
    watch.status.error = None;
    if !pause {
        watch.blocked = None;
        watch.handled = None;
        watch.status.failures = 0;
        watch.status.next_check_at = 0;
    }
    let status = watch.status.clone();
    tokio::spawn(async move {
        let _guard = guard;
        persist(&state, id, watch).await
    })
    .await
    .map_err(|_| AppError::internal(request.0))??;
    Ok(Json(status))
}

pub(crate) async fn validate_auth(
    state: &AppState,
    policy: &AutoDeployPolicy,
    request: Uuid,
) -> Result<(), AppError> {
    let reference = rules::reference(policy).map_err(|message| {
        AppError::validation(
            BTreeMap::from([("autoDeploy.reference".into(), message.into())]),
            request,
        )
    })?;
    require_image_auth(
        state,
        &format!("{}/{}:latest", reference.registry, reference.repository),
        request,
    )
    .await?;
    Ok(())
}

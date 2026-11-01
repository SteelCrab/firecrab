//! Capture the saved assignment before reserving an invocation.
use super::*;
pub(super) async fn list(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<Uuid>,
) -> Result<Json<Vec<CodeAgentRun>>, AppError> {
    operator(caller, id)?;
    let rows = state
        .store
        .code_agent_runs(key)
        .map_err(|_| AppError::internal(id.0))?;
    Ok(Json(rows.into_iter().map(|r| repair(&state, r)).collect()))
}
pub(super) async fn read(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path((key, run)): Path<(Uuid, Uuid)>,
) -> Result<Json<CodeAgentRun>, AppError> {
    operator(caller, id)?;
    let (r, _) = state
        .store
        .code_agent_run(key, run)
        .map_err(|_| AppError::internal(id.0))?
        .ok_or_else(|| AppError::not_found(id.0))?;
    Ok(Json(repair(&state, r)))
}
pub(super) async fn cancel(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path((key, run)): Path<(Uuid, Uuid)>,
) -> Result<Json<CodeAgentRunCancellation>, AppError> {
    operator(caller, id)?;
    let r = state
        .store
        .cancel_code_agent_run(key, run, false)
        .map_err(|_| AppError::internal(id.0))?;
    if r.is_some()
        && let Some(signal) = state
            .code_agent_runs
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&run)
    {
        let _ = signal.send(true);
    }
    Ok(Json(CodeAgentRunCancellation {
        id: run,
        workspace_id: key,
        run: r.map(|r| repair(&state, r)),
    }))
}
pub(super) async fn start(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path((key, run)): Path<(Uuid, Uuid)>,
    ValidatedJson(body): ValidatedJson<CodeAgentRunRequest>,
) -> Result<(StatusCode, Json<CodeAgentRun>), AppError> {
    operator(caller, id)?;
    admit(state, key, run, body, id, &native::Native).await
}
pub(crate) async fn admit(
    state: AppState,
    key: Uuid,
    id: Uuid,
    body: CodeAgentRunRequest,
    request_id: RequestId,
    driver: &'static impl Driver,
) -> Result<(StatusCode, Json<CodeAgentRun>), AppError> {
    if key.is_nil()
        || id.is_nil()
        || body.prompt.trim().is_empty()
        || body.prompt.len() > 16384
        || body.prompt.contains('\0')
        || body.model.len() > 200
        || !body
            .model
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.:/".contains(&c))
    {
        return Err(error(request_id, "code_agent_request_invalid"));
    }
    let fingerprint = hash(&body);
    if let Some((r, stored)) = state
        .store
        .code_agent_run(key, id)
        .map_err(|_| AppError::internal(request_id.0))?
    {
        return if fingerprint == stored {
            Ok((StatusCode::ACCEPTED, Json(repair(&state, r))))
        } else {
            Err(error(request_id, "code_agent_request_conflict"))
        };
    }
    let claim = crate::code_runtime::lock(&state, key)
        .map_err(|_| error(request_id, "code_runtime_busy"))?;
    let snapshot = state
        .store
        .code_verification_snapshot(key)
        .map_err(|_| AppError::internal(request_id.0))?
        .ok_or_else(|| AppError::not_found(request_id.0))?;
    let assignment = snapshot
        .workspace
        .spec
        .agent
        .as_ref()
        .ok_or_else(|| error(request_id, "code_agent_not_assigned"))?;
    let saved = snapshot
        .ai
        .as_ref()
        .ok_or_else(|| error(request_id, "code_agent_auth_missing"))?;
    if saved.api_key.is_empty() {
        return Err(error(request_id, "code_agent_auth_missing"));
    }
    if body.model.contains(&saved.api_key) {
        return Err(error(request_id, "code_agent_request_invalid"));
    }
    let profile = code_agent_auth::resolve(assignment, saved)
        .filter(|p| p.supported)
        .ok_or_else(|| error(request_id, "code_agent_adapter_unavailable"))?;
    if snapshot.workspace.revision != body.expected_revision {
        return Err(error(request_id, "code_agent_model_or_revision_changed"));
    }
    let record = state
        .store
        .code_runtime(key)
        .map_err(|_| AppError::internal(request_id.0))?
        .ok_or_else(|| error(request_id, "code_agent_runtime_required"))?;
    if record.spec.github != snapshot.workspace.spec.github
        || record.spec.runtime != snapshot.workspace.spec.runtime
        || record.spec.resources != snapshot.workspace.spec.resources
    {
        return Err(error(request_id, "code_agent_runtime_changed"));
    }
    if record.runtime.vm_id != body.vm_id
        || record.runtime.runtime_id != Some(body.runtime_id)
        || record.runtime.operation_id != body.operation_id
        || crate::code_runtime::auth_binding(&state, &record).is_err()
    {
        return Err(error(request_id, "code_agent_runtime_changed"));
    }
    let now = chrono::Utc::now().timestamp_millis();
    let r = CodeAgentRun {
        id,
        workspace_id: key,
        revision: body.expected_revision,
        connection_id: assignment.connection_id,
        agent: assignment.agent,
        adapter_id: profile.id.clone(),
        cli_version: profile.cli_version.clone(),
        model: body.model.clone(),
        models_used: Vec::new(),
        vm_id: body.vm_id,
        runtime_id: body.runtime_id,
        operation_id: body.operation_id,
        phase: Phase::Queued,
        output: String::new(),
        truncated: false,
        provider_requests: 0,
        exit_code: None,
        error: None,
        created_at_ms: now,
        updated_at_ms: now,
    };
    if !state
        .store
        .admit_code_agent_run(&snapshot, &record, &r, &fingerprint)
        .map_err(|_| AppError::internal(request_id.0))?
    {
        return Err(error(request_id, "code_agent_bindings_changed"));
    }
    let context = Context {
        snapshot,
        record,
        profile,
        request: body,
    };
    let (send, recv) = watch::channel(false);
    state
        .code_agent_runs
        .0
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(id, send);
    let result = r.clone();
    tokio::spawn(async move {
        let _claim = claim;
        execute(state, context, r, recv, driver).await;
    });
    Ok((StatusCode::ACCEPTED, Json(result)))
}

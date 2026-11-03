//! Authorized operator controls remain queryable when the private project DB is damaged.
use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/backups",
            get(list),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/backups/binding",
            get(backup_binding),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/backups/policy",
            get(policy).put(save_policy),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/backups/operations/{operation}",
            get(operation).put(submit).delete(abort),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/backups/operations/{operation}/retry",
            post(retry),
        )
}
pub(super) fn record(
    _state: &AppState,
    b: Binding,
    id: Uuid,
    caller: Caller,
    restore: Option<Uuid>,
    scheduled: bool,
) -> Record {
    Record {
        public: ProjectBackupOperation {
            id,
            project_id: b.project_id,
            request: ProjectBackupRequest {
                expected_generation: b.generation,
                restore_checkpoint: restore,
            },
            phase: "pending".into(),
            pending_reconciliation: false,
            protected: false,
            cleanup_id: None,
            cleaned_at: None,
            created_at: ai_sandbox::now(),
            updated_at: ai_sandbox::now(),
            manifest: None,
            restored_generation: None,
            error: None,
        },
        binding: b,
        caller,
        active: false,
        scheduled,
        original: None,
    }
}
pub(super) fn permit(
    state: &AppState,
    caller: Caller,
    project: Uuid,
    request: Uuid,
    write: bool,
) -> Result<(), AppError> {
    caller.operator().map_err(|_| denied(request))?;
    authorize(
        state,
        caller,
        project,
        if write {
            SandboxPermission::Write
        } else {
            SandboxPermission::Read
        },
        request,
    )?;
    Ok(())
}
async fn list(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
) -> Result<Json<Vec<ProjectBackupOperation>>, AppError> {
    permit(&state, caller, project, id.0, false)?;
    blocking(id.0, move || {
        Ok(state
            .store
            .backup_records(Some(project))?
            .into_iter()
            .map(|r| r.response())
            .collect())
    })
    .await
    .map(Json)
}
async fn policy(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
) -> Result<Json<ProjectBackupPolicy>, AppError> {
    permit(&state, caller, project, id.0, false)?;
    blocking(id.0, move || Ok(state.store.backup_policy(project)?))
        .await
        .map(Json)
}
async fn save_policy(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
    ValidatedJson(input): ValidatedJson<SaveProjectBackupPolicy>,
) -> Result<Json<ProjectBackupPolicy>, AppError> {
    permit(&state, caller, project, id.0, true)?;
    if !(300..=604800).contains(&input.interval_seconds) {
        return Err(
            Error::Invalid("Backup interval must be between five minutes and seven days")
                .response(id.0),
        );
    }
    blocking(id.0, move || {
        let b = raw_binding(&state, project)?;
        if b.generation != input.expected_generation {
            return Err(Error::Conflict(
                "Reload the project before changing backup policy",
            ));
        }
        if input.enabled {
            let db = open_as(&state, &b, caller)?;
            if !db.document()?.2 {
                return Err(Error::Conflict(
                    "Initialize the project before enabling scheduled backups",
                ));
            }
        }
        let mut p = state.store.backup_policy(project)?;
        p.enabled = input.enabled;
        p.interval_seconds = input.interval_seconds;
        p.next_run_at = ai_sandbox::now();
        p.error = None;
        state.store.save_backup_policy(project, &p)?;
        Ok(p)
    })
    .await
    .map(Json)
}
fn retained(state: &AppState, project: Uuid, id: Uuid) -> Result<Record, Error> {
    state
        .store
        .backup_record(id)?
        .filter(|r| r.public.project_id == project)
        .ok_or(Error::Missing)
}
async fn operation(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
    Path((project, id)): Path<(Uuid, Uuid)>,
) -> Result<Json<ProjectBackupOperation>, AppError> {
    permit(&state, caller, project, request.0, false)?;
    blocking(request.0, move || {
        Ok(retained(&state, project, id)?.response())
    })
    .await
    .map(Json)
}
async fn submit(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
    Path((project, id)): Path<(Uuid, Uuid)>,
    ValidatedJson(input): ValidatedJson<ProjectBackupRequest>,
) -> Result<Json<ProjectBackupOperation>, AppError> {
    permit(&state, caller, project, request.0, true)?;
    if id.is_nil() || input.restore_checkpoint.is_some_and(|x| x.is_nil()) {
        return Err(Error::Invalid("Use a unique nonzero backup operation ID").response(request.0));
    }
    let local = state.clone();
    let r = blocking(request.0, move || {
        if let Some(r) = local.store.backup_record(id)? {
            if r.public.project_id != project || r.public.request != input {
                return Err(Error::Conflict(
                    "Operation ID already has another immutable request",
                ));
            }
            return Ok(r);
        }
        let b = raw_binding(&local, project)?;
        if b.generation != input.expected_generation {
            return Err(Error::Conflict(
                "Workspace binding changed; reload before retrying",
            ));
        }
        Ok(record(
            &local,
            b,
            id,
            caller,
            input.restore_checkpoint,
            false,
        ))
    })
    .await?;
    run(state, r, true, request.0).await
}
async fn run(
    state: AppState,
    r: Record,
    fresh: bool,
    id: Uuid,
) -> Result<Json<ProjectBackupOperation>, AppError> {
    // Detached execution owns every lifecycle lock until the last durable write.
    tokio::spawn(execute(state, r, fresh))
        .await
        .map_err(|_| AppError::internal(id))?
        .map_err(|e| e.response(id))
        .map(Json)
}
async fn retry(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
    Path((project, id)): Path<(Uuid, Uuid)>,
) -> Result<Json<ProjectBackupOperation>, AppError> {
    permit(&state, caller, project, request.0, true)?;
    let local = state.clone();
    let r = blocking(request.0, move || retained(&local, project, id)).await?;
    run(state, r, false, request.0).await
}
async fn abort(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
    Path((project, id)): Path<(Uuid, Uuid)>,
) -> Result<Json<ProjectBackupOperation>, AppError> {
    permit(&state, caller, project, request.0, true)?;
    tokio::spawn(async move {
        let admission = state.sandbox.admission.clone().lock_owned().await;
        let local = state.clone();
        let r = blocking(request.0, move || retained(&local, project, id)).await?;
        let vm = state.lock_vm(r.binding.vm_id).await;
        blocking(request.0, move || {
            let _admission = admission;
            let _vm = vm;
            let directory = root(&state.workspaces, project, true)?;
            let _owner =
                db::OwnerLock::acquire(db::private_file(&directory.join("operation.lock"), true)?)?;
            let mut r = retained(&state, project, id)?;
            if r.public.cleanup_id.is_some() || state.store.workspace_cleanup_active(project)? {
                return Err(Error::Busy);
            }
            if !matches!(
                r.public.phase.as_str(),
                "pending" | "database_frozen" | "prepared" | "aborted"
            ) {
                return Err(Error::Conflict(
                    "Installation has begun; resume this operation instead of aborting",
                ));
            }
            r.active = true;
            progress(&state, &mut r, "aborted")?;
            let fence = directory.join("fence.json");
            if fence_id(&fence)?.is_some() {
                clear_fence(&fence, id)?;
            }
            r.active = false;
            state.store.save_backup_record(&r)?;
            Ok(r.response())
        })
        .await
        .map(Json)
    })
    .await
    .map_err(|_| AppError::internal(request.0))?
}

// The recovery screen must work without opening the database it needs to replace.
async fn backup_binding(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    permit(&state, caller, project, id.0, false)?;
    blocking(id.0, move || {
        Ok(serde_json::json!({"generation":raw_binding(&state,project)?.generation}))
    })
    .await
    .map(Json)
}

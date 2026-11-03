//! Operator-only policy, protection, preview and exact-request cleanup APIs.
use super::*;
use crate::project_workspace::backups::api::permit;
/// Operator endpoints share the existing project backup namespace.
const PREFIX: &str = "/api/ai-sandbox/projects/{project}/workspace/backups";
/// Separate irreversible cleanup from the existing pre-installation backup abort endpoint.
pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(&format!("{PREFIX}/retention"), get(policy).put(save_policy))
        .route(&format!("{PREFIX}/retention/preview"), get(preview))
        .route(
            &format!("{PREFIX}/operations/{{operation}}/protection"),
            axum::routing::put(protect),
        )
        .route(&format!("{PREFIX}/cleanups"), get(list))
        .route(
            &format!("{PREFIX}/cleanups/{{operation}}"),
            get(operation).put(submit),
        )
        .route(
            &format!("{PREFIX}/cleanups/{{operation}}/retry"),
            post(retry),
        )
}
/// Return the control-owned policy even when the project database cannot be opened.
async fn policy(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
) -> Result<Json<ProjectBackupRetention>, AppError> {
    permit(&state, caller, project, id.0, false)?;
    blocking(id.0, move || Ok(state.store.backup_retention(project)?))
        .await
        .map(Json)
}
/// Validate bounds and save against the acknowledged generation in one control transaction.
async fn save_policy(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
    ValidatedJson(input): ValidatedJson<SaveProjectBackupRetention>,
) -> Result<Json<ProjectBackupRetention>, AppError> {
    permit(&state, caller, project, id.0, true)?;
    if !(1..=63).contains(&input.keep_latest)
        || input.max_age_days.is_some_and(|d| !(1..=3650).contains(&d))
        || input.expected_generation == 0
        || input.expected_generation > i64::MAX as u64
    {
        return Err(Error::Invalid(
            "Keep one through 63 recent backups to reserve a rotation slot, and use an age from one through 3650 days",
        )
        .response(id.0));
    }
    blocking(id.0, move || {
        let mut p = state.store.backup_retention(project)?;
        p.enabled = input.enabled;
        p.keep_latest = input.keep_latest;
        p.max_age_days = input.max_age_days;
        p.include_manual = input.include_manual;
        p.next_run_at = ai_sandbox::now();
        p.error = None;
        state
            .store
            .save_backup_retention(project, input.expected_generation, &p)?;
        Ok(p)
    })
    .await
    .map(Json)
}
/// Report metadata eligibility only; admission performs filesystem and survivor checks.
async fn preview(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
) -> Result<Json<ProjectBackupCleanupPreview>, AppError> {
    permit(&state, caller, project, id.0, false)?;
    blocking(id.0, move || {
        selection::preview(&state, project, &state.store.backup_retention(project)?)
    })
    .await
    .map(Json)
}
/// Serialize protection edits against backup, restoration and cleanup admission.
async fn protect(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path((project, operation)): Path<(Uuid, Uuid)>,
    ValidatedJson(input): ValidatedJson<SaveProjectBackupProtection>,
) -> Result<Json<ProjectBackupOperation>, AppError> {
    permit(&state, caller, project, id.0, true)?;
    blocking(id.0, move || {
        let directory = root(&state.workspaces, project, true)?;
        let _operation =
            db::OwnerLock::acquire(db::private_file(&directory.join("operation.lock"), true)?)?;
        let b = raw_binding(&state, project)?;
        if b.generation != input.expected_generation {
            return Err(Error::Conflict(
                "Reload the project before changing backup protection",
            ));
        }
        if fenced(&state.workspaces, project)?
            || state.store.workspace_backup_active(project)?
            || state.store.workspace_cleanup_active(project)?
        {
            return Err(Error::Busy);
        }
        let mut r = state
            .store
            .backup_record(operation)?
            .filter(|r| r.public.project_id == project)
            .ok_or(Error::Missing)?;
        if r.public.phase != "complete"
            || r.active
            || r.public.request.restore_checkpoint.is_some()
            || r.public.cleanup_id.is_some()
        {
            return Err(Error::Conflict(
                "Only a complete retained checkpoint can change protection",
            ));
        }
        r.public.protected = input.protected;
        state.store.save_backup_record(&r)?;
        Ok(r.response())
    })
    .await
    .map(Json)
}
/// Return bounded public history, including the active receipt but excluding private file plans.
async fn list(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
) -> Result<Json<Vec<ProjectBackupCleanupOperation>>, AppError> {
    permit(&state, caller, project, id.0, false)?;
    blocking(id.0, move || Ok(state.store.cleanup_history(project)?))
        .await
        .map(Json)
}
/// Retrieve an exact old receipt independently of the bounded history list.
async fn operation(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path((project, operation)): Path<(Uuid, Uuid)>,
) -> Result<Json<ProjectBackupCleanupOperation>, AppError> {
    permit(&state, caller, project, id.0, false)?;
    blocking(id.0, move || {
        Ok(retained(&state, project, operation)?.response())
    })
    .await
    .map(Json)
}
/// Detach owned execution so a closed browser cannot cancel admitted deletion.
async fn run(
    state: AppState,
    project: Uuid,
    id: Uuid,
    input: ProjectBackupCleanupRequest,
    caller: Caller,
    request_id: Uuid,
) -> Result<Json<ProjectBackupCleanupOperation>, AppError> {
    tokio::spawn(execute(state, project, id, input, caller, None))
        .await
        .map_err(|_| AppError::internal(request_id))?
        .map(Json)
        .map_err(|e| e.response(request_id))
}
/// Admit a caller-chosen stable identity and an explicit immutable target selection.
async fn submit(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
    Path((project, id)): Path<(Uuid, Uuid)>,
    ValidatedJson(input): ValidatedJson<ProjectBackupCleanupRequest>,
) -> Result<Json<ProjectBackupCleanupOperation>, AppError> {
    permit(&state, caller, project, request.0, true)?;
    if id.is_nil() {
        return Err(Error::Invalid("Use a nonzero cleanup operation ID").response(request.0));
    }
    run(state, project, id, input, caller, request.0).await
}
/// Resume the server-retained target list rather than accepting a replacement selection.
async fn retry(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
    Path((project, id)): Path<(Uuid, Uuid)>,
) -> Result<Json<ProjectBackupCleanupOperation>, AppError> {
    permit(&state, caller, project, request.0, true)?;
    let local = state.clone();
    let r = blocking(request.0, move || retained(&local, project, id)).await?;
    run(state, project, id, r.public.request, caller, request.0).await
}

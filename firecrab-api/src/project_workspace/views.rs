//! View state never accepts a user ID: identity comes only from the verified transport.
use super::*;
use axum::extract::{DefaultBodyLimit, Query};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftPath {
    /// Validated workspace-relative buffer path.
    path: String,
}

/// Register only caller-scoped view and draft endpoints.
pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/views",
            get(list),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/views/{view}",
            get(read).put(save),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/views/{view}/drafts",
            get(drafts),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/views/{view}/draft",
            get(draft).put(save_draft).delete(delete_draft),
        )
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
}
/// Authorize before and after a bounded private database operation.
async fn work<T: Send + 'static>(
    state: AppState,
    caller: Caller,
    project: Uuid,
    write: bool,
    id: Uuid,
    operation: impl FnOnce(&db::ProjectDb) -> Result<T, Error> + Send + 'static,
) -> Result<T, AppError> {
    authorize(&state, caller, project, SandboxPermission::Read, id)?;
    if write {
        authorize(&state, caller, project, SandboxPermission::Write, id)?;
    }
    blocking(id, move || {
        let permission = || -> Result<(), Error> {
            caller
                .project(&state, project, SandboxPermission::Read)
                .map_err(|_| Error::Denied)?;
            if write {
                caller
                    .project(&state, project, SandboxPermission::Write)
                    .map_err(|_| Error::Denied)?;
            }
            Ok(())
        };
        permission()?;
        let db = open(&state, &binding(&state, project)?)?;
        permission()?;
        let result = operation(&db)?;
        permission()?;
        Ok(result)
    })
    .await
}
/// List this caller’s private view identities without loading buffers.
async fn list(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(project): Path<Uuid>,
    caller: Caller,
) -> Result<Json<Vec<ProjectViewSummary>>, AppError> {
    work(state, caller, project, false, id.0, move |db| {
        db.views(caller)
    })
    .await
    .map(Json)
}
/// Return this caller’s saved layout for a stable tab UUID.
async fn read(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path((project, view)): Path<(Uuid, Uuid)>,
    caller: Caller,
) -> Result<Json<ProjectView>, AppError> {
    work(state, caller, project, false, id.0, move |db| {
        db.view(caller, view)
    })
    .await
    .map(Json)
}
/// Save an optimistic private layout without requiring shared write access.
async fn save(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path((project, view)): Path<(Uuid, Uuid)>,
    caller: Caller,
    ValidatedJson(request): ValidatedJson<SaveProjectView>,
) -> Result<Json<ProjectPrivateSave>, AppError> {
    // A read-only collaborator can arrange their own view without changing shared project data.
    work(state, caller, project, false, id.0, move |db| {
        db.save_view(caller, view, &request)
    })
    .await
    .map(Json)
}
/// List active private buffers without returning their contents.
async fn drafts(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path((project, view)): Path<(Uuid, Uuid)>,
    caller: Caller,
) -> Result<Json<Vec<ProjectDraftSummary>>, AppError> {
    work(state, caller, project, false, id.0, move |db| {
        db.drafts(caller, view)
    })
    .await
    .map(Json)
}
/// Read a private buffer or its retained discard revision.
async fn draft(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path((project, view)): Path<(Uuid, Uuid)>,
    caller: Caller,
    Query(path): Query<DraftPath>,
) -> Result<Json<ProjectDraft>, AppError> {
    work(state, caller, project, false, id.0, move |db| {
        db.draft(caller, view, &path.path)
    })
    .await
    .map(Json)
}
/// Retain unsaved bytes without changing the shared file.
async fn save_draft(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path((project, view)): Path<(Uuid, Uuid)>,
    caller: Caller,
    Query(path): Query<DraftPath>,
    ValidatedJson(request): ValidatedJson<SaveProjectDraft>,
) -> Result<Json<ProjectPrivateSave>, AppError> {
    work(state, caller, project, true, id.0, move |db| {
        db.save_draft(caller, view, &path.path, &request, false)
    })
    .await
    .map(Json)
}
/// Discard bytes while keeping a revision that rejects stale retries.
async fn delete_draft(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path((project, view)): Path<(Uuid, Uuid)>,
    caller: Caller,
    Query(path): Query<DraftPath>,
    ValidatedJson(request): ValidatedJson<DeleteProjectDraft>,
) -> Result<Json<ProjectPrivateSave>, AppError> {
    work(state, caller, project, true, id.0, move |db| {
        db.save_draft(
            caller,
            view,
            &path.path,
            &SaveProjectDraft {
                operation_id: request.operation_id,
                expected_revision: request.expected_revision,
                base_version: 0,
                content_base64: String::new(),
            },
            true,
        )
    })
    .await
    .map(Json)
}

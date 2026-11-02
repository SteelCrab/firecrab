//! Project skill installation reuses native versioned writes and Sandbox admission.
pub(crate) mod db;
mod source;
pub(crate) mod tasks;
#[cfg(test)]
mod tests;
use super::*;
use files_db::Operation;
/// Register project-scoped catalog, installation and removal operations.
pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/skills/catalog",
            get(catalog),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/skills/catalog/{source}",
            post(refresh),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/skills/installations",
            get(list),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/skills/installations/{installation}",
            get(read).put(install).delete(remove),
        )
}
/// Authorize before opening private SQLite; completed metadata stays available while stopped.
fn owned_db(
    state: &AppState,
    caller: Caller,
    project: Uuid,
    write: bool,
    id: Uuid,
) -> Result<super::db::ProjectDb, Error> {
    authorize(state, caller, project, SandboxPermission::Read, id).map_err(|_| Error::Denied)?;
    if write {
        authorize(state, caller, project, SandboxPermission::Write, id)
            .map_err(|_| Error::Denied)?;
    }
    let b = binding(state, project)?;
    open(state, &b)
}
/// Return retained discovery results without performing an implicit source request.
async fn catalog(
    State(state): State<AppState>,
    Extension(r): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
) -> Result<Json<Vec<ProjectSkillCatalog>>, AppError> {
    blocking(r.0, move || {
        let db = owned_db(&state, caller, project, false, r.0)?;
        let result = db.skill_catalogs()?;
        caller
            .project(&state, project, SandboxPermission::Read)
            .map_err(|_| Error::Denied)?;
        Ok(Json(result))
    })
    .await
}
/// Refresh one fixed source, keeping the old accepted catalog on network failure.
async fn refresh(
    State(state): State<AppState>,
    Extension(r): Extension<RequestId>,
    caller: Caller,
    Path((project, source)): Path<(Uuid, ProjectSkillSource)>,
) -> Result<Json<ProjectSkillCatalog>, AppError> {
    authorize(&state, caller, project, SandboxPermission::Read, r.0)?;
    authorize(&state, caller, project, SandboxPermission::Write, r.0)?;
    let result = source::discover(source)
        .await
        .map_err(|e| e.response(r.0))?;
    blocking(r.0, move || {
        let db = owned_db(&state, caller, project, true, r.0)?;
        db.save_skill_catalog(&result)?;
        Ok(Json(result))
    })
    .await
}
/// List durable package states independently of individual browser selections.
async fn list(
    State(state): State<AppState>,
    Extension(r): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
) -> Result<Json<Vec<ProjectSkillInstallation>>, AppError> {
    blocking(r.0, move || {
        let db = owned_db(&state, caller, project, false, r.0)?;
        let result = db.skill_installations()?;
        caller
            .project(&state, project, SandboxPermission::Read)
            .map_err(|_| Error::Denied)?;
        Ok(Json(result))
    })
    .await
}
/// Read the same accepted installation after an ambiguous client response.
async fn read(
    State(state): State<AppState>,
    Extension(r): Extension<RequestId>,
    caller: Caller,
    Path((project, id)): Path<(Uuid, Uuid)>,
) -> Result<Json<ProjectSkillInstallation>, AppError> {
    blocking(r.0, move || {
        let db = owned_db(&state, caller, project, false, r.0)?;
        let result = db.skill_record(id)?;
        caller
            .project(&state, project, SandboxPermission::Read)
            .map_err(|_| Error::Denied)?;
        Ok(Json(result.public))
    })
    .await
}
/// Admit the complete downloaded package before starting any retained-volume write.
fn record(
    id: Uuid,
    caller: Caller,
    entry: ProjectSkillEntry,
    blobs: Vec<source::Blob>,
) -> Result<db::Record, Error> {
    if id.is_nil() {
        return Err(Error::Invalid("Use a stable installation UUID"));
    }
    let mut files = Vec::new();
    for blob in &blobs {
        let bytes = STANDARD
            .decode(&blob.base64)
            .map_err(|_| Error::Invalid("Invalid source bytes"))?;
        files.push(ProjectSkillFile {
            path: blob.path.clone(),
            operation_id: Uuid::new_v4(),
            version: 0,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            byte_length: bytes.len() as u64,
        });
    }
    let sha256 = db::digest(&files)?;
    let record = db::Record {
        public: ProjectSkillInstallation {
            id,
            entry,
            root: format!(".agents/firecrab-skills/{id}"),
            status: ProjectSkillStatus::Pending,
            files,
            sha256,
        },
        caller,
        created_at: crate::ai_sandbox::now(),
        blobs,
    };
    record.validate()?;
    Ok(record)
}
/// Resume exact writes with their original operation IDs; partial installs are never selectable.
async fn resume(
    ctx: &mut files::Files,
    mut record: db::Record,
) -> Result<ProjectSkillInstallation, Error> {
    if record.public.status != ProjectSkillStatus::Pending {
        return Ok(record.public);
    }
    for i in 0..record.public.files.len() {
        let file = &record.public.files[i];
        if file.version != 0 {
            continue;
        }
        let content = STANDARD
            .decode(&record.blobs[i].base64)
            .map_err(|_| Error::Conflict("Retained skill bytes changed"))?;
        let result = ctx
            .write(Operation {
                id: file.operation_id,
                path: format!("{}/{}", record.public.root, file.path),
                expected: 0,
                content,
                author: record.caller.principal,
                restored_from: None,
                created_at: record.created_at,
                version: None,
                state: "pending".into(),
            })
            .await?;
        if result.version.sha256 != file.sha256 {
            return Err(Error::Conflict(
                "Installed skill file failed its source proof",
            ));
        }
        record.public.files[i].version = result.version.id;
        ctx.permission()?;
        ctx.db.save_skill_record(&record)?;
    }
    record.public.status = ProjectSkillStatus::Installed;
    ctx.db.save_skill_record(&record)?;
    Ok(record.public)
}
/// Reuse an existing admission before contacting any official source again.
async fn install(
    State(state): State<AppState>,
    Extension(r): Extension<RequestId>,
    caller: Caller,
    Path((project, id)): Path<(Uuid, Uuid)>,
    ValidatedJson(input): ValidatedJson<InstallProjectSkill>,
) -> Result<Json<ProjectSkillInstallation>, AppError> {
    if id.is_nil() || !source::revision(&input.revision) {
        return Err(
            Error::Invalid("Select a pinned official entry and stable installation UUID")
                .response(r.0),
        );
    }
    files_guest::path(&input.path, false).map_err(|e| e.response(r.0))?;
    terminals::preauthorize(&state, caller, project, r.0)?;
    let local = state.clone();
    let request = input.clone();
    let cached = blocking(r.0, move || {
        let db = owned_db(&local, caller, project, true, r.0)?;
        match db.skill_record(id) {
            Ok(saved) if saved.matches(&request) => Ok(Some(saved)),
            Ok(_) => Err(Error::Conflict(
                "Installation UUID already selects another source entry",
            )),
            Err(Error::Missing) => Ok(None),
            Err(e) => Err(e),
        }
    })
    .await?;
    if let Some(saved) = &cached
        && saved.public.status != ProjectSkillStatus::Pending
    {
        return Ok(Json(saved.public.clone()));
    }
    let admitted = match cached {
        Some(saved) => saved,
        None => {
            let (entry, blobs) = source::package(&input).await.map_err(|e| e.response(r.0))?;
            record(id, caller, entry, blobs).map_err(|e| e.response(r.0))?
        }
    };
    files::run(state.workspaces.file_slots.clone(), r.0, async move {
        let mut ctx = files::Files::open(state, project, caller, true, r.0).await?;
        ctx.require_execution = true;
        ctx.permission().map_err(|e| e.response(r.0))?;
        let admitted = match ctx.db.skill_record(id) {
            Ok(saved) if saved.matches(&input) => saved,
            Ok(_) => return Err(Error::Conflict("Installation UUID changed").response(r.0)),
            Err(Error::Missing) => {
                ctx.db
                    .save_skill_record(&admitted)
                    .map_err(|e| e.response(r.0))?;
                admitted
            }
            Err(e) => return Err(e.response(r.0)),
        };
        resume(&mut ctx, admitted)
            .await
            .map(Json)
            .map_err(|e| e.response(r.0))
    })
    .await
}
/// Remove future catalog membership atomically; accepted task snapshots and historical files survive.
async fn remove(
    State(state): State<AppState>,
    Extension(r): Extension<RequestId>,
    caller: Caller,
    Path((project, id)): Path<(Uuid, Uuid)>,
) -> Result<Json<ProjectSkillInstallation>, AppError> {
    blocking(r.0, move || {
        let db = owned_db(&state, caller, project, true, r.0)?;
        let mut saved = db.skill_record(id)?;
        saved.public.status = ProjectSkillStatus::Removed;
        db.save_skill_record(&saved)?;
        Ok(Json(saved.public))
    })
    .await
}

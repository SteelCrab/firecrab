//! Capture saved root-to-leaf instructions without reading private editor drafts.
use super::*;
use axum::Json;
/// Register stable, caller-private instruction snapshots.
pub(super) fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/ai-sandbox/projects/{project}/workspace/instructions/snapshots/{snapshot}",
        get(read).put(capture),
    )
}
/// Enumerate only the project root and ancestors of the selected directory.
pub(super) fn paths(directory: &str) -> Result<Vec<String>, Error> {
    files_guest::path(directory, true)?;
    let mut result = vec!["AGENTS.md".to_string()];
    let mut prefix = String::new();
    for component in directory.split('/').filter(|s| !s.is_empty()) {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(component);
        let path = format!("{prefix}/AGENTS.md");
        files_guest::path(&path, false)?;
        result.push(path);
    }
    Ok(result)
}
/// Hash the exact ordered saved context, independently of request/time identities.
pub(super) fn digest(directory: &str, files: &[ProjectInstructionFile]) -> Result<String, Error> {
    let bytes = serde_json::to_vec(&(directory, files))
        .map_err(|_| Error::Invalid("Invalid saved instruction context"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
/// Reject invalid provenance, excessive rules or an altered retained snapshot.
pub(super) fn validate(snapshot: &ProjectInstructionSnapshot) -> Result<(), Error> {
    if snapshot.id.is_nil() || snapshot.created_at > i64::MAX as u64 {
        return Err(Error::Invalid("Invalid instruction snapshot identity"));
    }
    let candidates = paths(&snapshot.directory)?;
    let mut next = 0usize;
    let mut total = 0usize;
    for file in &snapshot.files {
        let relative = candidates[next..]
            .iter()
            .position(|path| path == &file.path)
            .ok_or(Error::Invalid(
                "Instructions must follow project root-to-leaf order",
            ))?;
        next += relative + 1;
        total = total
            .checked_add(file.content.len())
            .ok_or(Error::Invalid("Instructions exceed their limit"))?;
        if file.version == 0
            || file.content.len() > 32 * 1024
            || total > 64 * 1024
            || file.content.contains('\0')
            || format!("{:x}", Sha256::digest(file.content.as_bytes())) != file.sha256
        {
            return Err(Error::Invalid(
                "Invalid saved instructions or content limit exceeded",
            ));
        }
    }
    if digest(&snapshot.directory, &snapshot.files)? != snapshot.sha256 {
        return Err(Error::Conflict(
            "Retained instruction snapshot checksum changed",
        ));
    }
    Ok(())
}
/// Capture once under the file-operation/runtime fence; a retry never recaptures changed rules.
pub(super) async fn saved_context(
    ctx: &mut files::Files,
    caller: Caller,
    id: Uuid,
    directory: &str,
) -> Result<ProjectInstructionSnapshot, Error> {
    if id.is_nil() {
        return Err(Error::Invalid("Use a stable instruction snapshot UUID"));
    }
    let candidates = paths(directory)?;
    match ctx.db.instruction_snapshot(caller, id) {
        Ok(saved) if saved.directory == directory => return Ok(saved),
        Ok(_) => {
            return Err(Error::Conflict(
                "Instruction snapshot identity already selects another directory",
            ));
        }
        Err(Error::Missing) => {}
        Err(error) => return Err(error),
    }
    ctx.directory(directory).await?;
    let mut files = Vec::new();
    let mut total = 0usize;
    for path in candidates {
        if let Some(version) = ctx.saved_version(&path).await? {
            total = total.saturating_add(version.bytes.len());
            if version.bytes.len() > 32 * 1024 || total > 64 * 1024 {
                return Err(Error::Invalid(
                    "Saved instructions exceed their context limit",
                ));
            }
            let content = String::from_utf8(version.bytes)
                .map_err(|_| Error::Invalid("AGENTS.md must contain UTF-8 text"))?;
            files.push(ProjectInstructionFile {
                path,
                version: version.metadata.id,
                sha256: version.metadata.sha256,
                content,
            });
        }
    }
    let snapshot = ProjectInstructionSnapshot {
        id,
        directory: directory.into(),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Error::Invalid("Invalid server clock"))?
            .as_secs(),
        sha256: digest(directory, &files)?,
        files,
    };
    ctx.permission()?;
    ctx.db.save_instruction_snapshot(caller, &snapshot)?;
    Ok(snapshot)
}
/// Read retained accepted instructions even while the workload VM is stopped.
async fn read(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path((project, snapshot)): Path<(Uuid, Uuid)>,
) -> Result<Json<ProjectInstructionSnapshot>, AppError> {
    authorize(&state, caller, project, SandboxPermission::Read, id.0)?;
    blocking(id.0, move || {
        let b = binding(&state, project)?;
        let db = open_as(&state, &b, caller)?;
        authorize(&state, caller, project, SandboxPermission::Read, id.0)
            .map_err(|_| Error::Denied)?;
        let saved = db.instruction_snapshot(caller, snapshot)?;
        caller
            .project(&state, project, SandboxPermission::Read)
            .map_err(|_| Error::Denied)?;
        Ok(Json(saved))
    })
    .await
}
/// Snapshot creation reads only authorized saved files on a running verified workspace.
async fn capture(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path((project, snapshot)): Path<(Uuid, Uuid)>,
    ValidatedJson(request): ValidatedJson<CaptureProjectInstructions>,
) -> Result<Json<ProjectInstructionSnapshot>, AppError> {
    files::run(state.workspaces.file_slots.clone(), id.0, async move {
        // Completed request retries remain queryable with the VM stopped.
        authorize(&state, caller, project, SandboxPermission::Read, id.0)?;
        if snapshot.is_nil() {
            return Err(Error::Invalid("Use a stable instruction snapshot UUID").response(id.0));
        }
        paths(&request.directory).map_err(|e| e.response(id.0))?;
        let local = state.clone();
        let directory = request.directory.clone();
        let retained = blocking(id.0, move || {
            let b = binding(&local, project)?;
            let db = open_as(&local, &b, caller)?;
            caller
                .project(&local, project, SandboxPermission::Read)
                .map_err(|_| Error::Denied)?;
            let result = match db.instruction_snapshot(caller, snapshot) {
                Ok(saved) if saved.directory == directory => Ok(Some(saved)),
                Ok(_) => Err(Error::Conflict(
                    "Instruction snapshot identity already selects another directory",
                )),
                Err(Error::Missing) => Ok(None),
                Err(error) => Err(error),
            }?;
            caller
                .project(&local, project, SandboxPermission::Read)
                .map_err(|_| Error::Denied)?;
            Ok(result)
        })
        .await?;
        if let Some(retained) = retained {
            return Ok(Json(retained));
        }
        let mut ctx = files::Files::open(state, project, caller, false, id.0).await?;
        saved_context(&mut ctx, caller, snapshot, &request.directory)
            .await
            .map(Json)
            .map_err(|e| e.response(id.0))
    })
    .await
}

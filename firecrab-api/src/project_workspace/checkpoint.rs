//! Offline matched checkpoints. Lifecycle and database locks outlive HTTP cancellation.
use super::*;
use std::io::{Read, Write};

/// Hash a regular file without following its final symlink.
pub(super) fn digest(path: &std::path::Path) -> Result<String, Error> {
    let mut source = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    if !source.metadata()?.is_file() {
        return Err(Error::Conflict("Checkpoint file is not regular"));
    }
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
/// Copy and sync the inactive disk while computing its integrity digest.
fn copy_volume(source: &std::path::Path, destination: &std::path::Path) -> Result<String, Error> {
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count])?;
        hash.update(&buffer[..count]);
    }
    output.sync_all()?;
    Ok(format!("{:x}", hash.finalize()))
}
/// Read a completed matching manifest; retries additionally verify both backup files.
fn completed(
    directory: &std::path::Path,
    binding: &Binding,
    id: Uuid,
    verify: bool,
) -> Result<ProjectWorkspaceCheckpoint, Error> {
    let path = directory.join("manifest.json");
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > 16384 {
        return Err(Error::Conflict("Checkpoint manifest is invalid"));
    }
    let manifest: ProjectWorkspaceCheckpoint = serde_json::from_reader(file)?;
    if manifest.id != id
        || manifest.project_id != binding.project_id
        || manifest.database_id != binding.database_id
        || manifest.volume_id != binding.volume_id
        || manifest.schema_version != db::SCHEMA_VERSION
    {
        return Err(Error::Conflict(
            "Checkpoint identity does not match this project",
        ));
    }
    if verify
        && (digest(&directory.join("project.db"))? != manifest.database_sha256
            || digest(&directory.join("data.ext4"))? != manifest.volume_sha256)
    {
        return Err(Error::Conflict("Checkpoint integrity verification failed"));
    }
    Ok(manifest)
}
/// Publish completion only after a matched database/disk pair is durable.
pub(super) fn create_checkpoint(
    state: &AppState,
    binding: &Binding,
    db: &db::ProjectDb,
    id: Uuid,
) -> Result<ProjectWorkspaceCheckpoint, Error> {
    let root = db.directory.join("checkpoints");
    db::private_dir(&root)?;
    let directory = root.join(id.to_string());
    if directory.exists() {
        if !directory.join("manifest.json").exists() {
            return Err(Error::Conflict(
                "Checkpoint was interrupted; retain it for inspection and use a new checkpoint ID",
            ));
        }
        return completed(&directory, binding, id, true);
    }
    let (revision, _, initialized) = db.document()?;
    if binding.provisioning || !initialized {
        return Err(Error::Conflict(
            "Initialize the project volume before creating its checkpoint",
        ));
    }
    let volume = state.store.block_storage(binding.volume_id)?;
    let source = block_storage::verify_disk(state, &volume)
        .map_err(|_| Error::Conflict("Retained volume is missing or mismatched"))?;
    if volume.attached_vm_id.is_some_and(|vm| vm != binding.vm_id) {
        return Err(Error::Conflict("Volume attachment changed"));
    }
    db::private_dir(&directory)?;
    let database = directory.join("project.db");
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&database)?;
    drop(file);
    db.conn.backup(rusqlite::MAIN_DB, &database, None)?;
    File::open(&database)?.sync_all()?;
    let database_sha256 = digest(&database)?;
    let volume_sha256 = copy_volume(&source, &directory.join("data.ext4"))?;
    let manifest = ProjectWorkspaceCheckpoint {
        id,
        project_id: binding.project_id,
        database_id: binding.database_id,
        volume_id: binding.volume_id,
        generation: binding.generation,
        schema_version: db::SCHEMA_VERSION,
        revision,
        database_sha256,
        volume_sha256,
        created_at: ai_sandbox::now(),
    };
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(directory.join("manifest.partial"))?;
    output.write_all(&serde_json::to_vec(&manifest)?)?;
    output.sync_all()?;
    fs::rename(
        directory.join("manifest.partial"),
        directory.join("manifest.json"),
    )?;
    File::open(&directory)?.sync_all()?;
    File::open(root)?.sync_all()?;
    Ok(manifest)
}

/// Quiesce runtime lifecycle and database writes until the offline copy finishes.
pub(super) async fn create(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
    Path((project, id)): Path<(Uuid, Uuid)>,
) -> Result<Json<ProjectWorkspaceCheckpoint>, AppError> {
    caller.operator().map_err(|_| denied(request.0))?;
    authorize(&state, caller, project, SandboxPermission::Write, request.0)?;
    let b = binding(&state, project).map_err(|e| e.response(request.0))?;
    let guard = state.lock_vm(b.vm_id).await;
    if state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains_key(&b.vm_id)
    {
        handlers::block_storage::require_inactive_user_vm(&state, b.vm_id, request.0)?;
    }
    if state
        .store
        .sandbox_project_busy(project)
        .map_err(|_| AppError::internal(request.0))?
    {
        return Err(
            Error::Conflict("Finish or reconcile project tasks before checkpointing")
                .response(request.0),
        );
    }
    blocking(request.0, move || {
        let _guard = guard;
        let db = open(&state, &b)?;
        if binding(&state, project)? != b {
            return Err(Error::Conflict("Workspace binding changed"));
        }
        create_checkpoint(&state, &b, &db, id)
    })
    .await
    .map(Json)
}
/// Return an authorized completed manifest without exposing private host paths.
pub(super) async fn get(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
    Path((project, id)): Path<(Uuid, Uuid)>,
) -> Result<Json<ProjectWorkspaceCheckpoint>, AppError> {
    authorize(&state, caller, project, SandboxPermission::Read, request.0)?;
    blocking(request.0, move || {
        let b = binding(&state, project)?;
        let db = open(&state, &b)?;
        let directory = db.directory.join("checkpoints").join(id.to_string());
        if !directory.join("manifest.json").exists() {
            return Err(Error::Missing);
        }
        completed(&directory, &b, id, false)
    })
    .await
    .map(Json)
}

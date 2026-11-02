//! Offline paired backups. Durable fences outlive requests, SQLite replacement and service restarts.
mod api;
mod archive;
#[cfg(test)]
mod tests;
use super::*;
use std::path::{Path as FsPath, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Record {
    pub public: ProjectBackupOperation,
    pub binding: Binding,
    pub caller: Caller,
    pub active: bool,
    pub scheduled: bool,
    pub original: Option<archive::Original>,
}
impl Record {
    fn response(&self) -> ProjectBackupOperation {
        let mut public = self.public.clone();
        public.pending_reconciliation = self.active;
        public
    }
}
pub(super) fn routes() -> Router<AppState> {
    api::routes()
}

pub(super) fn root(runtime: &Runtime, project: Uuid, create: bool) -> Result<PathBuf, Error> {
    let parent = runtime
        .root
        .parent()
        .ok_or(Error::Conflict("Invalid workspace root"))?;
    if create {
        db::private_dir(parent)?;
    }
    let parent = if parent.exists() {
        fs::canonicalize(parent)?
    } else {
        parent.to_path_buf()
    };
    let root = parent.join("project-backups");
    let path = root.join(project.to_string());
    if create {
        db::private_dir(&root)?;
        db::private_dir(&path)?;
    } else {
        for p in [&root, &path] {
            match fs::symlink_metadata(p) {
                Ok(_) => db::private_dir(p)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(path)
}
/// Every ordinary database opener checks this before and after acquiring exclusive ownership.
pub(crate) fn fenced(runtime: &Runtime, project: Uuid) -> Result<bool, Error> {
    match fs::symlink_metadata(root(runtime, project, false)?.join("fence.json")) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn fence_id(path: &FsPath) -> Result<Option<Uuid>, Error> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
        Ok(_) => {
            let mut file = db::private_file(path, false)?;
            if file.metadata()?.len() > 128 {
                return Err(Error::Conflict("Invalid backup fence"));
            }
            Ok(Some(serde_json::from_reader(&mut file)?))
        }
    }
}
fn install_fence(path: &FsPath, id: Uuid) -> Result<(), Error> {
    if let Some(current) = fence_id(path)? {
        return if current == id {
            Ok(())
        } else {
            Err(Error::Busy)
        };
    }
    archive::publish(path, &serde_json::to_vec(&id)?)
}
fn clear_fence(path: &FsPath, id: Uuid) -> Result<(), Error> {
    if fence_id(path)? == Some(id) {
        fs::remove_file(path)?;
        File::open(path.parent().ok_or(Error::Missing)?)?.sync_all()?;
    } else {
        return Err(Error::Conflict("Backup fence changed"));
    }
    Ok(())
}
fn progress(state: &AppState, r: &mut Record, phase: &str) -> Result<(), Error> {
    r.public.phase = phase.into();
    r.public.updated_at = ai_sandbox::now();
    r.public.error = None;
    state.store.save_backup_record(r)?;
    Ok(())
}
fn raw_binding(state: &AppState, project: Uuid) -> Result<Binding, Error> {
    state
        .store
        .project_workspace_unfenced(project)?
        .ok_or(Error::Missing)
}
fn offline(state: &AppState, b: &Binding, id: Uuid, restore: bool) -> Result<(), Error> {
    if b.provisioning {
        return Err(Error::Conflict(
            "Initialize the project before backing up or restoring",
        ));
    }
    if state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains_key(&b.vm_id)
    {
        handlers::block_storage::require_inactive_user_vm(state, b.vm_id, id)
            .map_err(|_| Error::Conflict("Stop the project VM before backing up or restoring"))?;
    }
    if !fenced(&state.workspaces, b.project_id)? {
        match state.store.workspace_backup_busy(b.project_id, restore) {
            Ok(false) => {}
            Err(_)
                if restore
                    && state.store.sandbox_storage_status(b.project_id)?["storage"]
                        == "project"
                    && state
                        .sandbox
                        .cancellations
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .is_empty() =>
            {
                // Explicit offline disaster recovery can replace an unreadable canonical DB.
                // Admission remains locked, the permanent service lease excludes another owner,
                // the restored index must be idle, and the global task-ID catalog is not rewound.
            }
            _ => {
                return Err(Error::Conflict(
                    "Finish or reconcile project tasks before backing up or restoring",
                ));
            }
        }
    }
    let volume = state.store.block_storage(b.volume_id)?;
    if volume.attached_vm_id.is_some_and(|vm| vm != b.vm_id)
        || !matches!(volume.state.as_str(), "attached" | "available")
    {
        return Err(Error::Conflict("Retained volume binding changed"));
    }
    Ok(())
}
/// An owned task retains admission, VM and OS locks even when its HTTP response is dropped.
async fn execute(state: AppState, r: Record, fresh: bool) -> Result<ProjectBackupOperation, Error> {
    let capacity = state
        .workspaces
        .backup_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| Error::Busy)?;
    let admission = state.sandbox.admission.clone().lock_owned().await;
    let vm = state.lock_vm(r.binding.vm_id).await;
    tokio::task::spawn_blocking(move || {
        let _capacity = capacity;
        let _admission = admission;
        let _vm = vm;
        execute_locked(&state, r, fresh)
    })
    .await
    .map_err(|_| Error::Conflict("Backup worker interrupted; retain the operation and retry"))?
}
fn execute_locked(
    state: &AppState,
    mut r: Record,
    fresh: bool,
) -> Result<ProjectBackupOperation, Error> {
    let directory = root(&state.workspaces, r.binding.project_id, true)?;
    let _operation =
        db::OwnerLock::acquire(db::private_file(&directory.join("operation.lock"), true)?)?;
    if let Some(existing) = state.store.backup_record(r.public.id)? {
        if existing.public.project_id != r.public.project_id
            || existing.public.request != r.public.request
        {
            return Err(Error::Conflict(
                "Backup operation ID already has another request",
            ));
        }
        r = existing;
    }
    let fence = directory.join("fence.json");
    if matches!(r.public.phase.as_str(), "complete" | "aborted") {
        if fence_id(&fence)? == Some(r.public.id) {
            // No writer can change either target while the durable fence remains.
            if r.public.restored_generation.is_some() {
                archive::verify_installed(state, &r)?;
            }
            clear_fence(&fence, r.public.id)?;
        }
        if r.active || r.public.error.is_some() {
            r.active = false;
            let phase = r.public.phase.clone();
            progress(state, &mut r, &phase)?;
        }
        return Ok(r.response());
    }
    let current = raw_binding(state, r.binding.project_id)?;
    if current != r.binding {
        return Err(Error::Conflict("Workspace binding changed"));
    }
    offline(
        state,
        &current,
        r.public.id,
        r.public.request.restore_checkpoint.is_some(),
    )?;
    r.caller.operator().map_err(|_| Error::Denied)?;
    r.caller
        .project(state, current.project_id, SandboxPermission::Write)
        .map_err(|_| Error::Denied)?;
    if !fresh && !r.active && fence_id(&fence)?.is_none() {
        // Intent alone is not admission. Do not run it after unrelated intervening work.
        progress(state, &mut r, "aborted")?;
        return Ok(r.response());
    }
    let result = (|| {
        if fresh && state.store.backup_record(r.public.id)?.is_none() {
            if fenced(&state.workspaces, current.project_id)?
                || state.store.workspace_backup_active(current.project_id)?
            {
                return Err(Error::Busy);
            }
            if r.public.request.restore_checkpoint.is_none() {
                let count = state
                    .store
                    .backup_records(Some(current.project_id))?
                    .iter()
                    .filter(|x| {
                        x.public.request.restore_checkpoint.is_none()
                            && x.public.phase == "complete"
                    })
                    .count();
                if count >= 64 {
                    return Err(Error::Conflict(
                        "Backup retention limit reached; existing checkpoints are retained and retention maintenance is required",
                    ));
                }
            }
        }
        // Acquire the normal owner's lock without opening or repairing a damaged DB.
        // The permanent service lease lives outside the directory being recovered.
        if r.public.request.restore_checkpoint.is_some() {
            db::private_dir(&state.workspaces.root)?;
            db::private_dir(&state.workspaces.root.join(current.project_id.to_string()))?;
        }
        let lease = state.workspaces.service_lease(&current, false)?;
        if std::sync::Arc::strong_count(&lease) > 2 {
            return Err(Error::Busy);
        }
        let live = db::project_directory(&state.workspaces.root, &current, false)?;
        let owner = db::OwnerLock::acquire(db::private_file(&live.join("owner.lock"), true)?)?;
        if state.store.backup_record(r.public.id)?.is_none() {
            if r.public.request.restore_checkpoint.is_none() {
                // Unhealthy/unreconciled sources defer scheduling without fencing VM startup.
                archive::database(state, &live.join("project.db"), &current)?;
                let volume = state.store.block_storage(current.volume_id)?;
                block_storage::verify_disk(state, &volume).map_err(|_| {
                    Error::Conflict("Retained volume is unavailable; backup was not admitted")
                })?;
            }
            state.store.save_backup_record(&r)?;
        }
        install_fence(&fence, r.public.id)?;
        r.active = true;
        state.store.save_backup_record(&r)?;
        if r.public.request.restore_checkpoint.is_some() {
            archive::restore(state, &directory, &live, &mut r)?;
            state.store.finish_project_restore(&current, &mut r)?;
        } else {
            archive::create(state, &directory, &live, &mut r)?;
            progress(state, &mut r, "complete")?;
        }
        clear_fence(&fence, r.public.id)?;
        r.active = false;
        state.store.save_backup_record(&r)?;
        drop(owner);
        drop(lease);
        Ok(r.response())
    })();
    if let Err(ref error) = result {
        tracing::warn!(project=%r.binding.project_id,operation=%r.public.id,%error,"project backup requires retry");
        if state.store.backup_record(r.public.id)?.is_some() {
            r.public.updated_at = ai_sandbox::now();
            r.public.error=Some("Backup or restoration could not be confirmed. Retain all files and retry this operation.".into());
            state.store.save_backup_record(&r)?;
        }
    }
    result
}
/// Restart recovery never opens a new VM or replays an unadmitted intent.
pub(super) fn start(state: AppState) {
    tokio::spawn(async move {
        loop {
            let local = state.clone();
            if let Err(error) = tick(local).await {
                tracing::warn!(%error,"project backup maintenance failed");
            }
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
}
async fn tick(state: AppState) -> Result<(), Error> {
    let local = state.clone();
    let records = tokio::task::spawn_blocking(move || local.store.backup_records(None))
        .await
        .map_err(|_| Error::Busy)??;
    for r in records {
        if r.active
            || (r.public.phase == "pending" && r.public.error.is_none())
            || fenced(&state.workspaces, r.binding.project_id)?
        {
            // A retained error needs an explicit retry; do not repeatedly copy a failed large disk.
            if r.public.error.is_none() {
                let _ = execute(state.clone(), r, false).await;
            }
        }
    }
    let local = state.clone();
    let projects = tokio::task::spawn_blocking(move || local.store.backup_projects())
        .await
        .map_err(|_| Error::Busy)??;
    for project in projects {
        let mut p = state.store.backup_policy(project)?;
        if !p.enabled || p.next_run_at > ai_sandbox::now() {
            continue;
        }
        let expected = p.clone();
        let b = raw_binding(&state, project)?;
        let r = api::record(&state, b, Uuid::new_v4(), Caller::default(), None, true);
        match execute(state.clone(), r, true).await {
            Ok(op) if op.phase == "complete" => {
                p.last_checkpoint = Some(op.id);
                p.error = None;
                p.next_run_at = ai_sandbox::now().saturating_add(p.interval_seconds);
            }
            Err(Error::Busy) => continue,
            _ => {
                p.error=Some("Waiting for a stopped, idle project with healthy retained storage. Check retained operations before retrying.".into());
                p.next_run_at = ai_sandbox::now().saturating_add(60);
            }
        }
        state
            .store
            .finish_backup_policy_attempt(project, &expected, &p)?;
    }
    Ok(())
}

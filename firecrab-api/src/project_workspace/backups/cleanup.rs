//! Recoverable, fenced deletion of only explicitly journaled backup artifacts.
mod api;
mod files;
mod selection;
#[cfg(test)]
mod tests;
use super::Record as Backup;
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Record {
    pub public: ProjectBackupCleanupOperation,
    pub binding: Binding,
    pub active: bool,
    caller: Caller,
    files: Vec<files::Proof>,
    survivor: Option<Uuid>,
}
impl Record {
    fn response(&self) -> ProjectBackupCleanupOperation {
        let mut public = self.public.clone();
        public.pending_reconciliation = self.active;
        public
    }
}
pub(super) fn routes() -> Router<AppState> {
    api::routes()
}
fn retained(state: &AppState, project: Uuid, id: Uuid) -> Result<Record, Error> {
    state
        .store
        .cleanup_record(id)?
        .filter(|r| r.public.project_id == project)
        .ok_or(Error::Missing)
}
async fn execute(
    state: AppState,
    project: Uuid,
    id: Uuid,
    mut request: ProjectBackupCleanupRequest,
    caller: Caller,
    policy: Option<ProjectBackupRetention>,
) -> Result<ProjectBackupCleanupOperation, Error> {
    request.target_ids.sort();
    if id.is_nil()
        || request.expected_generation == 0
        || request.target_ids.is_empty()
        || request.target_ids.len() > 64
        || request.target_ids.iter().any(Uuid::is_nil)
        || request.target_ids.windows(2).any(|ids| ids[0] == ids[1])
    {
        return Err(Error::Invalid(
            "Select one through 64 distinct retained operation IDs",
        ));
    }
    let capacity = state
        .workspaces
        .backup_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| Error::Busy)?;
    let admission = state.sandbox.admission.clone().lock_owned().await;
    let binding = raw_binding(&state, project)?;
    let vm = state.lock_vm(binding.vm_id).await;
    tokio::task::spawn_blocking(move || {
        let (_capacity, _admission, _vm) = (capacity, admission, vm);
        execute_locked(&state, project, id, request, caller, policy)
    })
    .await
    .map_err(|_| Error::Conflict("Cleanup worker interrupted; retry the retained operation"))?
}
fn execute_locked(
    state: &AppState,
    project: Uuid,
    id: Uuid,
    request: ProjectBackupCleanupRequest,
    caller: Caller,
    policy: Option<ProjectBackupRetention>,
) -> Result<ProjectBackupCleanupOperation, Error> {
    let directory = root(&state.workspaces, project, true)?;
    let _operation =
        db::OwnerLock::acquire(db::private_file(&directory.join("operation.lock"), true)?)?;
    let existing = state.store.cleanup_record(id)?;
    if let Some(r) = &existing {
        if r.public.project_id != project || r.public.request != request {
            return Err(Error::Conflict(
                "Cleanup ID already has another immutable request",
            ));
        }
        if r.public.phase == "complete" {
            let mut r = r.clone();
            let fence = directory.join("fence.json");
            if fence_id(&fence)? == Some(id) {
                clear_fence(&fence, id)?;
            }
            r.active = false;
            r.public.error = None;
            state.store.save_cleanup_record(&r)?;
            return Ok(r.response());
        }
    }
    let b = raw_binding(state, project)?;
    if b.generation != request.expected_generation
        || existing.as_ref().is_some_and(|r| r.binding != b)
    {
        return Err(Error::Conflict(
            "Workspace binding changed; retain the cleanup identity",
        ));
    }
    caller.operator().map_err(|_| Error::Denied)?;
    caller
        .project(state, project, SandboxPermission::Write)
        .map_err(|_| Error::Denied)?;
    offline(state, &b, id, true)?;
    if existing.is_none()
        && (fenced(&state.workspaces, project)?
            || state.store.workspace_backup_active(project)?
            || state.store.workspace_cleanup_active(project)?)
    {
        return Err(Error::Busy);
    }
    if let Some(expected) = &policy
        && state.store.backup_retention(project)? != *expected
    {
        return Err(Error::Busy);
    }
    db::private_dir(&state.workspaces.root)?;
    db::private_dir(&state.workspaces.root.join(project.to_string()))?;
    let lease = state.workspaces.service_lease(&b, false)?;
    if std::sync::Arc::strong_count(&lease) > 2 {
        return Err(Error::Busy);
    }
    let live = db::project_directory(&state.workspaces.root, &b, false)?;
    let _owner = db::OwnerLock::acquire(db::private_file(&live.join("owner.lock"), true)?)?;
    let mut r = if let Some(r) = existing {
        if r.public.phase != "deleting" || r.public.removed_files as usize > r.files.len() {
            return Err(Error::Conflict("Invalid retained cleanup progress"));
        }
        r
    } else {
        prepare_record(state, b, id, request, caller, policy)?
    };
    let result = (|| {
        install_fence(&directory.join("fence.json"), id)?;
        if let Some(survivor) = r.survivor {
            let receipt = state.store.backup_record(survivor)?.ok_or(Error::Missing)?;
            if receipt.public.cleanup_id.is_some() || receipt.active {
                return Err(Error::Conflict("Retained survivor is no longer available"));
            }
            archive::completed(state, &directory, &r.binding, survivor)?;
        }
        files::verify(state, &r)?;
        while files::remove_one(state, &mut r)? {}
        files::remove_empty_archives(state, &r)?;
        state.store.complete_backup_cleanup(&mut r)?;
        clear_fence(&directory.join("fence.json"), id)?;
        r.active = false;
        r.public.error = None;
        state.store.save_cleanup_record(&r)?;
        Ok(r.response())
    })();
    if let Err(ref error) = result {
        tracing::warn!(%project,operation=%id,%error,"backup cleanup requires exact retry");
        r.public.error=Some("Cleanup could not be confirmed. Keep all remaining artifacts and retry this operation.".into());
        r.public.updated_at = ai_sandbox::now();
        state.store.save_cleanup_record(&r)?;
    }
    result
}
pub(super) async fn tick(state: AppState) -> Result<(), Error> {
    let local = state.clone();
    let records = tokio::task::spawn_blocking(move || local.store.cleanup_records(None))
        .await
        .map_err(|_| Error::Busy)??;
    for r in records {
        if r.public.error.is_none() {
            let _ = execute(
                state.clone(),
                r.public.project_id,
                r.public.id,
                r.public.request,
                r.caller,
                None,
            )
            .await;
        }
    }
    for project in state.store.retention_projects()? {
        if fenced(&state.workspaces, project)?
            || state.store.workspace_backup_active(project)?
            || state.store.workspace_cleanup_active(project)?
        {
            continue;
        }
        let expected = state.store.backup_retention(project)?;
        if !expected.enabled || expected.next_run_at > ai_sandbox::now() {
            continue;
        }
        let mut next = expected.clone();
        let local = state.clone();
        let p = expected.clone();
        let targets = tokio::task::spawn_blocking(move || selection::preview(&local, project, &p))
            .await
            .map_err(|_| Error::Busy)??
            .automatic_target_ids;
        next.next_run_at = ai_sandbox::now().saturating_add(300);
        if !targets.is_empty() {
            let b = raw_binding(&state, project)?;
            let input = ProjectBackupCleanupRequest {
                expected_generation: b.generation,
                target_ids: targets,
            };
            match execute(
                state.clone(),
                project,
                Uuid::new_v4(),
                input,
                Caller::default(),
                Some(expected.clone()),
            )
            .await
            {
                Ok(_) => {
                    next.error = None;
                    next.last_cleanup_at = Some(ai_sandbox::now());
                }
                Err(Error::Busy) => continue,
                Err(_) => {
                    next.error=Some("Cleanup deferred. Stop the workload and reconcile retained operations before retrying.".into());
                    next.next_run_at = ai_sandbox::now().saturating_add(60);
                }
            }
        } else {
            next.error = None;
        }
        state
            .store
            .finish_retention_attempt(project, &expected, &next)?;
    }
    Ok(())
}

fn prepare_record(
    state: &AppState,
    b: Binding,
    id: Uuid,
    request: ProjectBackupCleanupRequest,
    caller: Caller,
    policy: Option<ProjectBackupRetention>,
) -> Result<Record, Error> {
    let project = b.project_id;
    let targets = selection::targets(state, &b, &request.target_ids)?;
    let survivor = selection::survivor(state, &b, &targets)?;
    let files = files::plan(state, &b, &targets)?;
    if let Some(expected) = &policy
        && state.store.backup_retention(project)? != *expected
    {
        return Err(Error::Busy);
    }
    let r = Record {
        public: ProjectBackupCleanupOperation {
            id,
            project_id: project,
            request,
            phase: "deleting".into(),
            pending_reconciliation: true,
            automatic: policy.is_some(),
            created_at: ai_sandbox::now(),
            updated_at: ai_sandbox::now(),
            logical_bytes: 0,
            removed_files: 0,
            error: None,
        },
        binding: b,
        active: true,
        caller,
        files,
        survivor,
    };
    state.store.admit_backup_cleanup(&r)?;
    Ok(r)
}

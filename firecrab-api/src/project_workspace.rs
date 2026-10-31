//! Durable project metadata and retained-volume lifecycle. Sandbox keeps task ownership.
mod checkpoint;
mod db;
mod files;
mod files_db;
mod files_guest;
mod terminals;
mod terminals_db;
mod terminals_guest;
#[cfg(test)]
mod terminals_tests;
#[cfg(test)]
mod tests;
mod views;
mod views_db;
#[cfg(test)]
mod views_tests;

use crate::{
    ai_sandbox::{self, access::Caller, guest},
    block_storage,
    error::AppError,
    extract::ValidatedJson,
    handlers,
    persistence::PersistenceError,
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use firecrab_api_types::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
};
use uuid::Uuid;

/// Host-local project database root and current controller owner identity.
#[derive(Debug)]
pub struct Runtime {
    /// Private durable storage outside source paths exposed to project tools.
    pub root: PathBuf,
    /// New identity on each controller start, persisted with operation epochs.
    pub owner: Uuid,
    /// Bounded guest-file workers survive client cancellation without blocking the async executor.
    pub(crate) file_slots: std::sync::Arc<tokio::sync::Semaphore>,
    /// Projects with active or incompletely replayed terminal sessions.
    terminal_projects: std::sync::Mutex<std::collections::BTreeSet<Uuid>>,
    /// Wake replay workers promptly after interactive input.
    terminal_wake: tokio::sync::Notify,
    /// One OS lock per project remains held for this service lifetime.
    services: std::sync::Mutex<std::collections::BTreeMap<Uuid, std::sync::Arc<db::OwnerLock>>>,
}
impl Runtime {
    /// Establish controller ownership without creating project databases eagerly.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            owner: Uuid::new_v4(),
            file_slots: std::sync::Arc::new(tokio::sync::Semaphore::new(16)),
            terminal_projects: std::sync::Mutex::default(),
            terminal_wake: tokio::sync::Notify::new(),
            services: std::sync::Mutex::default(),
        }
    }
}

/// Common control registry reservation, independent of any browser or VM boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Binding {
    /// Existing Sandbox project owner.
    pub project_id: Uuid,
    /// Stable private SQLite identity.
    pub database_id: Uuid,
    /// Retained MicroBlockStorage identity.
    pub volume_id: Uuid,
    /// Explicitly selected workload VM.
    pub vm_id: Uuid,
    /// Monotonic runtime binding generation.
    pub generation: u64,
    /// Initial provisioning has not yet committed all resources.
    pub provisioning: bool,
    /// Runtime boot that most recently supplied an accepted mount proof.
    pub verified_runtime_id: Option<Uuid>,
}
/// Sanitized lifecycle failures; filesystem details remain in server logs.
#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("workspace not found")]
    Missing,
    #[error("workspace access denied")]
    Denied,
    #[error("workspace is busy")]
    Busy,
    #[error("{0}")]
    Conflict(&'static str),
    #[error("{0}")]
    Invalid(&'static str),
    #[error("workspace filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("workspace database operation failed: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("workspace registry operation failed: {0}")]
    Store(#[from] PersistenceError),
    #[error("workspace record is invalid: {0}")]
    Json(#[from] serde_json::Error),
}
impl Error {
    /// Translate bounded conflicts and log private implementation failures.
    /// Observe storage and boot identity without assuming an earlier proof is current.
    fn response(self, id: Uuid) -> AppError {
        match self {
            Self::Missing => AppError::not_found(id),
            Self::Denied => denied(id),
            Self::Busy => AppError::conflict(
                "workspace_busy",
                "Another owner is operating on this project; retry later",
                id,
            ),
            Self::Conflict(message) => AppError::conflict("workspace_conflict", message, id),
            Self::Invalid(message) => AppError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "invalid_workspace",
                message,
                id,
            ),
            Self::Sql(rusqlite::Error::SqliteFailure(ref error, _))
                if matches!(
                    error.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) =>
            {
                Self::Busy.response(id)
            }
            Self::Store(PersistenceError::BlockStorageConflict { message }) => {
                AppError::conflict("workspace_conflict", message, id)
            }
            other => {
                tracing::error!(error=%other,request_id=%id,"project workspace operation failed");
                AppError::internal(id)
            }
        }
    }
}
/// Run filesystem and SQLite work outside the asynchronous executor.
async fn blocking<T: Send + 'static>(
    id: Uuid,
    work: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, AppError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| AppError::internal(id))?
        .map_err(|e| e.response(id))
}
/// Avoid disclosing project or runtime existence on authorization failure.
fn denied(id: Uuid) -> AppError {
    AppError::new(
        axum::http::StatusCode::FORBIDDEN,
        "workspace_permission_denied",
        "Project or runtime access denied",
        id,
    )
}
/// Check the existing project grant before looking up private workspace data.
fn authorize(
    state: &AppState,
    caller: Caller,
    project: Uuid,
    permission: SandboxPermission,
    id: Uuid,
) -> Result<SandboxProject, AppError> {
    caller
        .project(state, project, permission)
        .map_err(|_| denied(id))?;
    state
        .store
        .sandbox_project(project)
        .map_err(|_| AppError::internal(id))?
        .ok_or_else(|| AppError::not_found(id))
}
/// Find only an existing project reservation; reads never provision resources.
fn binding(state: &AppState, project: Uuid) -> Result<Binding, Error> {
    state
        .store
        .project_workspace(project)?
        .ok_or(Error::Missing)
}
/// Open retained state without permitting missing-data replacement.
fn service_lease(
    state: &AppState,
    binding: &Binding,
    create: bool,
) -> Result<std::sync::Arc<db::OwnerLock>, Error> {
    let mut services = state
        .workspaces
        .services
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if let Some(lease) = services.get(&binding.project_id) {
        return Ok(lease.clone());
    }
    let directory = db::project_directory(&state.workspaces.root, binding, create)?;
    let file = db::private_file(&directory.join("service.lock"), true)?;
    let lease = std::sync::Arc::new(db::OwnerLock::acquire(file)?);
    services.insert(binding.project_id, lease.clone());
    Ok(lease)
}
fn open_owned(state: &AppState, binding: &Binding, create: bool) -> Result<db::ProjectDb, Error> {
    let lease = service_lease(state, binding, create)?;
    let mut db = db::ProjectDb::open(
        &state.workspaces.root,
        binding,
        state.workspaces.owner,
        create,
    )?;
    db.service = Some(lease);
    Ok(db)
}
fn open(state: &AppState, binding: &Binding) -> Result<db::ProjectDb, Error> {
    open_owned(state, binding, false)
}

fn response(
    state: &AppState,
    binding: &Binding,
    db: &db::ProjectDb,
) -> Result<ProjectWorkspaceResponse, Error> {
    let (revision, document, initialized) = db.document()?;
    let volume = state.store.block_storage(binding.volume_id)?;
    let available = block_storage::verify_disk(state, &volume).is_ok()
        && volume.attached_vm_id == Some(binding.vm_id)
        && volume.mount_path.as_deref() == Some("/workspace/shared");
    let vm = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&binding.vm_id)
        .cloned();
    let status = if binding.provisioning {
        ProjectWorkspaceStatus::Provisioning
    } else if !available {
        ProjectWorkspaceStatus::Unavailable
    } else {
        match vm {
            None => ProjectWorkspaceStatus::Unavailable,
            Some(vm) if matches!(vm.state, VmState::Created | VmState::Stopped) => {
                ProjectWorkspaceStatus::Stopped
            }
            Some(vm)
                if vm.state == VmState::Running
                    && initialized
                    && vm.last_runtime_id.is_some()
                    && vm.last_runtime_id == binding.verified_runtime_id =>
            {
                ProjectWorkspaceStatus::Ready
            }
            Some(_) => ProjectWorkspaceStatus::AwaitingMount,
        }
    };
    Ok(ProjectWorkspaceResponse {
        project_id: binding.project_id,
        database_id: binding.database_id,
        volume_id: binding.volume_id,
        vm_id: binding.vm_id,
        generation: binding.generation,
        schema_version: db::SCHEMA_VERSION,
        revision,
        document,
        initialized,
        status,
        verified_runtime_id: binding.verified_runtime_id,
    })
}
/// Extend the existing authenticated Sandbox project API.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/ai-sandbox/projects/{project}/workspace",
            get(get_workspace).put(bind_workspace),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/state",
            axum::routing::put(save_state),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/reconcile",
            post(reconcile),
        )
        .route(
            "/api/ai-sandbox/projects/{project}/workspace/checkpoints/{checkpoint}",
            get(checkpoint::get).put(checkpoint::create),
        )
        .merge(files::routes())
        .merge(terminals::routes())
        .merge(views::routes())
}
/// Read project metadata even when the workload VM is stopped.
async fn get_workspace(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
) -> Result<Json<ProjectWorkspaceResponse>, AppError> {
    authorize(&state, caller, project, SandboxPermission::Read, id.0)?;
    blocking(id.0, move || {
        let b = binding(&state, project)?;
        let db = open(&state, &b)?;
        response(&state, &b, &db)
    })
    .await
    .map(Json)
}
/// Apply a project-authorized optimistic metadata update.
async fn save_state(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
    ValidatedJson(input): ValidatedJson<UpdateProjectWorkspace>,
) -> Result<Json<ProjectWorkspaceResponse>, AppError> {
    authorize(&state, caller, project, SandboxPermission::Write, id.0)?;
    blocking(id.0, move || {
        let b = binding(&state, project)?;
        if b.provisioning {
            return Err(Error::Conflict(
                "Complete workspace provisioning before editing metadata",
            ));
        }
        let db = open(&state, &b)?;
        db.save_document(input.expected_revision, &input.document)?;
        response(&state, &b, &db)
    })
    .await
    .map(Json)
}
/// Reserve and attach retained resources while both runtimes are inactive.
async fn bind_workspace(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
    ValidatedJson(input): ValidatedJson<BindProjectWorkspace>,
) -> Result<Json<ProjectWorkspaceResponse>, AppError> {
    caller.operator().map_err(|_| denied(id.0))?;
    let admission = state.sandbox.admission.clone().lock_owned().await;
    let project_record = authorize(&state, caller, project, SandboxPermission::Write, id.0)?;
    let vm = project_record.spec.vm_id.ok_or_else(|| {
        Error::Conflict(
            "Select an existing borrowed VM in project settings before binding a workspace",
        )
        .response(id.0)
    })?;
    caller
        .require(
            &state,
            &SandboxResource::Vm { id: vm },
            SandboxPermission::Execute,
        )
        .map_err(|_| denied(id.0))?;
    if state
        .store
        .sandbox_project_busy(project)
        .map_err(|_| AppError::internal(id.0))?
    {
        return Err(Error::Conflict(
            "Finish or reconcile active project tasks before binding a workspace",
        )
        .response(id.0));
    }
    let volume = state
        .store
        .block_storage(input.volume_id)
        .map_err(|_| AppError::not_found(id.0))?;
    if !matches!(volume.state.as_str(), "available" | "attached") {
        return Err(
            Error::Conflict("Finish volume creation or recovery before binding it").response(id.0),
        );
    }
    if volume.attached_vm_id.is_some()
        && volume.mount_path.as_deref() != Some("/workspace/shared")
        && state
            .store
            .project_workspace(project)
            .map_err(|_| AppError::internal(id.0))?
            .is_none()
    {
        return Err(Error::Conflict(
            "Detach the selected volume before binding it at /workspace/shared",
        )
        .response(id.0));
    }
    let mut vm_ids = vec![vm];
    if let Some(old) = volume.attached_vm_id {
        vm_ids.push(old);
    }
    vm_ids.sort();
    vm_ids.dedup();
    let mut guards = Vec::new();
    for key in vm_ids {
        guards.push(state.lock_vm(key).await);
    }
    for key in [Some(vm), volume.attached_vm_id].into_iter().flatten() {
        if state
            .vms
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(&key)
        {
            handlers::block_storage::require_inactive_user_vm(&state, key, id.0)?;
        } else if key == vm {
            return Err(AppError::not_found(id.0));
        }
    }
    blocking(id.0, move || {
        let _admission = admission;
        let _guards = guards;
        block_storage::verify_disk(&state, &volume).map_err(|_| {
            Error::Conflict("Retained volume is missing or mismatched; repair it before binding")
        })?;
        if let Some(owner) = state.store.workspace_volume_owner(volume.id)?
            && owner.project_id != project
        {
            return Err(Error::Conflict("Volume belongs to another project"));
        }
        if volume.attached_vm_id.is_some_and(|old| old != vm)
            && state.store.project_workspace(project)?.is_none()
        {
            return Err(Error::Conflict(
                "Detach the selected volume from its current owner before first binding",
            ));
        }
        if let Some(existing) = state.store.project_workspace(project)? {
            service_lease(&state, &existing, existing.provisioning)?;
        }
        let b = state.store.reserve_project_workspace(
            project,
            volume.id,
            vm,
            input.expected_generation,
        )?;
        let db = open_owned(&state, &b, true)?;
        if let Some(old) = volume
            .attached_vm_id
            .filter(|old| *old != vm || volume.mount_path.as_deref() != Some("/workspace/shared"))
        {
            state.store.detach_workspace_volume(&b, old)?;
        }
        state.store.attach_block_storages(
            vm,
            &[VmBlockStorageMount {
                volume_id: b.volume_id,
                mount_path: "/workspace/shared".into(),
            }],
        )?;
        state.store.finish_project_workspace(&b)?;
        let b = binding(&state, project)?;
        response(&state, &b, &db)
    })
    .await
    .map(Json)
}

/// Persist one bounded sanitized import before any guest mutation.
fn prepare_import(
    state: &AppState,
    project: &SandboxProject,
    db: &db::ProjectDb,
) -> Result<(String, Vec<u8>), Error> {
    let archive = db.directory.join("import.tar");
    if let Some(expected) = db.import_sha()? {
        let data = fs::read(&archive)?;
        if format!("{:x}", Sha256::digest(&data)) != expected {
            return Err(Error::Conflict(
                "Retained import archive is missing or mismatched",
            ));
        }
        return Ok((expected, data));
    }
    let temporary = db.directory.join(format!("import-{}", Uuid::new_v4()));
    let result = (|| {
        let work = temporary.join("work");
        if let Some(source) = &project.spec.source_path {
            let mut secrets = Vec::new();
            for account in state.store.ai_connections()? {
                secrets.push(account.api_key);
                if let Some(session) = account.oauth {
                    secrets.push(session.refresh_token);
                }
            }
            ai_sandbox::workspace::prepare_from_commit(
                std::path::Path::new(source),
                &temporary,
                Uuid::new_v4(),
                Some(&state.sandbox.database),
                &secrets,
                None,
            )?;
        } else {
            fs::create_dir(&temporary)?;
            fs::create_dir(&work)?;
        }
        let data = ai_sandbox::workspace::archive(&work)?;
        for entry in tar::Archive::new(data.as_slice()).entries()? {
            let entry = entry?;
            let path = entry.path()?;
            if path.starts_with(".firecrab-workspace") || path.starts_with("lost+found") {
                return Err(Error::Invalid(
                    "Source contains reserved workspace bootstrap paths",
                ));
            }
        }
        let digest = format!("{:x}", Sha256::digest(&data));
        let staged = db.directory.join("import.tar.partial");
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(&staged)?;
        file.write_all(&data)?;
        file.sync_all()?;
        fs::rename(&staged, &archive)?;
        File::open(&db.directory)?.sync_all()?;
        db.set_import_sha(&digest)?;
        Ok((digest, data))
    })();
    let _ = fs::remove_dir_all(&temporary);
    result
}
/// Fill a fixed bootstrap protocol using generated identities and encoded bytes.
fn volume_script(b: &Binding, digest: &str, archive: &[u8], initialized: bool) -> Vec<u8> {
    include_str!("project_workspace/volume.sh")
        .replace("__PROJECT__", &b.project_id.to_string())
        .replace("__DATABASE__", &b.database_id.to_string())
        .replace("__VOLUME__", &b.volume_id.to_string())
        .replace("__DIGEST__", digest)
        .replace("__INITIALIZE__", if initialized { "0" } else { "1" })
        .replace("__ARCHIVE__", &STANDARD.encode(archive))
        .into_bytes()
}
/// Authorize and finish mount verification independently of HTTP cancellation.
async fn reconcile(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(project): Path<Uuid>,
) -> Result<Json<ProjectWorkspaceResponse>, AppError> {
    caller.operator().map_err(|_| denied(id.0))?;
    let record = authorize(&state, caller, project, SandboxPermission::Execute, id.0)?;
    caller
        .settings(&state, &record.spec)
        .map_err(|_| denied(id.0))?;
    let b = binding(&state, project).map_err(|e| e.response(id.0))?;
    if b.provisioning || record.spec.vm_id != Some(b.vm_id) {
        return Err(
            Error::Conflict("Complete the selected runtime binding before reconciling")
                .response(id.0),
        );
    }
    // Dropping a request must not release lifecycle locks while guest writes are still running.
    tokio::spawn(reconcile_runtime(state, record, b, id.0))
        .await
        .map_err(|_| AppError::internal(id.0))?
        .map(Json)
}

/// Hold runtime and database ownership until the bounded guest protocol finishes.
async fn reconcile_runtime(
    state: AppState,
    record: SandboxProject,
    b: Binding,
    request: Uuid,
) -> Result<ProjectWorkspaceResponse, AppError> {
    let _vm = state.lock_vm(b.vm_id).await;
    if binding(&state, b.project_id).map_err(|e| e.response(request))? != b {
        return Err(Error::Conflict("Workspace binding changed").response(request));
    }
    let runtime = guest::binding(&state, b.vm_id).map_err(|_| {
        Error::Conflict("Start the selected workspace VM before verifying its mount")
            .response(request)
    })?;
    let local = state.clone();
    let saved = b.clone();
    let (db, script) = blocking(request, move || {
        let db = open(&local, &saved)?;
        let current = response(&local, &saved, &db)?;
        if current.status == ProjectWorkspaceStatus::Unavailable {
            return Err(Error::Conflict(
                "Retained volume is missing, mismatched, or detached",
            ));
        }
        let (digest, archive) = if current.initialized {
            (
                db.import_sha()?
                    .ok_or(Error::Conflict("Missing import identity"))?,
                Vec::new(),
            )
        } else {
            prepare_import(&local, &record, &db)?
        };
        let script = volume_script(&saved, &digest, &archive, current.initialized);
        Ok((db, script))
    })
    .await?;
    let output = guest::prepare_project_volume(&state, &runtime, &db.directory, script)
        .await
        .map_err(|_| Error::Conflict(
            "Guest workspace mount or initialization could not be confirmed; retain the volume and retry reconciliation",
        ).response(request))?;
    let expected = format!(
        "firecrab_project_volume_v1\n{}\n{}\n",
        b.project_id, b.volume_id
    );
    if output != expected.as_bytes() || guest::binding(&state, b.vm_id).ok() != Some(runtime) {
        return Err(Error::Conflict(
            "Workspace runtime or mount proof changed during initialization",
        )
        .response(request));
    }
    blocking(request, move || {
        db.mark_initialized()?;
        state
            .store
            .verify_project_workspace(&b, runtime.runtime_id)?;
        response(&state, &binding(&state, b.project_id)?, &db)
    })
    .await
}

/// Recover terminal observers without starting replacement shells.
pub(crate) fn start_terminals(state: AppState) {
    terminals::start(state);
}

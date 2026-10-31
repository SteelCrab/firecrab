//! AI Sandbox REST lifecycle. Credentials and host process execution are never client inputs.
use crate::{
    ai_sandbox::{self, ModelProof, SavedTask, access::Caller},
    error::AppError,
    extract::ValidatedJson,
    pipe_ai::{self, Failure, SavedConnection},
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use firecrab_api_types::*;
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::{Path as FilePath, PathBuf},
};
use uuid::Uuid;

fn error(id: RequestId, code: &'static str, message: &'static str) -> AppError {
    AppError::new(StatusCode::CONFLICT, code, message, id.0)
}
fn provider(id: RequestId, failure: Failure) -> AppError {
    let (status, code, message) = failure.details();
    AppError::new(status, code, message, id.0)
}
fn invalid(id: RequestId, field: &str, message: impl ToString) -> AppError {
    AppError::validation(
        BTreeMap::from([(field.to_owned(), message.to_string())]),
        id.0,
    )
}
fn denied(id: RequestId, _message: String) -> AppError {
    AppError::new(
        StatusCode::FORBIDDEN,
        "sandbox_permission_denied",
        "Sandbox project or resource permission denied",
        id.0,
    )
}

/// Only the controller operator may inspect or replace delegated host policies.
pub async fn get_access(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(principal): Path<Uuid>,
) -> Result<Json<SandboxAccessPolicy>, AppError> {
    caller.operator().map_err(|e| denied(id, e))?;
    let store = state.store.clone();
    Ok(Json(db(id, move || store.sandbox_access(principal)).await?))
}

/// Replace exact resource grants. Empty grants revoke all delegated Sandbox access.
pub async fn set_access(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(principal): Path<Uuid>,
    ValidatedJson(mut policy): ValidatedJson<SandboxAccessPolicy>,
) -> Result<Json<SandboxAccessPolicy>, AppError> {
    caller.operator().map_err(|e| denied(id, e))?;
    let _claim = state.sandbox.admission.lock().await;
    if principal.is_nil() || policy.grants.len() > 256 {
        return Err(invalid(
            id,
            "grants",
            "Use a host UUID and at most 256 resource grants",
        ));
    }
    let mut resources = Vec::new();
    for grant in &mut policy.grants {
        grant.permissions.sort();
        grant.permissions.dedup();
        if grant.permissions.is_empty() {
            return Err(invalid(
                id,
                "permissions",
                "A resource grant needs at least one permission",
            ));
        }
        match &grant.resource {
            SandboxResource::Source { path } => {
                let canonical = std::fs::canonicalize(path).map_err(|_| {
                    invalid(id, "path", "Source must be an existing canonical directory")
                })?;
                if !canonical.is_dir() || canonical.to_str() != Some(path.as_str()) {
                    return Err(invalid(
                        id,
                        "path",
                        "Use the exact canonical source directory",
                    ));
                }
                if grant
                    .permissions
                    .iter()
                    .any(|p| !matches!(p, SandboxPermission::Read | SandboxPermission::Write))
                {
                    return Err(invalid(
                        id,
                        "permissions",
                        "Sources support read/write grants",
                    ));
                }
            }
            SandboxResource::Project { id: key }
            | SandboxResource::Connection { id: key }
            | SandboxResource::Vm { id: key } => {
                if key.is_nil() {
                    return Err(invalid(id, "resource", "Resource UUID cannot be nil"));
                }
                if !matches!(grant.resource, SandboxResource::Project { .. })
                    && grant
                        .permissions
                        .iter()
                        .any(|p| !matches!(p, SandboxPermission::Read | SandboxPermission::Execute))
                {
                    return Err(invalid(
                        id,
                        "permissions",
                        "Connections and VMs support read/execute grants",
                    ));
                }
            }
        }
        if resources.contains(&grant.resource) {
            return Err(invalid(id, "grants", "Use one grant per exact resource"));
        }
        resources.push(grant.resource.clone());
    }
    let store = state.store.clone();
    let saved = policy.clone();
    db(id, move || store.save_sandbox_access(principal, &saved)).await?;
    Ok(Json(policy))
}

/// Revocation persists across controller restart and affects subsequent tool actions.
pub async fn revoke_access(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(principal): Path<Uuid>,
) -> Result<Json<SandboxAccessPolicy>, AppError> {
    caller.operator().map_err(|e| denied(id, e))?;
    let _claim = state.sandbox.admission.lock().await;
    let store = state.store.clone();
    db(id, move || {
        store.save_sandbox_access(principal, &SandboxAccessPolicy::default())
    })
    .await?;
    Ok(Json(SandboxAccessPolicy::default()))
}
async fn db<T: Send + 'static>(
    id: RequestId,
    work: impl FnOnce() -> Result<T, crate::persistence::PersistenceError> + Send + 'static,
) -> Result<T, AppError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| AppError::internal(id.0))?
        .map_err(|_| AppError::internal(id.0))
}
async fn project(
    state: &AppState,
    id: RequestId,
    key: Uuid,
    caller: Caller,
    permission: SandboxPermission,
) -> Result<SandboxProject, AppError> {
    caller
        .project(state, key, permission)
        .map_err(|e| denied(id, e))?;
    let store = state.store.clone();
    db(id, move || store.sandbox_project(key))
        .await?
        .ok_or_else(|| AppError::not_found(id.0))
}
async fn task(
    state: &AppState,
    id: RequestId,
    key: Uuid,
    caller: Caller,
    permission: SandboxPermission,
) -> Result<SavedTask, AppError> {
    let store = state.store.clone();
    let saved = db(id, move || store.sandbox_task(key))
        .await?
        .ok_or_else(|| AppError::not_found(id.0))?;
    caller
        .task(state, &saved, permission)
        .map_err(|e| denied(id, e))?;
    project(state, id, saved.task.project_id, caller, permission).await?;
    Ok(saved)
}
async fn account(state: &AppState, id: RequestId, key: Uuid) -> Result<SavedConnection, AppError> {
    let store = state.store.clone();
    let mut saved = db(id, move || store.ai_connection(key))
        .await?
        .ok_or_else(|| error(id, "ai_connection_missing", "AI account is disconnected"))?;
    let previous = saved.clone();
    if saved.connection.provider == AiProvider::Openai
        && state
            .pipe_ai
            .refresh(&mut saved)
            .await
            .map_err(|e| provider(id, e))?
    {
        let store = state.store.clone();
        let copy = saved.clone();
        db(id, move || store.refresh_ai_connection(&previous, &copy)).await?;
    }
    Ok(saved)
}

/// List persistent projects independently of provider availability.
pub async fn projects(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
) -> Result<Json<Vec<SandboxProject>>, AppError> {
    let store = state.store.clone();
    let projects = db(id, move || store.sandbox_projects()).await?;
    let mut visible = Vec::new();
    for project in projects {
        if caller
            .allowed(
                &state,
                &SandboxResource::Project { id: project.id },
                SandboxPermission::Read,
            )
            .map_err(|e| denied(id, e))?
        {
            visible.push(project);
        }
    }
    Ok(Json(visible))
}
/// Fetch one project's current settings.
pub async fn get_project(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<Uuid>,
) -> Result<Json<SandboxProject>, AppError> {
    Ok(Json(
        project(&state, id, key, caller, SandboxPermission::Read).await?,
    ))
}

/// Save explicit user authorization for a source directory, borrowed VM and model default.
pub async fn save_project(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<Uuid>,
    ValidatedJson(mut spec): ValidatedJson<SandboxProjectSpec>,
) -> Result<Json<SandboxProject>, AppError> {
    let _claim = state.sandbox.admission.lock().await;
    caller
        .project(&state, key, SandboxPermission::Write)
        .map_err(|e| denied(id, e))?;
    spec.name = spec.name.trim().to_owned();
    if spec.name.is_empty()
        || spec.name.len() > 200
        || spec.name.chars().any(char::is_control)
        || spec.instructions.len() > 16384
    {
        return Err(invalid(
            id,
            "name",
            "Use a name up to 200 bytes and project instructions up to 16 KiB",
        ));
    }
    let store = state.store.clone();
    if db(id, move || store.sandbox_project_busy(key)).await? {
        return Err(error(
            id,
            "sandbox_busy",
            "Finish or reconcile the current task before editing the project",
        ));
    }
    if let Some(path) = spec.source_path.as_ref().filter(|s| !s.trim().is_empty()) {
        caller
            .require(
                &state,
                &SandboxResource::Source { path: path.clone() },
                SandboxPermission::Read,
            )
            .map_err(|e| denied(id, e))?;
        let path = FilePath::new(path);
        if !path.is_absolute() {
            return Err(invalid(
                id,
                "sourcePath",
                "Use an absolute project directory on the API host",
            ));
        }
        let canonical = std::fs::canonicalize(path).map_err(|_| {
            invalid(
                id,
                "sourcePath",
                "Project directory is unavailable on the API host",
            )
        })?;
        if !canonical.is_dir()
            || canonical.parent().is_none()
            || ["/etc", "/proc", "/sys", "/dev", "/run", "/var/lib/firecrab"]
                .iter()
                .any(|p| canonical.starts_with(p))
            || canonical.starts_with(&state.sandbox.root)
            || state.sandbox.root.starts_with(&canonical)
        {
            return Err(invalid(
                id,
                "sourcePath",
                "Select a project directory, not a system or Sandbox data directory",
            ));
        }
        spec.source_path = Some(canonical.to_string_lossy().into_owned());
    } else {
        spec.source_path = None;
    }
    ai_sandbox::context::validate(&spec).map_err(|e| invalid(id, "context", e))?;
    if let Some(environment_id) = spec.environment_id {
        if spec.vm_id.is_some() {
            return Err(invalid(
                id,
                "environmentId",
                "Select a managed Environment or a borrowed MicroVM",
            ));
        }
        let environment = state
            .store
            .sandbox_environment(environment_id)
            .map_err(|_| AppError::internal(id.0))?
            .filter(|environment| environment.project_id == key)
            .ok_or_else(|| {
                invalid(
                    id,
                    "environmentId",
                    "Environment must belong to this project",
                )
            })?;
        if matches!(
            environment.state,
            SandboxEnvironmentState::Deleted | SandboxEnvironmentState::CleanupPending
        ) {
            return Err(invalid(
                id,
                "environmentId",
                "Select an Environment that is not being deleted",
            ));
        }
    }
    caller.settings(&state, &spec).map_err(|e| denied(id, e))?;
    if let Some(selection) = &spec.selection {
        pipe_ai::validate_model(&selection.model).map_err(|e| provider(id, e))?;
        ai_sandbox::proof(&state, selection, false)
            .map_err(|message| invalid(id, "selection", message))?;
    }
    let store = state.store.clone();
    let existing = db(id, move || store.sandbox_projects()).await?;
    if spec.vm_id.is_some()
        && existing
            .iter()
            .any(|p| p.id != key && p.spec.vm_id == spec.vm_id)
    {
        return Err(invalid(
            id,
            "vmId",
            "This MicroVM is already bound to another Sandbox project",
        ));
    }
    if let Some(vm) = spec.vm_id {
        if state
            .store
            .sandbox_environment_owns_vm(vm)
            .map_err(|_| AppError::internal(id.0))?
        {
            return Err(invalid(
                id,
                "vmId",
                "Managed Environment VMs must use their owning Environment binding",
            ));
        }
        if state
            .store
            .cluster_worker(vm)
            .map_err(|_| AppError::internal(id.0))?
            .is_some()
        {
            return Err(invalid(
                id,
                "vmId",
                "Choose a user MicroVM without a controller placement owner",
            ));
        }
        let vm = state
            .vms
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&vm)
            .cloned()
            .ok_or_else(|| invalid(id, "vmId", "MicroVM does not exist"))?;
        if vm.purpose != crate::model::VmPurpose::Instance
            || state
                .store
                .vm_has_service_owner(vm.id)
                .map_err(|_| AppError::internal(id.0))?
        {
            return Err(invalid(
                id,
                "vmId",
                "Choose a user MicroVM without a MicroService owner",
            ));
        }
    }
    let accounts = state
        .store
        .ai_connections()
        .map_err(|_| AppError::internal(id.0))?;
    if ai_sandbox::redact(
        &accounts,
        &serde_json::to_string(&spec).map_err(|_| AppError::internal(id.0))?,
    ) != serde_json::to_string(&spec).map_err(|_| AppError::internal(id.0))?
    {
        return Err(invalid(
            id,
            "instructions",
            "Project settings must not contain provider credentials",
        ));
    }
    let value = SandboxProject {
        id: key,
        owner: existing
            .iter()
            .find(|p| p.id == key)
            .map_or(caller.principal, |p| p.owner),
        created_at: existing
            .iter()
            .find(|p| p.id == key)
            .map_or_else(ai_sandbox::now, |p| p.created_at),
        spec,
    };
    let copy = value.clone();
    let store = state.store.clone();
    db(id, move || store.save_sandbox_project(&copy)).await?;
    Ok(Json(value))
}

/// Delete only this project's retained local workspaces; borrowed MicroVMs and source files survive.
pub async fn delete_project(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let _claim = state.sandbox.admission.lock().await;
    project(&state, id, key, caller, SandboxPermission::Write).await?;
    let store = state.store.clone();
    if db(id, move || store.sandbox_project_busy(key)).await? {
        return Err(error(
            id,
            "sandbox_busy",
            "Cancel active work and reconcile guest resources before deleting this project",
        ));
    }
    if state
        .store
        .sandbox_environments(key)
        .map_err(|_| AppError::internal(id.0))?
        .iter()
        .any(|environment| environment.state != SandboxEnvironmentState::Deleted)
    {
        return Err(error(
            id,
            "sandbox_environment_retained",
            "Delete owned Environments explicitly before deleting their project",
        ));
    }
    if state
        .store
        .sandbox_project_has_live_tunnels(key)
        .map_err(|_| AppError::internal(id.0))?
    {
        return Err(error(
            id,
            "sandbox_tunnel_retained",
            "Stop and delete active preview bindings before deleting their project",
        ));
    }
    let owned = state.sandbox.root.join(key.to_string());
    if owned.exists() {
        tokio::task::spawn_blocking(move || std::fs::remove_dir_all(owned))
            .await
            .map_err(|_| AppError::internal(id.0))?
            .map_err(|_| AppError::internal(id.0))?;
    }
    let store = state.store.clone();
    db(id, move || store.delete_sandbox_project(key)).await?;
    Ok(Json(json!({"deleted":true})))
}

/// Discover available models without treating catalogue membership as inference proof.
#[derive(serde::Deserialize, Default)]
pub struct Discovery {
    /// Explicit refresh; ordinary reads return existing proofs without provider requests.
    #[serde(default)]
    pub discover: bool,
}

/// Read proofs or explicitly discover the configured provider catalogue.
pub async fn models(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<Uuid>,
    Query(query): Query<Discovery>,
) -> Result<Json<SandboxModelCatalog>, AppError> {
    let mut rights = vec![(
        SandboxResource::Connection { id: key },
        SandboxPermission::Read,
    )];
    if query.discover {
        rights.push((
            SandboxResource::Connection { id: key },
            SandboxPermission::Execute,
        ));
    }
    caller
        .require_all(&state, &rights)
        .map_err(|e| denied(id, e))?;
    let _guard = if query.discover {
        Some(
            state
                .pipe_ai_mutations
                .try_lock()
                .map_err(|_| error(id, "ai_busy", "Another AI operation is in progress"))?,
        )
    } else {
        None
    };
    let saved = if query.discover {
        account(&state, id, key).await?
    } else {
        let store = state.store.clone();
        db(id, move || store.ai_connection(key))
            .await?
            .ok_or_else(|| error(id, "ai_connection_missing", "AI account is disconnected"))?
    };
    let discovery_available = true;
    let catalogue = if discovery_available && query.discover {
        caller
            .require_all(&state, &rights)
            .map_err(|e| denied(id, e))?;
        ai_sandbox::model::catalogue(&state, &saved)
            .await
            .map_err(|e| provider(id, e))?
    } else {
        vec![]
    };
    let current = ai_sandbox::fingerprint(&saved);
    let store = state.store.clone();
    let proofs = db(id, move || store.sandbox_models(key))
        .await?
        .into_iter()
        .filter(|p| p.fingerprint == current)
        .collect::<Vec<_>>();
    let mut result: BTreeMap<String, SandboxModel> = catalogue
        .into_iter()
        .map(|(model, name)| {
            (
                model.clone(),
                SandboxModel {
                    id: model,
                    name,
                    verified: false,
                    tools_verified: false,
                },
            )
        })
        .collect();
    for proof in proofs {
        let entry = result
            .entry(proof.selection.model.clone())
            .or_insert_with(|| SandboxModel {
                id: proof.selection.model.clone(),
                name: proof.selection.model,
                verified: false,
                tools_verified: false,
            });
        entry.verified = proof.verified;
        entry.tools_verified = proof.tools_verified;
    }
    Ok(Json(SandboxModelCatalog {
        models: result.into_values().collect(),
        discovery_available,
    }))
}

/// Verify actual text and optionally a harmless native tool-result round trip.
pub async fn verify_model(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<Uuid>,
    ValidatedJson(input): ValidatedJson<SandboxVerifyModel>,
) -> Result<Json<SandboxModel>, AppError> {
    let resource = SandboxResource::Connection { id: key };
    caller
        .require(&state, &resource, SandboxPermission::Execute)
        .map_err(|e| denied(id, e))?;
    pipe_ai::validate_model(&input.model).map_err(|e| provider(id, e))?;
    let _guard = state
        .pipe_ai_mutations
        .try_lock()
        .map_err(|_| error(id, "ai_busy", "Another AI operation is in progress"))?;
    let saved = account(&state, id, key).await?;
    if saved.redact(&input.model) != input.model {
        return Err(provider(id, Failure::InvalidInput));
    }
    let mut history = vec![
        json!({"role":"user","content":if input.tools {"Call sandbox_ready once, then reply OK after its result."}else{"Reply with OK."}}),
    ];
    let tools = if input.tools {
        ai_sandbox::model::probe()
    } else {
        vec![]
    };
    caller
        .require(&state, &resource, SandboxPermission::Execute)
        .map_err(|e| denied(id, e))?;
    let turn = ai_sandbox::model::turn(&state, &saved, &input.model, &history, &tools, input.tools)
        .await
        .map_err(|e| provider(id, e))?;
    if input.tools {
        if turn.calls.len() != 1
            || turn.calls[0].name != "sandbox_ready"
            || turn.calls[0].arguments != json!({})
        {
            return Err(error(
                id,
                "sandbox_tools_unverified",
                "This model did not complete the native tool probe; verify conversation access separately",
            ));
        }
        history.extend(turn.continuation);
        ai_sandbox::model::append_results(
            &mut history,
            saved.connection.provider,
            &[(turn.calls[0].clone(), "OK".into(), false)],
        );
        caller
            .require(&state, &resource, SandboxPermission::Execute)
            .map_err(|e| denied(id, e))?;
        let final_turn =
            ai_sandbox::model::turn(&state, &saved, &input.model, &history, &tools, false)
                .await
                .map_err(|e| provider(id, e))?;
        if final_turn.text.trim().is_empty() || !final_turn.calls.is_empty() {
            return Err(error(
                id,
                "sandbox_tools_unverified",
                "The model did not return a complete response after the tool result",
            ));
        }
    } else if turn.text.trim().is_empty() || !turn.calls.is_empty() {
        return Err(provider(id, Failure::NoText));
    }
    caller
        .require(&state, &resource, SandboxPermission::Execute)
        .map_err(|e| denied(id, e))?;
    let old_proofs = state
        .store
        .sandbox_models(key)
        .map_err(|_| AppError::internal(id.0))?;
    let tools_verified = input.tools
        || old_proofs.iter().any(|p| {
            p.selection.model == input.model
                && p.fingerprint == ai_sandbox::fingerprint(&saved)
                && p.tools_verified
        });
    let proof = ModelProof {
        selection: AiModelSelection {
            connection_id: key,
            model: input.model.clone(),
        },
        fingerprint: ai_sandbox::fingerprint(&saved),
        verified: true,
        tools_verified,
    };
    let store = state.store.clone();
    db(id, move || store.save_sandbox_model(&proof)).await?;
    Ok(Json(SandboxModel {
        id: input.model.clone(),
        name: input.model,
        verified: true,
        tools_verified,
    }))
}

/// Cursor filters keep polling independent of retained file contents and old conversation pages.
#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TaskQuery {
    /// Return records updated at or after this checkpoint.
    pub since: Option<u64>,
    /// Read the previous page before this task identity.
    pub before: Option<Uuid>,
}
/// Conversation and Kanban consume these same task records.
pub async fn tasks(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<Uuid>,
    Query(query): Query<TaskQuery>,
) -> Result<Json<Vec<SandboxTaskSummary>>, AppError> {
    project(&state, id, key, caller, SandboxPermission::Read).await?;
    let sources = caller.sources(&state, key).map_err(|e| denied(id, e))?;
    let store = state.store.clone();
    let before = if let Some(cursor) = query.before {
        let store = state.store.clone();
        Some(
            db(id, move || store.sandbox_cursor(key, cursor))
                .await?
                .ok_or_else(|| invalid(id, "before", "Unknown conversation cursor"))?,
        )
    } else {
        None
    };
    Ok(Json(
        db(id, move || {
            store.sandbox_visible_summaries(key, query.since, before, sources)
        })
        .await?,
    ))
}
/// Poll a task's persisted progress and results.
pub async fn get_task(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<Uuid>,
) -> Result<Json<SandboxTask>, AppError> {
    Ok(Json(
        task(&state, id, key, caller, SandboxPermission::Read)
            .await?
            .task,
    ))
}

/// Persist admission before dispatch. Repeating an identical task id never starts another attempt.
pub async fn create_task(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path((project_id, key)): Path<(Uuid, Uuid)>,
    ValidatedJson(mut input): ValidatedJson<SandboxTaskInput>,
) -> Result<(StatusCode, Json<SandboxTask>), AppError> {
    let claim = state.sandbox.admission.clone().lock_owned().await;
    caller
        .project(&state, project_id, SandboxPermission::Read)
        .map_err(|e| denied(id, e))?;
    let project = project(&state, id, project_id, caller, SandboxPermission::Execute).await?;
    let store = state.store.clone();
    let previous = db(id, move || store.sandbox_task(key)).await?;
    let accounts = state
        .store
        .ai_connections()
        .map_err(|_| AppError::internal(id.0))?;
    input.input = ai_sandbox::redact(&accounts, input.input.trim());
    if let Some(previous) = previous {
        if previous.task.project_id != project_id {
            return Err(AppError::not_found(id.0));
        }
        caller
            .task(&state, &previous, SandboxPermission::Execute)
            .map_err(|e| denied(id, e))?;
        if previous.task.request != input {
            return Err(error(
                id,
                "sandbox_task_conflict",
                "This task id already belongs to another request",
            ));
        }
        return Ok((StatusCode::OK, Json(previous.task)));
    }
    ai_sandbox::coordination::validate(&input).map_err(|e| invalid(id, "coordination", e))?;
    let serialized = serde_json::to_string(&input).map_err(|_| AppError::internal(id.0))?;
    if ai_sandbox::redact(&accounts, &serialized) != serialized {
        return Err(invalid(
            id,
            "coordination",
            "Task settings must not contain provider credentials",
        ));
    }
    if let Some(prior) = input.retry_of {
        task(&state, id, prior, caller, SandboxPermission::Execute).await?;
    }
    let (retry_root, retry_number) =
        ai_sandbox::coordination::retry(&state.store, project_id, key, &input)
            .map_err(|e| invalid(id, "retryOf", e))?;
    ai_sandbox::coordination::dependencies(
        &state,
        caller,
        project_id,
        project.spec.source_path.as_deref(),
        &input,
    )
    .map_err(|e| invalid(id, "dependencies", e))?;
    let store = state.store.clone();
    let admission_limit = ai_sandbox::coordination::admission_limit(&input);
    if db(id, move || store.sandbox_pending_count()).await? >= admission_limit {
        return Err(error(
            id,
            "sandbox_queue_full",
            "Pending attempt limit reached; dependency waiters reserve one slot for an explicit retry",
        ));
    }
    if input.input.is_empty() || input.input.len() > 32768 {
        return Err(invalid(id, "input", "Enter a task or message up to 32 KiB"));
    }
    let store = state.store.clone();
    if db(id, move || store.sandbox_task_count(project_id)).await? >= 500 {
        return Err(error(
            id,
            "sandbox_task_limit",
            "This project reached its 500-task limit",
        ));
    }
    if !ai_sandbox::coordination::queued_allowed(&state.store, project_id, &input)
        .map_err(|e| invalid(id, "dependencies", e))?
    {
        return Err(error(
            id,
            "sandbox_busy",
            "Finish or reconcile the current task before starting another",
        ));
    }
    let selection = input
        .selection
        .clone()
        .or(project.spec.selection.clone())
        .ok_or_else(|| {
            invalid(
                id,
                "selection",
                "Choose a verified project default or task model",
            )
        })?;
    caller
        .require(
            &state,
            &SandboxResource::Connection {
                id: selection.connection_id,
            },
            SandboxPermission::Execute,
        )
        .map_err(|e| denied(id, e))?;
    ai_sandbox::proof(&state, &selection, input.kind == SandboxTaskKind::Code)
        .map_err(|e| invalid(id, "selection", e))?;
    let mut runtime = None;
    let mut environment_ref = None;
    let mut execution_mode = None;
    let mut captured_project = project.spec.clone();
    if input.environment_role.is_some()
        && (input.kind != SandboxTaskKind::Code || project.spec.environment_id.is_none())
    {
        return Err(invalid(
            id,
            "environmentRole",
            "An Environment role requires a code task bound to a managed Environment",
        ));
    }
    if input.kind == SandboxTaskKind::Code {
        caller
            .settings(&state, &project.spec)
            .map_err(|e| denied(id, e))?;
        if project.spec.source_path.is_none() {
            return Err(invalid(
                id,
                "sourcePath",
                "Select a project source directory for code tasks",
            ));
        }
        if let Some(environment_id) = project.spec.environment_id {
            let environment = state
                .store
                .sandbox_environment(environment_id)
                .map_err(|_| AppError::internal(id.0))?
                .filter(|environment| environment.project_id == project_id)
                .ok_or_else(|| {
                    invalid(
                        id,
                        "environmentId",
                        "Environment must belong to this project",
                    )
                })?;
            let (reference, binding, mode) = ai_sandbox::environment::binding(
                &state,
                environment.id,
                input.environment_role.as_deref(),
            )
            .map_err(|e| invalid(id, "environmentId", e))?;
            caller
                .require(
                    &state,
                    &SandboxResource::Vm { id: binding.vm_id },
                    SandboxPermission::Execute,
                )
                .map_err(|e| denied(id, e))?;
            captured_project.vm_id = Some(binding.vm_id);
            runtime = Some(binding);
            environment_ref = Some(reference);
            execution_mode = Some(mode);
        } else {
            let vm = project
                .spec
                .vm_id
                .ok_or_else(|| invalid(id, "vmId", "Select a MicroVM for code tasks"))?;
            runtime =
                Some(ai_sandbox::guest::binding(&state, vm).map_err(|e| invalid(id, "vmId", e))?);
        }
    }
    let timestamp = ai_sandbox::now();
    let spec = project.spec.clone();
    let git_commit = tokio::task::spawn_blocking(move || ai_sandbox::context::resolve(&spec))
        .await
        .map_err(|_| AppError::internal(id.0))?
        .map_err(|e| invalid(id, "gitRef", e))?;
    if matches!(
        execution_mode,
        Some(SandboxExecutionMode::PersistentWorkspace)
    ) && let Some(runtime) = runtime
        && let Some(origin) = state
            .store
            .sandbox_workspace_origin(runtime.vm_id)
            .map_err(|_| AppError::internal(id.0))?
        && (origin.project.source_path != captured_project.source_path
            || origin.task.git_commit != git_commit)
    {
        return Err(error(
            id,
            "sandbox_workspace_source_changed",
            "Retained Workspace source or Git base changed. Restore its original source/ref or explicitly select a new Environment.",
        ));
    }
    let saved = SavedTask {
        caller,
        task: SandboxTask {
            git_commit,
            retry_root: Some(retry_root),
            retry_number,
            dependency_results: vec![],
            tests: vec![],
            artifacts: vec![],
            environment: environment_ref,
            id: key,
            project_id,
            attempt_id: Uuid::new_v4(),
            runtime,
            request: input,
            selection,
            state: SandboxTaskState::Waiting,
            created_at: timestamp,
            updated_at: timestamp,
            result: String::new(),
            error_code: None,
            error: None,
            events: vec![],
            changes: vec![],
        },
        project: captured_project,
        workspace: None,
        guest: runtime,
        guest_pending: false,
        guest_command: None,
        guest_staged: false,
        results_exported: false,
        execution_mode,
    };
    caller
        .task(&state, &saved, SandboxPermission::Execute)
        .map_err(|e| denied(id, e))?;
    let state = state.clone();
    let response = tokio::spawn(async move { ai_sandbox::admit(state, saved, claim).await })
        .await
        .map_err(|_| AppError::internal(id.0))?
        .map_err(|_| AppError::internal(id.0))?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

/// Request cooperative cancellation. Terminal state is reported only after confirmed guest cleanup.
pub async fn cancel_task(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<Uuid>,
) -> Result<(StatusCode, Json<SandboxTask>), AppError> {
    let saved = task(&state, id, key, caller, SandboxPermission::Execute).await?;
    if let Some(cancel) = state
        .sandbox
        .cancellations
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&key)
    {
        let _ = cancel.send(true);
    }
    Ok((StatusCode::ACCEPTED, Json(saved.task)))
}
/// Explicitly reconcile interrupted work before retrying or deleting owned data.
pub async fn reconcile_task(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<Uuid>,
) -> Result<Json<SandboxTask>, AppError> {
    let _claim = state.sandbox.admission.lock().await;
    let mut saved = task(&state, id, key, caller, SandboxPermission::Execute).await?;
    if saved.task.state.active() {
        return Err(error(
            id,
            "sandbox_busy",
            "Cancel the active task before reconciliation",
        ));
    }
    if saved.task.state != SandboxTaskState::Interrupted {
        return Err(error(
            id,
            "sandbox_reconcile_unneeded",
            "Only an interrupted attempt needs reconciliation",
        ));
    }
    if let Some(binding) = &saved.guest {
        saved.guest = Some(
            ai_sandbox::guest::binding(&state, binding.vm_id)
                .map_err(|e| invalid(id, "guest", e))?,
        );
    }
    ai_sandbox::reconcile_guest(&state, &mut saved)
        .await
        .map_err(|e| invalid(id, "guest", e))?;
    if let Some(path) = &saved.workspace {
        saved.task.changes = match ai_sandbox::workspace::changes(path) {
            Ok(changes) => changes,
            // Preparation checkpoints precede every provider/tool action. A partial initial
            // snapshot has no produced outputs to recover and remains retained for inspection.
            Err(_)
                if saved.task.events.is_empty()
                    && saved.task.result.is_empty()
                    && saved.task.artifacts.is_empty() =>
            {
                Vec::new()
            }
            Err(error) => return Err(invalid(id, "workspace", error)),
        };
    }
    let connections = state
        .store
        .ai_connections()
        .map_err(|_| AppError::internal(id.0))?;
    for change in &mut saved.task.changes {
        change.before = change
            .before
            .take()
            .map(|v| ai_sandbox::redact(&connections, &v));
        change.after = change
            .after
            .take()
            .map(|v| ai_sandbox::redact(&connections, &v));
    }
    saved.task.state = SandboxTaskState::Cancelled;
    saved.task.updated_at = ai_sandbox::now();
    saved.task.error_code = None;
    saved.task.error = Some("Interrupted attempt reconciled; retry creates a new attempt".into());
    saved.results_exported = true;
    ai_sandbox::artifacts::verify_export(&saved).map_err(|e| invalid(id, "artifacts", e))?;
    let store = state.store.clone();
    let copy = saved.clone();
    db(id, move || store.save_sandbox_task(&copy)).await?;
    Ok(Json(saved.task))
}

/// Download only immutable exported bytes owned by the authorized task and attempt.
pub async fn get_artifact(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path((key, artifact)): Path<(Uuid, Uuid)>,
) -> Result<([(axum::http::header::HeaderName, &'static str); 2], Vec<u8>), AppError> {
    let saved = task(&state, id, key, caller, SandboxPermission::Read).await?;
    if !saved
        .task
        .artifacts
        .iter()
        .any(|record| record.id == artifact)
    {
        return Err(AppError::not_found(id.0));
    }
    let bytes = tokio::task::spawn_blocking(move || ai_sandbox::artifacts::read(&saved, artifact))
        .await
        .map_err(|_| AppError::internal(id.0))?
        .map_err(|e| invalid(id, "artifact", e))?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "application/octet-stream"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=artifact.bin",
            ),
        ],
        bytes,
    ))
}
/// Apply selected reviewed files only if their original contents still match the task snapshot.
pub async fn apply_task(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<Uuid>,
    ValidatedJson(input): ValidatedJson<SandboxApply>,
) -> Result<Json<SandboxTask>, AppError> {
    let _claim = state.sandbox.admission.lock().await;
    let mut saved = task(&state, id, key, caller, SandboxPermission::Apply).await?;
    if saved.task.state.active() || saved.guest_pending || saved.guest_staged {
        return Err(error(
            id,
            "sandbox_busy",
            "Finish and reconcile the task before applying changes",
        ));
    }
    let store = state.store.clone();
    let project_id = saved.task.project_id;
    if db(id, move || store.sandbox_project_busy(project_id)).await? {
        return Err(error(
            id,
            "sandbox_busy",
            "Finish or reconcile the project's active attempt before applying changes",
        ));
    }
    let project = project(
        &state,
        id,
        saved.task.project_id,
        caller,
        SandboxPermission::Apply,
    )
    .await?;
    if project.spec.source_path != saved.project.source_path {
        return Err(error(
            id,
            "sandbox_source_changed",
            "Project source changed after this task; restore its source before applying",
        ));
    }
    let source = PathBuf::from(
        saved
            .project
            .source_path
            .as_ref()
            .ok_or_else(|| invalid(id, "sourcePath", "Task has no project source"))?,
    );
    let mut changes = saved.task.changes.clone();
    let paths = input.paths;
    saved.task.changes = tokio::task::spawn_blocking(move || {
        ai_sandbox::workspace::apply(&source, &mut changes, &paths)?;
        Ok::<_, std::io::Error>(changes)
    })
    .await
    .map_err(|_| AppError::internal(id.0))?
    .map_err(|e| invalid(id, "changes", e))?;
    if saved.task.changes.iter().all(|c| c.applied) {
        saved.task.state = SandboxTaskState::Merged;
    }
    saved.task.updated_at = ai_sandbox::now();
    let store = state.store.clone();
    let copy = saved.clone();
    db(id, move || store.save_sandbox_task(&copy)).await?;
    Ok(Json(saved.task))
}

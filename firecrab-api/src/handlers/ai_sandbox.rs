//! AI Sandbox REST lifecycle. Credentials and host process execution are never client inputs.
use crate::{
    ai_sandbox::{self, ModelProof, SavedTask},
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
async fn db<T: Send + 'static>(
    id: RequestId,
    work: impl FnOnce() -> Result<T, crate::persistence::PersistenceError> + Send + 'static,
) -> Result<T, AppError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| AppError::internal(id.0))?
        .map_err(|_| AppError::internal(id.0))
}
async fn project(state: &AppState, id: RequestId, key: Uuid) -> Result<SandboxProject, AppError> {
    let store = state.store.clone();
    db(id, move || store.sandbox_project(key))
        .await?
        .ok_or_else(|| AppError::not_found(id.0))
}
async fn task(state: &AppState, id: RequestId, key: Uuid) -> Result<SavedTask, AppError> {
    let store = state.store.clone();
    db(id, move || store.sandbox_task(key))
        .await?
        .ok_or_else(|| AppError::not_found(id.0))
}
async fn account(state: &AppState, id: RequestId, key: Uuid) -> Result<SavedConnection, AppError> {
    let store = state.store.clone();
    let mut saved = db(id, move || store.ai_connection(key))
        .await?
        .ok_or_else(|| error(id, "ai_connection_missing", "AI account is disconnected"))?;
    if saved.connection.provider == AiProvider::Openai
        && state
            .pipe_ai
            .refresh(&mut saved)
            .await
            .map_err(|e| provider(id, e))?
    {
        let store = state.store.clone();
        let copy = saved.clone();
        db(id, move || store.save_ai_connection(&copy)).await?;
    }
    Ok(saved)
}

/// List persistent projects independently of provider availability.
pub async fn projects(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
) -> Result<Json<Vec<SandboxProject>>, AppError> {
    let store = state.store.clone();
    Ok(Json(db(id, move || store.sandbox_projects()).await?))
}
/// Fetch one project's current settings.
pub async fn get_project(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(key): Path<Uuid>,
) -> Result<Json<SandboxProject>, AppError> {
    Ok(Json(project(&state, id, key).await?))
}

/// Save explicit user authorization for a source directory, borrowed VM and model default.
pub async fn save_project(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(key): Path<Uuid>,
    ValidatedJson(mut spec): ValidatedJson<SandboxProjectSpec>,
) -> Result<Json<SandboxProject>, AppError> {
    let _claim = state.sandbox.admission.lock().await;
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
    Path(key): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let _claim = state.sandbox.admission.lock().await;
    project(&state, id, key).await?;
    let store = state.store.clone();
    if db(id, move || store.sandbox_project_busy(key)).await? {
        return Err(error(
            id,
            "sandbox_busy",
            "Cancel active work and reconcile guest resources before deleting this project",
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
    Path(key): Path<Uuid>,
    Query(query): Query<Discovery>,
) -> Result<Json<SandboxModelCatalog>, AppError> {
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
    Path(key): Path<Uuid>,
    ValidatedJson(input): ValidatedJson<SandboxVerifyModel>,
) -> Result<Json<SandboxModel>, AppError> {
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
    Path(key): Path<Uuid>,
    Query(query): Query<TaskQuery>,
) -> Result<Json<Vec<SandboxTaskSummary>>, AppError> {
    project(&state, id, key).await?;
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
            store.sandbox_summaries(key, query.since, before)
        })
        .await?,
    ))
}
/// Poll a task's persisted progress and results.
pub async fn get_task(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(key): Path<Uuid>,
) -> Result<Json<SandboxTask>, AppError> {
    Ok(Json(task(&state, id, key).await?.task))
}

/// Persist admission before dispatch. Repeating an identical task id never starts another attempt.
pub async fn create_task(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path((project_id, key)): Path<(Uuid, Uuid)>,
    ValidatedJson(mut input): ValidatedJson<SandboxTaskInput>,
) -> Result<(StatusCode, Json<SandboxTask>), AppError> {
    let claim = state.sandbox.admission.clone().lock_owned().await;
    let project = project(&state, id, project_id).await?;
    let store = state.store.clone();
    let previous = db(id, move || store.sandbox_task(key)).await?;
    let accounts = state
        .store
        .ai_connections()
        .map_err(|_| AppError::internal(id.0))?;
    input.input = ai_sandbox::redact(&accounts, input.input.trim());
    if let Some(previous) = previous {
        if previous.task.project_id != project_id || previous.task.request != input {
            return Err(error(
                id,
                "sandbox_task_conflict",
                "This task id already belongs to another request",
            ));
        }
        return Ok((StatusCode::OK, Json(previous.task)));
    }
    let store = state.store.clone();
    if db(id, move || store.sandbox_pending_count()).await? >= 16 {
        return Err(error(
            id,
            "sandbox_queue_full",
            "The controller already has 16 pending attempts; finish or reconcile one before starting another",
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
    let store = state.store.clone();
    if db(id, move || store.sandbox_project_busy(project_id)).await? {
        return Err(error(
            id,
            "sandbox_busy",
            "Finish or reconcile the current task before starting another",
        ));
    }
    if let Some(retry) = input.retry_of {
        let store = state.store.clone();
        let prior = db(id, move || store.sandbox_task(retry))
            .await?
            .filter(|t| t.task.project_id == project_id)
            .ok_or_else(|| {
                invalid(
                    id,
                    "retryOf",
                    "Previous task does not belong to this project",
                )
            })?;
        if !matches!(
            prior.task.state,
            SandboxTaskState::Failed | SandboxTaskState::Cancelled | SandboxTaskState::Interrupted
        ) || prior.guest_pending
            || prior.guest_staged
        {
            return Err(error(
                id,
                "sandbox_retry_blocked",
                "Reconcile the previous attempt before retrying",
            ));
        }
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
    ai_sandbox::proof(&state, &selection, input.kind == SandboxTaskKind::Code)
        .map_err(|e| invalid(id, "selection", e))?;
    if input.kind == SandboxTaskKind::Code {
        if project.spec.source_path.is_none() {
            return Err(invalid(
                id,
                "sourcePath",
                "Select a project source directory for code tasks",
            ));
        }
        let vm = project
            .spec
            .vm_id
            .ok_or_else(|| invalid(id, "vmId", "Select a MicroVM for code tasks"))?;
        ai_sandbox::guest::binding(&state, vm).map_err(|e| invalid(id, "vmId", e))?;
    }
    let timestamp = ai_sandbox::now();
    let saved = SavedTask {
        task: SandboxTask {
            id: key,
            project_id,
            attempt_id: Uuid::new_v4(),
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
        project: project.spec,
        workspace: None,
        guest: None,
        guest_pending: false,
        guest_command: None,
        guest_staged: false,
    };
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
    Path(key): Path<Uuid>,
) -> Result<(StatusCode, Json<SandboxTask>), AppError> {
    let saved = task(&state, id, key).await?;
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
    Path(key): Path<Uuid>,
) -> Result<Json<SandboxTask>, AppError> {
    let _claim = state.sandbox.admission.lock().await;
    let mut saved = task(&state, id, key).await?;
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
        saved.task.changes =
            ai_sandbox::workspace::changes(path).map_err(|e| invalid(id, "workspace", e))?;
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
    saved.task.error = Some("Interrupted attempt reconciled; retry creates a new attempt".into());
    let store = state.store.clone();
    let copy = saved.clone();
    db(id, move || store.save_sandbox_task(&copy)).await?;
    Ok(Json(saved.task))
}
/// Apply selected reviewed files only if their original contents still match the task snapshot.
pub async fn apply_task(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(key): Path<Uuid>,
    ValidatedJson(input): ValidatedJson<SandboxApply>,
) -> Result<Json<SandboxTask>, AppError> {
    let _claim = state.sandbox.admission.lock().await;
    let mut saved = task(&state, id, key).await?;
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
    let project = project(&state, id, saved.task.project_id).await?;
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

//! Native, journaled stack execution. Request cancellation does not abandon accepted work.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use firecrab_stack::{
    Action, ApplyFailure, ApplyRequest, ApplyState, ApplyStatus, ApplyStep, GitCheck, StepState,
};
use uuid::Uuid;

use super::stacks::{database, inventory, validate_name, validation_errors};
use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::persistence::PersistenceError;
use crate::server::RequestId;
use crate::state::AppState;

mod execute;

/// Accept a fresh plan or return an identical operation without repeating its effects.
pub async fn apply(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(name): Path<String>,
    ValidatedJson(request): ValidatedJson<ApplyRequest>,
) -> Result<(StatusCode, Json<ApplyStatus>), AppError> {
    validate_name(&name, request_id.0)?;
    if name != request.stack.name || request.operation_id.is_nil() {
        return Err(AppError::conflict(
            "invalid_stack_apply_identity",
            "URL name must match the manifest and operation ID must be non-nil",
            request_id.0,
        ));
    }
    firecrab_stack::validate(&request.stack).map_err(|e| validation_errors(e, request_id.0))?;
    // The independent admission task commits the journal and spawns its worker even
    // when the HTTP deadline drops this JoinHandle before acknowledgement arrives.
    let status = tokio::spawn(admit(state, request, None))
        .await
        .map_err(|_| AppError::internal(request_id.0))??;
    Ok((StatusCode::ACCEPTED, Json(status)))
}

/// Read redacted progress after disconnects, failures or an API restart.
pub async fn get(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path((name, id)): Path<(String, Uuid)>,
) -> Result<Json<ApplyStatus>, AppError> {
    validate_name(&name, request_id.0)?;
    database(request_id.0, move || {
        state
            .store
            .stored_stack_apply(id)?
            .filter(|s| s.status.name == name)
            .map(|s| s.status)
            .ok_or(PersistenceError::MissingStackApply)
    })
    .await
    .map(Json)
}

pub(crate) async fn admit(
    state: AppState,
    request: ApplyRequest,
    source: Option<GitCheck>,
) -> Result<ApplyStatus, AppError> {
    let _admission = state.stack_apply_admission.lock().await;
    let id = request.operation_id;
    let hash = firecrab_stack::desired_hash(&request.stack);
    let store = state.store.clone();
    let templates = state.templates.clone();
    let name = request.stack.name.clone();
    let (record, observed, previous) = database(id, move || {
        let record = store.stack(&name)?.ok_or(PersistenceError::MissingStack)?;
        Ok((
            record,
            inventory(&store, &templates)?,
            store.stored_stack_apply(id)?,
        ))
    })
    .await?;
    if let Some(previous) = previous {
        if previous.status.stack_id != record.id
            || previous.desired_hash != hash
            || previous.allow_stop != request.allow_stop
            || previous.status.git_revision != source.as_ref().and_then(GitCheck::revision)
        {
            return Err(AppError::conflict(
                "stack_apply_identity_conflict",
                "reuse requires the exact registration, declaration and stop permission",
                id,
            ));
        }
        return Ok(previous.status);
    }
    let mut plan = firecrab_stack::plan(&request.stack, &record, &observed)
        .map_err(|e| validation_errors(e, id))?;
    if request.expected_fingerprint.len() != 64
        || !request
            .expected_fingerprint
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
        || plan.fingerprint != request.expected_fingerprint
    {
        return Err(AppError::conflict(
            "stale_stack_plan",
            "review a fresh plan for the exact declaration and host configuration",
            id,
        ));
    }
    if plan
        .changes
        .iter()
        .any(|c| matches!(c.action, Action::Blocked | Action::Replace))
    {
        return Err(AppError::conflict(
            "unsupported_stack_plan",
            "resolve blocked entries; automatic replacement and pruning are not supported",
            id,
        ));
    }
    for change in &mut plan.changes {
        if change.action == Action::Create {
            change.resource_id = Some(Uuid::new_v4());
        }
    }
    execute::preflight(&state, &request.stack, &plan, request.allow_stop, id)
        .await
        .map_err(AppError::redacted)?;
    let status = ApplyStatus {
        inputs: plan.inputs,
        git_revision: source.as_ref().and_then(GitCheck::revision),
        operation_id: id,
        stack_id: record.id,
        name: record.name.clone(),
        state: ApplyState::Queued,
        changes: plan
            .changes
            .into_iter()
            .map(|change| ApplyStep {
                change,
                state: StepState::Pending,
            })
            .collect(),
        error: None,
    };
    let store = state.store.clone();
    let expected = record.clone();
    let queued = status.clone();
    let services = state.service_mutations.lock().await;
    let (status, fresh) = database(id, move || {
        store.begin_stack_apply(
            &expected,
            &hash,
            request.allow_stop,
            &queued,
            source.as_ref(),
        )
    })
    .await?;
    drop(services);
    if fresh {
        let worker_state = state.clone();
        let worker_status = status.clone();
        let task = tokio::spawn(execute::run(
            worker_state,
            request.stack,
            worker_status,
            record,
            observed,
            request.allow_stop,
        ));
        tokio::spawn(async move {
            if task.await.is_err() {
                let store = state.store.clone();
                let _ = database(id, move || {
                    if let Some(mut job) = store.stored_stack_apply(id)? {
                        fail_status(
                            &mut job.status,
                            ApplyFailure {
                                code: "worker_failed".into(),
                                message: "inspect recorded UUIDs before explicit recovery".into(),
                                fields: vec![],
                            },
                        );
                        store.save_stack_apply(&job.status)?;
                    }
                    Ok(())
                })
                .await;
            }
        });
    }
    Ok(status)
}

fn fail_status(status: &mut ApplyStatus, error: ApplyFailure) {
    status.state = ApplyState::Failed;
    status.error = Some(error);
    for step in &mut status.changes {
        step.state = match step.state {
            StepState::Running => StepState::Failed,
            StepState::Pending => StepState::Skipped,
            current => current,
        };
    }
}

#[cfg(test)]
mod tests;

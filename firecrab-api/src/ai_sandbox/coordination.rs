//! Dependencies and bounded explicit retries extend the existing durable task lifecycle.
use super::{SavedTask, access::Caller, now, save};
use crate::{persistence::Store, state::AppState};
use firecrab_api_types::*;
use std::{collections::BTreeSet, time::Duration};
use tokio::sync::watch;
use uuid::Uuid;

/// Validate assignments and retry budgets without accepting caller-provided attempt identities.
pub fn validate(input: &SandboxTaskInput) -> Result<(), String> {
    let policy = &input.coordination;
    if policy.retry_limit > 3 || policy.dependencies.len() > 8 {
        return Err("Use at most eight dependencies and three explicit retries".into());
    }
    if policy.assigned_agent.as_ref().is_some_and(|agent| {
        agent.is_empty()
            || agent.len() > 64
            || !agent
                .chars()
                .all(|c| c.is_alphanumeric() || " _-.".contains(c))
    }) {
        return Err(
            "Use an agent label up to 64 characters without control or path characters".into(),
        );
    }
    if policy.dependencies.iter().any(Uuid::is_nil)
        || policy.dependencies.iter().collect::<BTreeSet<_>>().len() != policy.dependencies.len()
    {
        return Err("Dependency task identities must be distinct non-nil UUIDs".into());
    }
    Ok(())
}

/// A retry extends only the latest settled attempt and cannot replace the initial policy.
pub fn retry(
    store: &Store,
    project: Uuid,
    key: Uuid,
    input: &SandboxTaskInput,
) -> Result<(Uuid, u8), String> {
    let Some(prior_id) = input.retry_of else {
        return Ok((key, 0));
    };
    let prior = store
        .sandbox_task(prior_id)
        .map_err(|_| "Cannot read previous attempt")?
        .ok_or("Previous task is unavailable")?;
    let root = prior.task.retry_root.unwrap_or(prior.task.id);
    let chain = store
        .sandbox_retry_tasks(root)
        .map_err(|_| "Cannot read retry chain")?;
    let initial = chain.first().ok_or("Retry chain is unavailable")?;
    let latest = chain.last().ok_or("Retry chain is unavailable")?;
    if prior.task.project_id != project || latest.task.id != prior_id {
        return Err("Retry only the latest attempt in this project's retry chain".into());
    }
    if !matches!(
        prior.task.state,
        SandboxTaskState::Failed | SandboxTaskState::Cancelled
    ) || prior.guest_pending
        || prior.guest_staged
    {
        return Err("Reconcile interrupted or uncertain work before explicitly retrying".into());
    }
    let policy = &initial.task.request.coordination;
    if input.coordination.retry_limit != policy.retry_limit
        || input.coordination.dependencies != policy.dependencies
        || input.kind != initial.task.request.kind
    {
        return Err(
            "An explicit retry must preserve the initial retry budget, dependencies and task kind"
                .into(),
        );
    }
    let number = prior
        .task
        .retry_number
        .checked_add(1)
        .ok_or("Retry budget exhausted")?;
    if number > policy.retry_limit {
        return Err("Explicit retry budget exhausted".into());
    }
    Ok((root, number))
}

/// Validate dependency authorization and fixed source identity at admission.
pub fn dependencies(
    state: &AppState,
    caller: Caller,
    project: Uuid,
    source: Option<&str>,
    input: &SandboxTaskInput,
) -> Result<(), String> {
    let mut roots = BTreeSet::new();
    for id in &input.coordination.dependencies {
        let saved = state
            .store
            .sandbox_task(*id)
            .map_err(|_| "Cannot read dependency")?
            .ok_or("Dependency task is unavailable")?;
        caller.task(state, &saved, SandboxPermission::Read)?;
        if saved.task.project_id != project || saved.project.source_path.as_deref() != source {
            return Err("Dependencies must belong to the same project and source directory".into());
        }
        if !roots.insert(saved.task.retry_root.unwrap_or(saved.task.id)) {
            return Err("Select each dependency retry chain only once".into());
        }
    }
    Ok(())
}

/// Initial unrelated work preserves the existing one-project admission boundary.
/// Dependents and a retry needed by a waiting dependent can share the durable queue.
pub fn queued_allowed(
    store: &Store,
    project: Uuid,
    input: &SandboxTaskInput,
    root: Uuid,
) -> Result<bool, String> {
    let busy = store
        .sandbox_busy_tasks(project)
        .map_err(|_| "Cannot read project queue")?;
    if busy.is_empty() {
        return Ok(true);
    }
    if !input.coordination.dependencies.is_empty() {
        return Ok(true);
    }
    if input.retry_of.is_none() {
        return Ok(false);
    }
    for saved in busy {
        if saved.task.state != SandboxTaskState::Waiting
            || saved.guest_pending
            || saved.guest_staged
        {
            return Ok(false);
        }
        let mut pending = saved.task.request.coordination.dependencies;
        let mut seen = BTreeSet::new();
        let mut found = false;
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            let dependency = store
                .sandbox_task(id)
                .map_err(|_| "Cannot read dependency")?
                .ok_or("Dependency task is unavailable")?;
            if dependency.task.retry_root.unwrap_or(dependency.task.id) == root {
                found = true;
                break;
            }
            pending.extend(dependency.task.request.coordination.dependencies);
        }
        if !found {
            return Ok(false);
        }
    }
    Ok(true)
}

enum DependencyState {
    Waiting,
    Ready(Vec<SavedTask>),
    Failed,
}

fn state(store: &Store, saved: &SavedTask) -> Result<DependencyState, String> {
    let mut ready = Vec::new();
    let mut waiting = false;
    for id in &saved.task.request.coordination.dependencies {
        let original = store
            .sandbox_task(*id)
            .map_err(|_| "Cannot read dependency")?
            .ok_or("Dependency task is unavailable")?;
        let chain = store
            .sandbox_retry_tasks(original.task.retry_root.unwrap_or(*id))
            .map_err(|_| "Cannot read dependency retry chain")?;
        let latest = chain
            .last()
            .ok_or("Dependency retry chain is unavailable")?;
        if latest.task.project_id != saved.task.project_id
            || latest.project.source_path != saved.project.source_path
        {
            return Err("Dependency project or source identity changed".into());
        }
        match latest.task.state {
            SandboxTaskState::Result | SandboxTaskState::Merged
                if !latest.guest_pending && !latest.guest_staged =>
            {
                ready.push(latest.clone())
            }
            SandboxTaskState::Cancelled => return Ok(DependencyState::Failed),
            SandboxTaskState::Failed
                if latest.task.retry_number >= chain[0].task.request.coordination.retry_limit =>
            {
                return Ok(DependencyState::Failed);
            }
            _ => waiting = true,
        }
    }
    if waiting {
        Ok(DependencyState::Waiting)
    } else {
        Ok(DependencyState::Ready(ready))
    }
}

/// Wait before taking a provider permit. Persist Accepted under the same admission claim as dispatch.
pub async fn wait(
    state: &AppState,
    saved: &mut SavedTask,
    cancel: &mut watch::Receiver<bool>,
    deadline: tokio::time::Instant,
) -> Result<Vec<SavedTask>, super::ExecutionError> {
    loop {
        if *cancel.borrow() {
            return Err((
                "sandbox_cancelled".into(),
                "Task cancelled by the user".into(),
            ));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err((
                "sandbox_timeout".into(),
                "Dependency or project queue exceeded the 10-minute wait limit".into(),
            ));
        }
        {
            let _claim = state.sandbox.admission.lock().await;
            super::access::attempt(state, saved).map_err(super::execution_error)?;
            match self::state(&state.store, saved).map_err(super::execution_error)? {
                DependencyState::Failed => {
                    return Err((
                        "sandbox_dependency_failed".into(),
                        "A dependency was cancelled or exhausted its bounded explicit retry budget"
                            .into(),
                    ));
                }
                DependencyState::Ready(dependencies) => {
                    for dependency in &dependencies {
                        saved
                            .caller
                            .task(state, dependency, SandboxPermission::Read)
                            .map_err(super::execution_error)?;
                    }
                    let busy = state
                        .store
                        .sandbox_busy_tasks(saved.task.project_id)
                        .map_err(|_| super::execution_error("Cannot inspect project execution"))?;
                    if !busy.iter().any(|task| {
                        task.task.id != saved.task.id
                            && (matches!(
                                task.task.state,
                                SandboxTaskState::Accepted | SandboxTaskState::InProgress
                            ) || task.guest_pending
                                || task.guest_staged)
                    }) {
                        saved.task.dependency_results = dependencies
                            .iter()
                            .map(|task| SandboxDependencyResult {
                                task_id: task.task.id,
                                attempt_id: task.task.attempt_id,
                                assigned_agent: task
                                    .task
                                    .request
                                    .coordination
                                    .assigned_agent
                                    .clone(),
                                result: super::context_output(&task.task.result),
                                changed_paths: task
                                    .task
                                    .changes
                                    .iter()
                                    .map(|change| change.path.clone())
                                    .collect(),
                                tests: task.task.tests.clone(),
                                artifacts: task.task.artifacts.clone(),
                            })
                            .collect();
                        saved.task.state = SandboxTaskState::Accepted;
                        saved.task.updated_at = now();
                        save(state, saved).await.map_err(super::execution_error)?;
                        return Ok(dependencies);
                    }
                }
                DependencyState::Waiting => {}
            }
        }
        tokio::select! {
            _ = cancel.changed() => {},
            _ = tokio::time::sleep(Duration::from_millis(100)) => {},
        }
    }
}

#[cfg(test)]
#[path = "coordination_tests.rs"]
mod tests;

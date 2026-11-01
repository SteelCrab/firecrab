//! Explicit, idempotent agent execution; real AUTH credentials remain on the controller.
mod broker;
mod native;
mod requests;
use crate::{
    ai_sandbox::{access::Caller, guest},
    code_agent_auth,
    error::AppError,
    extract::ValidatedJson,
    persistence::{code_runtime::Record, code_verification::Snapshot},
    server::RequestId,
    state::AppState,
};
use CodeAgentRunPhase as Phase;
use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use firecrab_api_types::*;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, future::Future, sync::Mutex, time::Duration};
use tokio::sync::watch;
use uuid::Uuid;
#[derive(Default)]
pub(crate) struct Runs(Mutex<HashMap<Uuid, watch::Sender<bool>>>);
impl std::fmt::Debug for Runs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRuns").finish_non_exhaustive()
    }
}
pub(crate) struct Context {
    snapshot: Snapshot,
    record: Record,
    profile: CodeAgentAuthAdapter,
    request: CodeAgentRunRequest,
}
pub(crate) struct Outcome {
    code: i32,
    failed: bool,
}
pub(crate) trait Driver: Sync {
    fn execute(
        &self,
        state: &AppState,
        context: &Context,
        run: &mut CodeAgentRun,
    ) -> impl Future<Output = Result<Outcome, &'static str>> + Send;
}
fn error(id: RequestId, code: &'static str) -> AppError {
    AppError::new(
        StatusCode::CONFLICT,
        code,
        "Read the current workspace, assigned AUTH and ready runtime before running its agent",
        id.0,
    )
}
fn operator(caller: Caller, id: RequestId) -> Result<(), AppError> {
    caller.operator().map_err(|_| {
        AppError::new(
            StatusCode::FORBIDDEN,
            "code_operator_required",
            "Only the controller operator can execute CODE agents",
            id.0,
        )
    })
}
fn hash(body: &CodeAgentRunRequest) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(body).expect("request"))
    )
}
fn current(state: &AppState, context: &Context) -> bool {
    state.store.code_agent_current(&context.snapshot).ok() == Some(true)
        && state
            .store
            .code_runtime(context.snapshot.workspace.id)
            .ok()
            .flatten()
            .is_some_and(|r| r.runtime == context.record.runtime && r.spec == context.record.spec)
        && crate::code_runtime::auth_binding(state, &context.record).is_ok()
}
fn active(state: &AppState, workspace: Uuid, id: Uuid) -> bool {
    state
        .store
        .code_agent_run(workspace, id)
        .ok()
        .flatten()
        .is_some_and(|(r, _)| r.phase.active() && r.phase != Phase::Cancelling)
}
fn save(state: &AppState, run: &mut CodeAgentRun) -> bool {
    run.updated_at_ms = chrono::Utc::now().timestamp_millis();
    state.store.save_code_agent_run(run).ok() == Some(true)
}
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/code/workspaces/{id}/agent-runs", get(requests::list))
        .route(
            "/api/code/workspaces/{id}/agent-runs/{run}",
            get(requests::read).post(requests::start),
        )
        .route(
            "/api/code/workspaces/{id}/agent-runs/{run}/cancel",
            post(requests::cancel),
        )
        .layer(axum::middleware::from_fn(
            crate::handlers::secrets::no_store,
        ))
}
fn repair(state: &AppState, mut run: CodeAgentRun) -> CodeAgentRun {
    if run.phase.active()
        && !state
            .code_agent_runs
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(&run.id)
        && let Ok(_claim) = crate::code_runtime::lock(state, run.workspace_id)
    {
        if run.phase == Phase::Cancelling {
            return state
                .store
                .cancel_code_agent_run(run.workspace_id, run.id, true)
                .ok()
                .flatten()
                .unwrap_or(run);
        }
        run.phase = Phase::Interrupted;
        run.error = Some("controller_interrupted".into());
        save(state, &mut run);
    }
    run
}
async fn execute(
    state: AppState,
    context: Context,
    mut run: CodeAgentRun,
    mut cancel: watch::Receiver<bool>,
    driver: &impl Driver,
) {
    let workspace = run.workspace_id;
    let id = run.id;
    let monitor = async {
        loop {
            tokio::select! {_=cancel.changed()=>return "cancelled",_=tokio::time::sleep(Duration::from_millis(500))=>{}}
            if !current(&state, &context) || !active(&state, workspace, id) {
                return "bindings_changed";
            }
        }
    };
    let work = async {
        let _permit = state
            .code_jobs
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "worker_unavailable")?;
        if !current(&state, &context) || !active(&state, workspace, id) {
            return Err("bindings_changed");
        }
        run.phase = Phase::Starting;
        if !save(&state, &mut run) {
            return Err("bindings_changed");
        }
        driver.execute(&state, &context, &mut run).await
    };
    let result = tokio::select! {result=tokio::time::timeout(Duration::from_secs(600),work)=>result.unwrap_or(Err("timed_out")),reason=monitor=>Err(reason)};
    let result = if current(&state, &context) {
        result
    } else {
        Err("bindings_changed")
    };
    let cancelled = state
        .store
        .code_agent_run(workspace, id)
        .ok()
        .flatten()
        .is_some_and(|(r, _)| r.phase == Phase::Cancelling);
    if cancelled {
        let _ = state.store.cancel_code_agent_run(workspace, id, true);
    } else {
        match result {
            Ok(outcome) => {
                run.exit_code = Some(outcome.code);
                run.phase = if outcome.code == 0 && !outcome.failed && run.provider_requests > 0 {
                    Phase::Succeeded
                } else {
                    Phase::Failed
                };
                if run.phase == Phase::Failed {
                    run.error = Some("agent_failed".into());
                }
            }
            Err(reason) => {
                run.phase = if matches!(reason, "bindings_changed" | "cancelled") {
                    Phase::Interrupted
                } else {
                    Phase::Failed
                };
                run.error = Some(reason.into());
            }
        }
        save(&state, &mut run);
    }
    state
        .code_agent_runs
        .0
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&id);
}
#[cfg(test)]
mod tests;

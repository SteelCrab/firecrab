//! Operator-only recipe discovery and disposable runtime configuration checks.
use super::{database, operator};
use crate::{
    ai_sandbox::access::Caller,
    code_agent_auth::{self, Probe},
    error::AppError,
    extract::ValidatedJson,
    persistence::{code_runtime::Record, code_verification::Snapshot},
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use firecrab_api_types::*;
use std::time::Duration;

pub async fn catalog(
    caller: Caller,
    Extension(id): Extension<RequestId>,
) -> Result<Json<Vec<CodeAgentAuthAdapter>>, AppError> {
    operator(caller, id)?;
    Ok(Json(code_agent_auth::catalog()))
}
fn blocked(reason: CodeAgentAuthFailure) -> CodeAgentAuthResult {
    CodeAgentAuthResult::Blocked { reason }
}
fn changed(id: RequestId) -> AppError {
    AppError::conflict(
        "code_workspace_bindings_changed",
        "Workspace, account or runtime changed during the check; reload before retrying",
        id.0,
    )
}
fn runtime_current(state: &AppState, record: &Record, id: RequestId) -> Result<bool, AppError> {
    Ok(state
        .store
        .code_runtime(record.runtime.workspace_id)
        .map_err(|_| AppError::internal(id.0))?
        .is_some_and(|r| {
            r.runtime == record.runtime
                && r.spec == record.spec
                && r.vm_created == record.vm_created
        }))
}
pub(crate) async fn check(
    state: &AppState,
    mut snapshot: Snapshot,
    record: Record,
    profile: CodeAgentAuthAdapter,
    probe: &impl Probe,
    id: RequestId,
) -> Result<CodeAgentAuthResult, AppError> {
    let key = snapshot.workspace.id;
    if !profile.supported {
        return Ok(blocked(CodeAgentAuthFailure::AdapterUnavailable));
    }
    let expected = snapshot
        .workspace
        .spec
        .agent
        .as_ref()
        .zip(snapshot.ai.as_ref())
        .and_then(|(binding, saved)| code_agent_auth::resolve(binding, saved));
    if expected.is_none_or(|p| p != profile) {
        return Ok(blocked(CodeAgentAuthFailure::Incompatible));
    }
    if record.runtime.workspace_id != key
        || record.runtime.phase != CodeRuntimePhase::Ready
        || !runtime_current(state, &record, id)?
        || !state
            .store
            .code_verification_current(&snapshot)
            .map_err(|_| AppError::internal(id.0))?
    {
        return Err(changed(id));
    }
    let provider = tokio::time::timeout(
        Duration::from_secs(25),
        super::verification::ai(state, &mut snapshot, id),
    )
    .await;
    use CodeAgentAuthFailure as F;
    match provider {
        Ok(Ok(CodeAiVerification::CredentialsVerified { .. })) => {}
        Ok(Err(error)) => return Err(error),
        Ok(Ok(CodeAiVerification::Failed { reason })) => {
            return Ok(blocked(match reason {
                CodeVerificationFailure::Missing => F::Missing,
                CodeVerificationFailure::Incompatible => F::Incompatible,
                CodeVerificationFailure::AuthenticationRejected => F::AuthenticationRejected,
                CodeVerificationFailure::ConsentRequired => F::ConsentRequired,
                CodeVerificationFailure::TimedOut => F::TimedOut,
                _ => F::ProviderUnavailable,
            }));
        }
        _ => return Ok(blocked(F::TimedOut)),
    }
    if !state
        .store
        .code_verification_current(&snapshot)
        .map_err(|_| AppError::internal(id.0))?
    {
        return Err(changed(id));
    }
    let saved = snapshot.ai.as_ref().ok_or_else(|| changed(id))?;
    let outcome = probe.check(state, &record, &profile, saved).await;
    if !state
        .store
        .code_verification_current(&snapshot)
        .map_err(|_| AppError::internal(id.0))?
        || !runtime_current(state, &record, id)?
    {
        return Err(changed(id));
    }
    Ok(match outcome {
        Ok(()) => CodeAgentAuthResult::ConfigurationVerified {
            adapter_id: profile.id,
            cli_version: profile.cli_version,
            vm_id: record.runtime.vm_id,
            runtime_id: record.runtime.runtime_id.ok_or_else(|| changed(id))?,
        },
        Err(reason) => blocked(reason),
    })
}
pub async fn verify(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    Path(key): Path<String>,
    ValidatedJson(input): ValidatedJson<CodeWorkspaceVerify>,
) -> Result<Json<CodeAgentAuthVerification>, AppError> {
    operator(caller, id)?;
    let key = crate::handlers::vms::parse_id(&key, id.0)?;
    if input.expected_revision == 0 || input.expected_revision > i64::MAX as u64 {
        return Err(AppError::validation(
            [(
                "expectedRevision".into(),
                "use a positive saved revision".into(),
            )]
            .into(),
            id.0,
        ));
    }
    let store = state.store.clone();
    let snapshot = database(id, move || store.code_verification_snapshot(key))
        .await?
        .ok_or_else(|| AppError::not_found(id.0))?;
    if snapshot.workspace.revision != input.expected_revision {
        return Err(super::conflict(id));
    }
    let result = if let Some(binding) = &snapshot.workspace.spec.agent {
        let profile = snapshot
            .ai
            .as_ref()
            .and_then(|saved| code_agent_auth::resolve(binding, saved));
        match profile {
            None => blocked(if snapshot.ai.is_none() {
                CodeAgentAuthFailure::Missing
            } else {
                CodeAgentAuthFailure::Incompatible
            }),
            Some(p) if !p.supported => blocked(CodeAgentAuthFailure::AdapterUnavailable),
            Some(profile) => {
                let guard = state
                    .pipe_ai_mutations
                    .clone()
                    .try_lock_owned()
                    .map_err(|_| {
                        AppError::conflict("ai_busy", "Another AI operation is in progress", id.0)
                    })?;
                let claim = crate::code_runtime::lock(&state, key).map_err(|_| {
                    AppError::conflict(
                        "code_runtime_busy",
                        "A runtime operation is in progress",
                        id.0,
                    )
                })?;
                let record = state
                    .store
                    .code_runtime(key)
                    .map_err(|_| AppError::internal(id.0))?;
                if let Some(record) = record.filter(|r| {
                    r.runtime.phase == CodeRuntimePhase::Ready
                        && r.runtime.runtime_id.is_some()
                        && r.runtime.error.is_none()
                }) {
                    // HTTP cancellation must not release credential/lifecycle gates while a guest probe still runs.
                    let (send, recv) = tokio::sync::oneshot::channel();
                    let state = state.clone();
                    tokio::spawn(async move {
                        let (_guard, _claim) = (guard, claim);
                        let result = check(
                            &state,
                            snapshot,
                            record,
                            profile,
                            &code_agent_auth::Native,
                            id,
                        )
                        .await;
                        let _ = send.send(result);
                    });
                    recv.await.map_err(|_| AppError::internal(id.0))??
                } else {
                    blocked(CodeAgentAuthFailure::RuntimeUnavailable)
                }
            }
        }
    } else {
        CodeAgentAuthResult::NotRequired
    };
    Ok(Json(CodeAgentAuthVerification {
        id: key,
        revision: input.expected_revision,
        checked_at_ms: chrono::Utc::now().timestamp_millis(),
        result,
    }))
}

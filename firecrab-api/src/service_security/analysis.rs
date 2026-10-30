//! On-demand explanations of bounded security signals.
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use firecrab_api_types::{AiModelSelection, AiProvider};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use crate::{
    ai_sandbox::{self, access::Caller},
    error::AppError,
    extract::ValidatedJson,
    server::RequestId,
    state::AppState,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Explanation {
    service_id: Uuid,
    deployment_id: Uuid,
    text: String,
}

/// Explain fixed, aggregate signals only. Raw logs, addresses and credentials never enter the prompt.
pub(crate) async fn explain(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
    Path(service_id): Path<Uuid>,
    ValidatedJson(selection): ValidatedJson<AiModelSelection>,
) -> Result<Json<Explanation>, AppError> {
    caller.operator().map_err(|_| {
        AppError::new(
            StatusCode::FORBIDDEN,
            "service_operator_required",
            "Only the operator can access service security",
            request.0,
        )
    })?;
    let view = super::get(State(state.clone()), Extension(request), caller)
        .await?
        .0;
    let service = view
        .services
        .into_iter()
        .find(|service| service.id == service_id)
        .ok_or_else(|| AppError::not_found(request.0))?;
    if service.signals.is_empty() {
        return Err(AppError::conflict(
            "security_no_signals",
            "There are no activity signals to explain",
            request.0,
        ));
    }
    let signals =
        serde_json::to_string(&service.signals).map_err(|_| AppError::internal(request.0))?;
    let guard = state.pipe_ai_mutations.try_lock().map_err(|_| {
        AppError::conflict("ai_busy", "Another AI operation is in progress", request.0)
    })?;
    let mut account = ai_sandbox::proof(&state, &selection, false).map_err(|_| {
        AppError::new(
            StatusCode::BAD_REQUEST,
            "ai_model_not_verified",
            "Verify model access in AI Sandbox first",
            request.0,
        )
    })?;
    if account.connection.provider == AiProvider::Openai
        && state.pipe_ai.refresh(&mut account).await.map_err(|error| {
            let (status, code, message) = error.details();
            AppError::new(status, code, message, request.0)
        })?
    {
        state
            .store
            .save_ai_connection(&account)
            .map_err(|_| AppError::internal(request.0))?;
    }
    let connections = state
        .store
        .ai_connections()
        .map_err(|_| AppError::internal(request.0))?;
    let prompt = format!(
        "Explain these aggregated service security signals and suggest practical next steps. The signals are observations, not confirmed attacks. Do not infer missing facts. Data: {signals}"
    );
    let prompt = ai_sandbox::redact(&connections, &prompt);
    let history = [json!({"role":"user","content":prompt})];
    let turn = ai_sandbox::model::turn_with_instructions(
        &state, &account, &selection.model, &history, &[], false,
        Some("Explain security observations accurately. Do not claim an attack, CVE, test, or fix without evidence. Give concise, practical investigation steps. Do not request credentials."),
    )
        .await
        .map_err(|error| {
            let (status, code, message) = error.details();
            AppError::new(status, code, message, request.0)
        })?;
    drop(guard);
    if !turn.calls.is_empty() || turn.text.trim().is_empty() {
        return Err(AppError::new(
            StatusCode::BAD_GATEWAY,
            "security_ai_invalid",
            "AI did not return a complete explanation",
            request.0,
        ));
    }
    // A redeploy can complete while the provider is responding. Do not attach
    // its explanation to a replacement deployment.
    let store = state.store.clone();
    let current = crate::handlers::micro_services::database(request.0, move || {
        store.service_deployment(service_id)
    })
    .await?;
    if current.as_ref().map(|deployment| deployment.id) != Some(service.deployment_id) {
        return Err(AppError::conflict(
            "security_deployment_changed",
            "The service deployment changed during analysis. Retry with current signals.",
            request.0,
        ));
    }
    Ok(Json(Explanation {
        service_id,
        deployment_id: service.deployment_id,
        text: ai_sandbox::redact(&connections, &turn.text),
    }))
}

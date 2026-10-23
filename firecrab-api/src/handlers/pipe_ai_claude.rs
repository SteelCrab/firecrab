//! Account-management only. Provider credentials never cross the CLI boundary.
use axum::{
    Extension, Json,
    extract::{Path, State, WebSocketUpgrade},
    response::{IntoResponse, Response},
};
use firecrab_api_types::{
    AiAuthentication, AiCliLoginRequest, AiCliLoginStart, AiCliState, AiCliStatus, AiConnection,
    AiProvider,
};
use uuid::Uuid;

use super::pipe_ai::{connection, gate, missing, stored};
use crate::{
    error::AppError,
    extract::ValidatedJson,
    pipe_ai::{
        SavedConnection,
        claude_code::{self, Error},
    },
    server::RequestId,
    state::AppState,
};

fn failed(error: Error, id: RequestId) -> AppError {
    let (status, code, message) = error.details();
    AppError::new(status, code, message, id.0)
}

fn uncached_json(value: impl serde::Serialize) -> Response {
    (
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(value),
    )
        .into_response()
}

async fn require_cli(state: &AppState, key: Uuid, id: RequestId) -> Result<(), AppError> {
    if connection(state, key, id).await?.connection.authentication != AiAuthentication::ClaudeCode {
        return Err(missing(id));
    }
    Ok(())
}

pub async fn login_start(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(key): Path<Uuid>,
    ValidatedJson(input): ValidatedJson<AiCliLoginRequest>,
) -> Result<Response, AppError> {
    if input.name.trim().is_empty()
        || input.name.len() > 100
        || input.name.chars().any(char::is_control)
    {
        return Err(AppError::new(
            reqwest::StatusCode::BAD_REQUEST,
            "ai_invalid_input",
            "Invalid connection name",
            id.0,
        ));
    }
    let _guard = gate(&state, id)?;
    let store = state.store.clone();
    if let Some(saved) = stored(id, move || store.ai_connection(key)).await?
        && saved.connection.authentication != AiAuthentication::ClaudeCode
    {
        return Err(AppError::new(
            reqwest::StatusCode::CONFLICT,
            "ai_connection_type",
            "Use a new connection ID for Claude Code",
            id.0,
        ));
    }
    let operation = state.claude_code.operation(key);
    let _operation = operation.try_lock().map_err(|_| failed(Error::Busy, id))?;
    if state
        .claude_code
        .status(key)
        .await
        .map_err(|e| failed(e, id))?
        == AiCliState::Unavailable
    {
        return Err(failed(Error::Unavailable, id));
    }
    let ticket = state.claude_code.prepare(key).map_err(|e| failed(e, id))?;
    let saved = SavedConnection {
        connection: AiConnection {
            id: key,
            name: input.name.trim().into(),
            provider: AiProvider::Anthropic,
            authentication: AiAuthentication::ClaudeCode,
            endpoint: claude_code::ENDPOINT.into(),
            models: vec![],
            streaming: false,
            tool_calls: false,
        },
        api_key: String::new(),
        oauth: None,
    };
    let result = saved.connection.clone();
    let store = state.store.clone();
    if let Err(error) = stored(id, move || store.save_ai_connection(&saved)).await {
        state.claude_code.cancel(key);
        return Err(error);
    }
    Ok(uncached_json(AiCliLoginStart {
        connection: result,
        ticket,
        expires_in: claude_code::ATTACH_SECONDS,
    }))
}

pub async fn status(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(key): Path<Uuid>,
) -> Result<Response, AppError> {
    require_cli(&state, key, id).await?;
    let operation = state.claude_code.operation(key);
    let Ok(_guard) = operation.try_lock() else {
        return Ok(uncached_json(AiCliStatus {
            status: AiCliState::SigningIn,
        }));
    };
    Ok(uncached_json(AiCliStatus {
        status: state
            .claude_code
            .status(key)
            .await
            .map_err(|e| failed(e, id))?,
    }))
}

pub async fn terminal(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(key): Path<Uuid>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, AppError> {
    require_cli(&state, key, id).await?;
    Ok(upgrade
        .max_message_size(8192)
        .max_frame_size(8192)
        .on_upgrade(move |socket| {
            claude_code::login_terminal(state.claude_code.clone(), key, socket)
        }))
}

pub(super) async fn logout(state: &AppState, key: Uuid, id: RequestId) -> Result<(), AppError> {
    state.claude_code.cancel(key);
    let operation = state.claude_code.operation(key);
    let _guard = tokio::time::timeout(std::time::Duration::from_secs(5), operation.lock())
        .await
        .map_err(|_| failed(Error::Busy, id))?;
    state
        .claude_code
        .logout(key)
        .await
        .map_err(|e| failed(e, id))
}

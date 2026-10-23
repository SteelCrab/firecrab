//! Pipe AI has the same REST origin policy as other host operations.
use crate::{
    error::AppError,
    extract::ValidatedJson,
    pipe_ai::{self, Failure, SavedConnection},
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
};
use firecrab_api_types::{
    AiAuthentication, AiConnection, AiConnectionInput, AiLoginRequest, AiLoginStart, AiLoginStatus,
    AiModelSelection, AiProvider, AiResponse, AiResponseRequest, AiVerifyRequest,
};
use uuid::Uuid;

fn failed(failure: Failure, id: RequestId) -> AppError {
    let (status, code, message) = failure.details();
    AppError::new(status, code, message, id.0)
}

fn missing(id: RequestId) -> AppError {
    AppError::new(
        StatusCode::NOT_FOUND,
        "ai_connection_missing",
        "AI connection is disconnected or unknown",
        id.0,
    )
}

// Keep DB work off the async runtime. Errors never include SQL or private documents.
async fn stored<T: Send + 'static>(
    id: RequestId,
    work: impl FnOnce() -> Result<T, crate::persistence::PersistenceError> + Send + 'static,
) -> Result<T, AppError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| AppError::internal(id.0))?
        .map_err(|_| AppError::internal(id.0))
}

async fn connection(
    state: &AppState,
    key: Uuid,
    id: RequestId,
) -> Result<SavedConnection, AppError> {
    let store = state.store.clone();
    stored(id, move || store.ai_connection(key))
        .await?
        .ok_or_else(|| missing(id))
}

async fn refreshed(
    state: &AppState,
    key: Uuid,
    id: RequestId,
) -> Result<SavedConnection, AppError> {
    let mut saved = connection(state, key, id).await?;
    if saved.connection.provider == AiProvider::Anthropic {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            "ai_connection_only",
            "Anthropic supports API key connections only; model use belongs in AI Sandbox",
            id.0,
        ));
    }
    if state
        .pipe_ai
        .refresh(&mut saved)
        .await
        .map_err(|e| failed(e, id))?
    {
        let copy = saved.clone();
        let store = state.store.clone();
        stored(id, move || store.save_ai_connection(&copy)).await?;
    }
    Ok(saved)
}

fn require_model(saved: &SavedConnection, model: &str, id: RequestId) -> Result<(), AppError> {
    if !saved.connection.models.iter().any(|value| value == model) {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            "ai_model_unverified",
            "Verify this model before selecting it",
            id.0,
        ));
    }
    Ok(())
}

fn gate(state: &AppState, id: RequestId) -> Result<tokio::sync::MutexGuard<'_, ()>, AppError> {
    state.pipe_ai_mutations.try_lock().map_err(|_| {
        AppError::new(
            StatusCode::CONFLICT,
            "ai_busy",
            "Another AI operation is in progress; retry after it finishes",
            id.0,
        )
    })
}

pub async fn list(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
) -> Result<Json<Vec<AiConnection>>, AppError> {
    let store = state.store.clone();
    Ok(Json(
        stored(id, move || store.ai_connections())
            .await?
            .into_iter()
            .map(|v| v.connection)
            .collect(),
    ))
}

pub async fn save(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(key): Path<Uuid>,
    ValidatedJson(input): ValidatedJson<AiConnectionInput>,
) -> Result<Json<AiConnection>, AppError> {
    pipe_ai::validate_connection(&input).map_err(|e| failed(e, id))?;
    let _guard = gate(&state, id)?;
    if input.provider == AiProvider::Anthropic {
        state
            .pipe_ai_anthropic
            .verify_credentials(&input.api_key)
            .await
            .map_err(|e| failed(e, id))?;
    } else if let Some(model) = &input.model {
        state
            .pipe_ai
            .respond(&input.api_key, model, "Reply with OK.")
            .await
            .map_err(|e| failed(e, id))?;
    } else {
        state
            .pipe_ai
            .verify_credentials(&input.api_key)
            .await
            .map_err(|e| failed(e, id))?;
    }
    let saved = SavedConnection {
        connection: AiConnection {
            id: key,
            name: input.name.trim().into(),
            provider: input.provider,
            authentication: AiAuthentication::ApiKey,
            endpoint: match input.provider {
                AiProvider::Openai => pipe_ai::ENDPOINT,
                AiProvider::Anthropic => pipe_ai::anthropic::CREDENTIAL_ENDPOINT,
            }
            .into(),
            models: input.model.into_iter().collect(),
            streaming: false,
            tool_calls: false,
        },
        api_key: input.api_key,
        oauth: None,
    };
    let result = saved.connection.clone();
    let store = state.store.clone();
    stored(id, move || store.save_ai_connection(&saved)).await?;
    state.pipe_ai_logins.cancel_connection(key);
    Ok(Json(result))
}

pub async fn verify(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(key): Path<Uuid>,
    ValidatedJson(input): ValidatedJson<AiVerifyRequest>,
) -> Result<Json<AiConnection>, AppError> {
    let _guard = gate(&state, id)?;
    let mut saved = refreshed(&state, key, id).await?;
    if saved.redact(&input.model) != input.model {
        return Err(failed(Failure::InvalidInput, id));
    }
    state
        .pipe_ai
        .infer(
            &saved.api_key,
            &input.model,
            "Reply with OK.",
            saved.oauth.is_some(),
        )
        .await
        .map_err(|e| failed(e, id))?;
    if !saved.connection.models.contains(&input.model) {
        saved.connection.models.push(input.model);
    }
    let result = saved.connection.clone();
    let store = state.store.clone();
    stored(id, move || store.save_ai_connection(&saved)).await?;
    Ok(Json(result))
}

pub async fn disconnect(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(key): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let _guard = gate(&state, id)?;
    let store = state.store.clone();
    let saved = stored(id, move || store.ai_connection(key)).await?;
    state.pipe_ai_logins.cancel_connection(key);
    let store = state.store.clone();
    stored(id, move || store.delete_ai_connection(key)).await?;
    let remote = if let Some(session) = saved.as_ref().and_then(|s| s.oauth.as_ref()) {
        Some(state.pipe_ai.revoke(session).await)
    } else {
        None
    };
    Ok(Json(
        serde_json::json!({ "deleted": true, "remoteRevocationConfirmed": remote }),
    ))
}

pub async fn get_default(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(project): Path<Uuid>,
) -> Result<Json<Option<AiModelSelection>>, AppError> {
    let store = state.store.clone();
    Ok(Json(stored(id, move || store.ai_default(project)).await?))
}

pub async fn set_default(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(project): Path<Uuid>,
    ValidatedJson(selection): ValidatedJson<AiModelSelection>,
) -> Result<Json<AiModelSelection>, AppError> {
    let _guard = gate(&state, id)?;
    let saved = refreshed(&state, selection.connection_id, id).await?;
    require_model(&saved, &selection.model, id)?;
    let result = selection.clone();
    let store = state.store.clone();
    stored(id, move || store.save_ai_default(project, &selection)).await?;
    Ok(Json(result))
}

pub async fn respond(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    ValidatedJson(input): ValidatedJson<AiResponseRequest>,
) -> Result<Json<AiResponse>, AppError> {
    let _guard = gate(&state, id)?;
    let selection = match input.selection {
        Some(selection) => selection,
        None => {
            let store = state.store.clone();
            stored(id, move || store.ai_default(input.project_id))
                .await?
                .ok_or_else(|| {
                    AppError::new(
                        StatusCode::CONFLICT,
                        "ai_default_missing",
                        "Select a project default or request-specific model",
                        id.0,
                    )
                })?
        }
    };
    let saved = refreshed(&state, selection.connection_id, id).await?;
    require_model(&saved, &selection.model, id)?;
    let prompt = saved.redact(&input.input);
    let text = state
        .pipe_ai
        .infer(
            &saved.api_key,
            &selection.model,
            &prompt,
            saved.oauth.is_some(),
        )
        .await
        .map_err(|e| failed(e, id))?;
    let text = saved.redact(&text);
    Ok(Json(AiResponse { selection, text }))
}

pub async fn login_start(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(key): Path<Uuid>,
    headers: HeaderMap,
    ValidatedJson(input): ValidatedJson<AiLoginRequest>,
) -> Result<Json<AiLoginStart>, AppError> {
    let _guard = gate(&state, id)?;
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if host != format!("127.0.0.1:{}", state.api_port)
        && host != format!("localhost:{}", state.api_port)
    {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "ai_loopback_required",
            "ChatGPT login needs the local API through 127.0.0.1 or localhost",
            id.0,
        ));
    }
    let store = state.store.clone();
    let (host_id, registration) = stored(id, move || {
        Ok((store.ai_host_id()?, store.ai_registration(key)?))
    })
    .await?;
    let login = state
        .pipe_ai_logins
        .begin(key, input, host_id, state.api_port, registration.as_ref())
        .map_err(|e| failed(e, id))?;
    Ok(Json(login))
}

pub async fn login_status(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(key): Path<Uuid>,
) -> Result<Json<AiLoginStatus>, AppError> {
    Ok(Json(
        state
            .pipe_ai_logins
            .status(key)
            .map_err(|e| failed(e, id))?,
    ))
}

#[derive(serde::Deserialize)]
pub struct LoginCallback {
    state: Uuid,
    code: Option<String>,
    client_id: Option<String>,
    error: Option<String>,
}

pub async fn login_callback(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Query(callback): Query<LoginCallback>,
) -> Result<Response, AppError> {
    let _guard = gate(&state, id)?;
    let pending = state
        .pipe_ai_logins
        .take(callback.state)
        .map_err(|e| failed(e, id))?;
    let result = async {
        if callback.error.is_some() {
            return Err(Failure::Consent);
        }
        let code = callback
            .code
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(Failure::Login)?;
        let (access_token, session) = state
            .pipe_ai
            .exchange(&pending, code, callback.client_id.as_deref())
            .await?;
        if let Some(model) = &pending.input.model
            && let Err(error) = state
                .pipe_ai
                .infer(&access_token, model, "Reply with OK.", true)
                .await
        {
            state.pipe_ai.revoke(&session).await;
            return Err(error);
        }
        Ok(SavedConnection {
            connection: AiConnection {
                id: pending.connection_id,
                name: pending.input.name.trim().into(),
                provider: AiProvider::Openai,
                authentication: AiAuthentication::Chatgpt,
                endpoint: pipe_ai::ENDPOINT.into(),
                models: pending.input.model.iter().cloned().collect(),
                streaming: false,
                tool_calls: false,
            },
            api_key: access_token,
            oauth: Some(session),
        })
    };
    let result = match tokio::time::timeout(std::time::Duration::from_secs(50), result).await {
        Ok(Ok(saved)) => {
            let store = state.store.clone();
            stored(id, move || store.save_ai_connection(&saved))
                .await
                .map_err(|_| Failure::Provider)
        }
        Ok(Err(error)) => Err(error),
        Err(_) => Err(Failure::Timeout),
    };
    state.pipe_ai_logins.finish(callback.state, result);
    let message = if result.is_ok() {
        "ChatGPT account connected. Return to Firecrab."
    } else {
        "ChatGPT connection was not saved. Return to Firecrab for details."
    };
    Ok((
        [
            ("cache-control", "no-store"),
            ("referrer-policy", "no-referrer"),
            ("content-security-policy", "default-src 'none'"),
        ],
        Html(message),
    )
        .into_response())
}

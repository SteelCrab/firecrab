//! Credential checks are distinct from model entitlement and an agent's runtime authentication.
use super::{database, operator};
use crate::{
    error::AppError,
    extract::ValidatedJson,
    persistence::code_verification::Snapshot,
    pipe_ai::{self, Failure},
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use firecrab_api_types::{
    AiAuthentication, AiProvider, CodeAiVerification as Ai, CodeAuthenticationMethod as Method,
    CodeRepositoryVerification as Repository, CodeVerificationFailure as Reason,
    CodeWorkspaceVerification, CodeWorkspaceVerify,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn changed(id: RequestId) -> AppError {
    AppError::conflict(
        "code_workspace_bindings_changed",
        "Configuration or accounts changed during verification; retry with the current revision",
        id.0,
    )
}

fn provider_failure(failure: Failure) -> Ai {
    Ai::Failed {
        reason: match failure {
            Failure::Authentication | Failure::Login => Reason::AuthenticationRejected,
            Failure::Consent => Reason::ConsentRequired,
            Failure::RateLimited => Reason::RateLimited,
            Failure::Timeout => Reason::TimedOut,
            _ => Reason::Unavailable,
        },
    }
}

async fn ai(state: &AppState, snapshot: &mut Snapshot, id: RequestId) -> Result<Ai, AppError> {
    let Some(binding) = &snapshot.workspace.spec.agent else {
        return Ok(Ai::NotRequired);
    };
    let Some(mut saved) = snapshot.ai.clone() else {
        return Ok(Ai::Failed {
            reason: Reason::Missing,
        });
    };
    if !binding.agent.provider_compatible(&saved.connection) {
        return Ok(Ai::Failed {
            reason: Reason::Incompatible,
        });
    }
    let method = match (
        saved.connection.provider,
        saved.connection.authentication,
        saved.oauth.is_some(),
    ) {
        (AiProvider::Openai, AiAuthentication::ApiKey, false)
            if saved.connection.endpoint == pipe_ai::ENDPOINT =>
        {
            Method::OpenaiApiKey
        }
        (AiProvider::Openai, AiAuthentication::Chatgpt, true)
            if saved.connection.endpoint == pipe_ai::ENDPOINT =>
        {
            Method::ChatgptPlan
        }
        (AiProvider::Anthropic, AiAuthentication::ApiKey, false)
            if saved.connection.endpoint == pipe_ai::anthropic::CREDENTIAL_ENDPOINT =>
        {
            Method::AnthropicApiKey
        }
        _ => {
            return Ok(Ai::Failed {
                reason: Reason::Incompatible,
            });
        }
    };
    if method == Method::ChatgptPlan {
        let session = saved.oauth.as_ref().expect("ChatGPT session");
        if !["resource.invoke", "chatgpt.tokens.use.direct"]
            .iter()
            .all(|required| {
                session
                    .scope
                    .split_whitespace()
                    .any(|scope| scope == *required)
            })
        {
            return Ok(Ai::Failed {
                reason: Reason::ConsentRequired,
            });
        }
        if session.client_id.is_empty()
            || session.subject.is_empty()
            || session.refresh_token.is_empty()
        {
            return Ok(Ai::Failed {
                reason: Reason::AuthenticationRejected,
            });
        }
        match state.pipe_ai.refresh(&mut saved).await {
            Err(failure) => return Ok(provider_failure(failure)),
            Ok(true) => {
                let mut previous = snapshot.clone();
                let store = state.store.clone();
                let (updated, written) = database(id, move || {
                    let written = store.refresh_code_ai(&mut previous, saved)?;
                    Ok((previous, written))
                })
                .await?;
                if !written {
                    return Err(changed(id));
                }
                *snapshot = updated;
                saved = snapshot.ai.clone().expect("refreshed ChatGPT session");
            }
            Ok(false) => {}
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if saved
            .oauth
            .as_ref()
            .is_none_or(|session| session.expires_at <= now)
        {
            return Ok(Ai::Failed {
                reason: Reason::AuthenticationRejected,
            });
        }
        if saved.oauth.as_ref().is_none_or(|session| {
            !["resource.invoke", "chatgpt.tokens.use.direct"]
                .iter()
                .all(|required| {
                    session
                        .scope
                        .split_whitespace()
                        .any(|scope| scope == *required)
                })
        }) {
            return Ok(Ai::Failed {
                reason: Reason::ConsentRequired,
            });
        }
    }
    if saved.api_key.is_empty()
        || saved.api_key.len() > 16384
        || !saved.api_key.bytes().all(|c| c.is_ascii_graphic())
    {
        return Ok(Ai::Failed {
            reason: Reason::AuthenticationRejected,
        });
    }
    let result = match method {
        Method::OpenaiApiKey => state.pipe_ai.verify_credentials(&saved.api_key).await,
        Method::ChatgptPlan => {
            state
                .pipe_ai
                .verify_chatgpt_credentials(&saved.api_key)
                .await
        }
        Method::AnthropicApiKey => {
            state
                .pipe_ai_anthropic
                .verify_credentials(&saved.api_key)
                .await
        }
    };
    Ok(match result {
        Ok(()) => Ai::CredentialsVerified { method },
        Err(failure) => provider_failure(failure),
    })
}

pub async fn verify(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: crate::ai_sandbox::access::Caller,
    Path(key): Path<String>,
    ValidatedJson(input): ValidatedJson<CodeWorkspaceVerify>,
) -> Result<Json<CodeWorkspaceVerification>, AppError> {
    operator(caller, id)?;
    let key = crate::handlers::vms::parse_id(&key, id.0)?;
    if input.expected_revision == 0 || input.expected_revision > i64::MAX as u64 {
        return Err(AppError::validation(
            [(
                "expectedRevision".into(),
                "use a positive saved workspace revision".into(),
            )]
            .into(),
            id.0,
        ));
    }
    // Same gate as AUTH refresh, account replacement and disconnect. Database CAS also
    // handles other processes and direct store writers, which do not share this mutex.
    let _guard = state.pipe_ai_mutations.try_lock().map_err(|_| {
        AppError::conflict(
            "ai_busy",
            "Another AI operation is in progress; retry after it finishes",
            id.0,
        )
    })?;
    let store = state.store.clone();
    let mut snapshot = database(id, move || store.code_verification_snapshot(key))
        .await?
        .ok_or_else(|| AppError::not_found(id.0))?;
    if snapshot.workspace.revision != input.expected_revision {
        return Err(super::conflict(id));
    }
    let github = tokio::time::timeout(
        Duration::from_secs(20),
        state
            .code_github
            .verify(&snapshot.workspace.spec.github, snapshot.github.as_ref()),
    )
    .await
    .unwrap_or(Repository::Failed {
        reason: Reason::TimedOut,
    });
    let ai = tokio::time::timeout(Duration::from_secs(25), ai(&state, &mut snapshot, id))
        .await
        .unwrap_or(Ok(Ai::Failed {
            reason: Reason::TimedOut,
        }))?;
    let store = state.store.clone();
    let revision = snapshot.workspace.revision;
    if !database(id, move || store.code_verification_current(&snapshot)).await? {
        return Err(changed(id));
    }
    Ok(Json(CodeWorkspaceVerification {
        id: key,
        revision,
        checked_at_ms: chrono::Utc::now().timestamp_millis(),
        github,
        ai,
    }))
}

//! Operator-only discovery. Recheck the exact private account snapshot after remote IO.
use super::{database, operator};
use crate::{
    ai_sandbox::access::Caller, error::AppError, pipe_auth::SavedAuth, server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json,
    extract::{Query, State, rejection::QueryRejection},
    http::StatusCode,
};
use firecrab_api_types::{
    CodeBranchPage, CodeGithubRepository, CodeRepositoryPage, CodeVerificationFailure,
};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogQuery {
    account: String,
    #[serde(default = "first")]
    page: u32,
    owner: Option<String>,
    repository: Option<String>,
}
fn first() -> u32 {
    1
}
fn bad_query(id: RequestId) -> AppError {
    AppError::new(
        StatusCode::BAD_REQUEST,
        "invalid_query",
        "Use a GitHub account, page 1–1000 and explicit repository identifiers",
        id.0,
    )
}
fn parse(
    query: Result<Query<CatalogQuery>, QueryRejection>,
    branches: bool,
    id: RequestId,
) -> Result<CatalogQuery, AppError> {
    let Query(query) = query.map_err(|_| bad_query(id))?;
    let spec = CodeGithubRepository {
        account: query.account.clone(),
        owner: query.owner.clone().unwrap_or_else(|| query.account.clone()),
        repository: query.repository.clone().unwrap_or_else(|| "catalog".into()),
        branch: "main".into(),
    };
    if !(1..=1000).contains(&query.page)
        || !spec.validate().is_empty()
        || (branches && (query.owner.is_none() || query.repository.is_none()))
        || (!branches && (query.owner.is_some() || query.repository.is_some()))
    {
        return Err(bad_query(id));
    }
    Ok(query)
}
async fn snapshot(
    state: &AppState,
    id: RequestId,
) -> Result<(Option<String>, Option<SavedAuth>), AppError> {
    let store = state.store.clone();
    let raw = database(id, move || store.code_github_snapshot()).await?;
    let auth = raw
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| AppError::internal(id.0))?;
    Ok((raw, auth))
}
async fn current(state: &AppState, raw: Option<String>, id: RequestId) -> Result<(), AppError> {
    let store = state.store.clone();
    if database(id, move || Ok(store.code_github_snapshot()? == raw)).await? {
        return Ok(());
    }
    Err(AppError::conflict(
        "code_github_account_changed",
        "GitHub account changed; reload repository choices",
        id.0,
    ))
}
fn failure(reason: CodeVerificationFailure, id: RequestId) -> AppError {
    use CodeVerificationFailure::*;
    let code = match reason {
        Missing => "code_github_missing",
        IdentityChanged => "code_github_identity_changed",
        AuthenticationRejected => "code_github_authentication_rejected",
        AccessDenied => "code_github_access_denied",
        NotFound => "code_github_not_found",
        RateLimited => "code_github_rate_limited",
        Redirected => "code_github_redirected",
        InvalidResponse => "code_github_invalid_response",
        _ => "code_github_unavailable",
    };
    AppError::new(
        StatusCode::BAD_GATEWAY,
        code,
        "GitHub choices could not be loaded; check the connected account and retry",
        id.0,
    )
}
pub async fn repositories(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    query: Result<Query<CatalogQuery>, QueryRejection>,
) -> Result<Json<CodeRepositoryPage>, AppError> {
    operator(caller, id)?;
    let query = parse(query, false, id)?;
    let (raw, auth) = snapshot(&state, id).await?;
    let result = state
        .code_github
        .repositories(&query.account, auth.as_ref(), query.page)
        .await;
    current(&state, raw, id).await?;
    result.map(Json).map_err(|reason| failure(reason, id))
}
pub async fn branches(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    caller: Caller,
    query: Result<Query<CatalogQuery>, QueryRejection>,
) -> Result<Json<CodeBranchPage>, AppError> {
    operator(caller, id)?;
    let query = parse(query, true, id)?;
    let (raw, auth) = snapshot(&state, id).await?;
    let spec = CodeGithubRepository {
        account: query.account,
        owner: query.owner.unwrap(),
        repository: query.repository.unwrap(),
        branch: "main".into(),
    };
    let result = state
        .code_github
        .branches(&spec, auth.as_ref(), query.page)
        .await;
    current(&state, raw, id).await?;
    result.map(Json).map_err(|reason| failure(reason, id))
}

//! CI can replace one service's image, but cannot reach operator configuration or Secrets.
use crate::{
    ai_sandbox::access::Caller,
    error::AppError,
    extract::ValidatedJson,
    handlers::{
        micro_services::{database, ensure_stack_writer, require_image_auth},
        vms::parse_id,
    },
    oci::{ImageReference, ImageVersion},
    persistence::ci_deploy::Admission,
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json,
    extract::{FromRequestParts, Path, State},
    http::{StatusCode, header, request::Parts},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use firecrab_api_types::*;
use ring::rand::{SecureRandom, SystemRandom};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

/// The separate listener has no operator routes. Nginx forwards only the reserved service path.
pub(crate) const ADDRESS: &str = firecrab_helper_protocol::pipe::CI_DEPLOY_ADDRESS;

pub(crate) fn repository(image: &str) -> Option<String> {
    let reference = ImageReference::parse(image).ok()?;
    Some(format!("{}/{}", reference.registry, reference.repository))
}

pub(crate) fn pinned(image: &str) -> bool {
    image.len() <= 1024
        && ImageReference::parse(image).is_ok_and(|r| matches!(r.version, ImageVersion::Digest(_)))
        && repository(image).is_some_and(|repo| image.starts_with(&format!("{repo}@sha256:")))
}

fn unauthorized(id: Uuid) -> AppError {
    AppError::new(
        StatusCode::UNAUTHORIZED,
        "deploy_token_required",
        "A valid unexpired service Deploy token is required",
        id,
    )
}

/// Constructed only after a bearer secret has been verified against its saved hash.
pub(crate) struct Credential(pub DeployToken);

impl FromRequestParts<AppState> for Credential {
    type Rejection = AppError;
    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let request = parts
            .extensions
            .get::<RequestId>()
            .map_or_else(Uuid::new_v4, |r| r.0);
        if parts.headers.get_all(header::AUTHORIZATION).iter().count() != 1 {
            return Err(unauthorized(request));
        }
        let raw = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| unauthorized(request))?;
        if raw.len() != 80 {
            return Err(unauthorized(request));
        }
        let (id, secret) = raw
            .strip_prefix("fcd_")
            .and_then(|v| v.split_once('_'))
            .ok_or_else(|| unauthorized(request))?;
        if id.len() != 32
            || URL_SAFE_NO_PAD
                .decode(secret)
                .ok()
                .is_none_or(|v| v.len() != 32)
        {
            return Err(unauthorized(request));
        }
        let id = Uuid::parse_str(id).map_err(|_| unauthorized(request))?;
        let hash = Sha256::digest(raw.as_bytes());
        let store = state.store.clone();
        let saved = database(request, move || store.deploy_token(id))
            .await?
            .ok_or_else(|| unauthorized(request))?;
        if saved.metadata.expires_at <= chrono::Utc::now().timestamp()
            || saved.hash.len() != 32
            || !bool::from(saved.hash.as_slice().ct_eq(hash.as_slice()))
        {
            return Err(unauthorized(request));
        }
        Ok(Self(saved.metadata))
    }
}

impl Credential {
    fn service(&self, service: Uuid, request: Uuid) -> Result<(), AppError> {
        if self.0.service_id == service {
            return Ok(());
        }
        Err(AppError::new(
            StatusCode::FORBIDDEN,
            "deploy_token_scope",
            "The token cannot access this service",
            request,
        ))
    }
}

fn operator(caller: Caller, request: Uuid) -> Result<(), AppError> {
    caller.operator().map_err(|_| {
        AppError::new(
            StatusCode::FORBIDDEN,
            "deploy_operator_required",
            "Only the operator can manage service Deploy tokens",
            request,
        )
    })
}

pub async fn list_tokens(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
    Path(service): Path<String>,
) -> Result<Json<Vec<DeployToken>>, AppError> {
    operator(caller, request.0)?;
    let service = parse_id(&service, request.0)?;
    let store = state.store.clone();
    database(request.0, move || store.deploy_tokens(service))
        .await
        .map(Json)
}

pub async fn create_token(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
    Path(service): Path<String>,
    ValidatedJson(body): ValidatedJson<CreateDeployToken>,
) -> Result<(StatusCode, Json<CreatedDeployToken>), AppError> {
    operator(caller, request.0)?;
    let service = parse_id(&service, request.0)?;
    if body.name.trim().is_empty()
        || body.name.len() > 64
        || body.name.chars().any(char::is_control)
        || !(60..=90 * 86400).contains(&body.expires_in_seconds)
    {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "invalid_deploy_token",
            "Use a name of 1–64 bytes and a lifetime of 60 seconds to 90 days",
            request.0,
        ));
    }
    let claim = state.service_mutations.clone().lock_owned().await;
    ensure_stack_writer(&state, service, None, request.0).await?;
    ensure_local(&state, service, request.0).await?;
    let store = state.store.clone();
    let active = database(request.0, move || store.service_deployment(service))
        .await?
        .ok_or_else(|| AppError::not_found(request.0))?;
    if !matches!(active.phase, ServicePhase::Running | ServicePhase::Degraded)
        || !active.domain_route_owned
        || active.spec.domain.is_none()
        || active.update.as_ref().is_some_and(|u| !u.completed)
    {
        return Err(AppError::conflict(
            "service_not_running",
            "Start a domain service before issuing a CI Deploy token",
            request.0,
        ));
    }
    require_image_auth(&state, &active.spec.image, request.0).await?;
    let repository = repository(&active.spec.image).ok_or_else(|| AppError::internal(request.0))?;
    let mut secret = [0u8; 32];
    SystemRandom::new()
        .fill(&mut secret)
        .map_err(|_| AppError::internal(request.0))?;
    let now = chrono::Utc::now().timestamp();
    let credential = DeployToken {
        id: Uuid::new_v4(),
        service_id: service,
        name: body.name,
        repository,
        created_at: now,
        expires_at: now + i64::from(body.expires_in_seconds),
    };
    let token = format!(
        "fcd_{}_{}",
        credential.id.simple(),
        URL_SAFE_NO_PAD.encode(secret)
    );
    let hash = Sha256::digest(token.as_bytes()).to_vec();
    let saved = credential.clone();
    tokio::spawn(async move {
        let _claim = claim;
        database(request.0, move || {
            state.store.insert_deploy_token(&saved, &hash)
        })
        .await
    })
    .await
    .map_err(|_| AppError::internal(request.0))??;
    Ok((
        StatusCode::CREATED,
        Json(CreatedDeployToken { credential, token }),
    ))
}

pub async fn revoke_token(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
    Path((service, token)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    operator(caller, request.0)?;
    let service = parse_id(&service, request.0)?;
    let token = parse_id(&token, request.0)?;
    let claim = state.service_mutations.clone().lock_owned().await;
    tokio::spawn(async move {
        let _claim = claim;
        database(request.0, move || {
            state.store.revoke_deploy_token(service, token)
        })
        .await
    })
    .await
    .map_err(|_| AppError::internal(request.0))??;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn ensure_local(
    state: &AppState,
    service: Uuid,
    request: Uuid,
) -> Result<(), AppError> {
    let store = state.store.clone();
    if database(request, move || store.service_is_controller_owned(service)).await? {
        return Err(AppError::conflict(
            "controller_owned_service",
            "Use the owning controller for this service",
            request,
        ));
    }
    Ok(())
}

pub(crate) async fn authorize(
    state: &AppState,
    service: Uuid,
    admission: &Admission,
    request: Uuid,
) -> Result<DeployToken, AppError> {
    ensure_stack_writer(state, service, None, request).await?;
    ensure_local(state, service, request).await?;
    let id = admission.token_id;
    let store = state.store.clone();
    let token = database(request, move || store.deploy_token(id))
        .await?
        .ok_or_else(|| unauthorized(request))?
        .metadata;
    if token.service_id != service || token.expires_at <= chrono::Utc::now().timestamp() {
        return Err(unauthorized(request));
    }
    if !pinned(&admission.request.image)
        || repository(&admission.request.image).as_deref() != Some(token.repository.as_str())
    {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "deploy_image_scope",
            "Use a sha256-pinned image from the token's exact repository",
            request,
        ));
    }
    Ok(token)
}

pub async fn service_status(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    credential: Credential,
    Path(service): Path<String>,
) -> Result<Json<CiServiceStatus>, AppError> {
    let service = parse_id(&service, request.0)?;
    credential.service(service, request.0)?;
    let store = state.store.clone();
    let active = database(request.0, move || store.service_deployment(service))
        .await?
        .ok_or_else(|| AppError::not_found(request.0))?;
    Ok(Json(CiServiceStatus {
        service_id: service,
        release_id: active.id,
        image: active.spec.image,
        phase: active.phase,
    }))
}

pub async fn submit(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    credential: Credential,
    Path(service): Path<String>,
    ValidatedJson(body): ValidatedJson<CiDeployRequest>,
) -> Result<(StatusCode, Json<CiDeployStatus>), AppError> {
    let service = parse_id(&service, request.0)?;
    credential.service(service, request.0)?;
    if body.operation_id.is_nil() || body.expected_release_id.is_nil() {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "invalid_deploy_identity",
            "operationId and expectedReleaseId must be non-nil UUIDs",
            request.0,
        ));
    }
    let admission = Admission {
        token_id: credential.0.id,
        request: body.clone(),
    };
    let token = admission.token_id;
    let record =
        crate::service_runtime::deploy::from_ci(state.clone(), request, service, admission).await?;
    let store = state.store.clone();
    let operation = body.operation_id;
    let receipt = database(request.0, move || {
        store.ci_result(service, token, operation)
    })
    .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(receipt.unwrap_or_else(|| outcome(&record, &body))),
    ))
}

pub async fn operation_status(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    credential: Credential,
    Path((service, operation)): Path<(String, String)>,
) -> Result<Json<CiDeployStatus>, AppError> {
    let service = parse_id(&service, request.0)?;
    let operation = parse_id(&operation, request.0)?;
    credential.service(service, request.0)?;
    let token = credential.0.id;
    let store = state.store.clone();
    if let Some(receipt) = database(request.0, move || {
        store.ci_result(service, token, operation)
    })
    .await?
    {
        return Ok(Json(receipt));
    }
    let store = state.store.clone();
    let body = database(request.0, move || {
        store.ci_request(service, token, operation)
    })
    .await?
    .ok_or_else(|| AppError::not_found(request.0))?;
    let store = state.store.clone();
    let record = database(request.0, move || store.service_attempt(operation))
        .await?
        .filter(|r| r.service_id == service)
        .ok_or_else(|| AppError::not_found(request.0))?;
    Ok(Json(outcome(&record, &body)))
}

pub(crate) fn outcome(record: &MicroServiceDeployment, body: &CiDeployRequest) -> CiDeployStatus {
    let succeeded = matches!(record.phase, ServicePhase::Running | ServicePhase::Degraded)
        && record
            .update
            .as_ref()
            .is_some_and(|u| u.completed && u.failure.is_none() && !u.cancelled);
    CiDeployStatus {
        service_id: record.service_id,
        operation_id: record.id,
        expected_release_id: body.expected_release_id,
        image: body.image.clone(),
        strategy: body.strategy,
        phase: record.phase,
        completed: succeeded
            || matches!(
                record.phase,
                ServicePhase::Failed | ServicePhase::Cancelled | ServicePhase::Stopped
            ),
        succeeded,
        digest: record.digest.clone(),
    }
}

#[cfg(test)]
mod tests;

//! Serialize public configuration edits with Stack admission and active reservations.

use axum::extract::{FromRequestParts, MatchedPath, Path, Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use firecrab_stack::ManagedResource;
use std::collections::HashMap;
use uuid::Uuid;

use crate::error::AppError;
use crate::server::RequestId;
use crate::state::AppState;

/// Internal Stack execution calls resource functions directly, never this HTTP gate.
/// Public callers cannot authorize a write with a request-ID or operation-ID header.
pub(crate) async fn protect_resources(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) {
        return next.run(request).await;
    }
    let Some(path) = request.extensions().get::<MatchedPath>() else {
        return next.run(request).await;
    };
    if path.as_str() == "/api/vms/{id}/ssh-host-key/check" {
        return next.run(request).await;
    }
    let configuration = match path.as_str() {
        "/api/pools/{id}" => Some("pool"),
        "/api/queues/{id}" | "/api/queues/{id}/retry" => Some("queue"),
        "/api/pipe/databases/{id}" | "/api/pipe/databases/{id}/access" => {
            Some("database_connection")
        }
        "/api/pipe/storage/{id}" => Some("storage_connection"),
        "/api/secrets/{id}" => Some("secret"),
        "/api/secrets/connections/{id}" => Some("secret_connection"),
        "/api/pipe/auth/{provider}" => Some("auth"),
        "/api/pipe/registries/{registry}" | "/api/microregistry/docker-hub" => Some("registry"),
        "/api/pipe/ai/connections/{id}"
        | "/api/pipe/ai/connections/{id}/verify"
        | "/api/pipe/ai/connections/{id}/login" => Some("ai_connection"),
        "/api/pipe/ai/projects/{id}/model" => Some("ai_model"),
        "/api/ai-sandbox/projects/{id}" => Some("sandbox_project"),
        "/api/ai-sandbox/projects/{project}/environments/{environment}"
        | "/api/ai-sandbox/projects/{project}/environments/{environment}/reconcile"
        | "/api/ai-sandbox/projects/{project}/environments/{environment}/stop"
        | "/api/ai-sandbox/projects/{project}/environments/{environment}/resume" => {
            Some("sandbox_environment")
        }
        _ => None,
    };
    let kind = configuration.or_else(|| {
        [
            ("/api/micro-networks/{id}", "network"),
            ("/api/vms/{id}", "vm"),
            ("/api/micro-storages/{id}", "storage_root"),
            ("/api/microblockstorage/{id}", "volume"),
            ("/api/security-groups/{id}", "security_group"),
            ("/api/shells/{id}", "shell"),
            ("/api/schedules/{id}", "schedule"),
        ]
        .into_iter()
        .find_map(|(prefix, kind)| path.as_str().starts_with(prefix).then_some(kind))
    });
    let Some(kind) = kind else {
        return next.run(request).await;
    };
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(|id| id.0)
        .unwrap_or_else(Uuid::new_v4);
    let docker_hub = path.as_str() == "/api/microregistry/docker-hub";
    // Use decoded identities so percent encoding cannot bypass the ownership check.
    let (mut parts, body) = request.into_parts();
    let id = if docker_hub {
        "docker.io".into()
    } else {
        match Path::<HashMap<String, String>>::from_request_parts(&mut parts, &state).await {
            Ok(Path(parameters)) if kind == "registry" => {
                let registry = parameters
                    .get("registry")
                    .map(|value| value.trim().to_ascii_lowercase())
                    .unwrap_or_default();
                if registry == "registry-1.docker.io" {
                    "docker.io".into()
                } else {
                    registry
                }
            }
            Ok(Path(parameters)) if kind == "auth" => {
                parameters.get("provider").cloned().unwrap_or_default()
            }
            Ok(Path(parameters)) => match super::super::vms::parse_id(
                parameters
                    .get(if kind == "sandbox_environment" {
                        "environment"
                    } else {
                        "id"
                    })
                    .map(String::as_str)
                    .unwrap_or_default(),
                request_id,
            ) {
                Ok(id) => id.to_string(),
                Err(error) => return error.into_response(),
            },
            Err(_) => return AppError::not_found(request_id).into_response(),
        }
    };
    // Admission cannot race between this check and a synchronous resource edit.
    // Detached runtime work remains subject to the existing resource-state checks.
    let _admission = state.stack_apply_admission.lock().await;
    let store = state.store.clone();
    match super::database(request_id, move || {
        store.iac_public_write_allowed(&ManagedResource {
            kind: kind.into(),
            id,
        })
    })
    .await
    {
        Ok(false) => crate::handlers::iac::managed(request_id).into_response(),
        Ok(true) => next.run(Request::from_parts(parts, body)).await,
        Err(error) => error.into_response(),
    }
}

#[cfg(test)]
mod tests;

//! Serialize public configuration edits with Stack admission and active reservations.

use axum::extract::{FromRequestParts, MatchedPath, Path, Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use firecrab_stack::ResourceKind;
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
    let kind = if path.as_str().starts_with("/api/micro-networks/{id}") {
        ResourceKind::Network
    } else if path.as_str().starts_with("/api/vms/{id}")
        && path.as_str() != "/api/vms/{id}/ssh-host-key/check"
    {
        ResourceKind::Vm
    } else {
        return next.run(request).await;
    };
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(|id| id.0)
        .unwrap_or_else(Uuid::new_v4);
    // Use the router's decoded parameter, not the raw URI. Percent-encoded UUID
    // characters must not provide a second route around the ownership check.
    let (mut parts, body) = request.into_parts();
    let id = match Path::<String>::from_request_parts(&mut parts, &state).await {
        Ok(Path(id)) => match super::super::vms::parse_id(&id, request_id) {
            Ok(id) => id,
            Err(error) => return error.into_response(),
        },
        Err(_) => return AppError::not_found(request_id).into_response(),
    };
    // Admission cannot race between this check and a synchronous resource edit.
    // Detached runtime work remains subject to the existing resource-state checks.
    let _admission = state.stack_apply_admission.lock().await;
    let store = state.store.clone();
    match super::database(request_id, move || {
        store.stack_resource_apply_busy(kind, id)
    })
    .await
    {
        Ok(false) => next.run(Request::from_parts(parts, body)).await,
        Ok(true) => AppError::conflict(
            "stack_apply_busy",
            "wait for the owning stack apply before changing this resource",
            request_id,
        )
        .into_response(),
        Err(error) => error.into_response(),
    }
}

#[cfg(test)]
mod tests;

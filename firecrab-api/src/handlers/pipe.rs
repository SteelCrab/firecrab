//! Dashboard controls use the privileged helper and retain systemd ownership.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use firecrab_helper_protocol::pipe::{
    ApplyRequest, Operation, QuickConnectRequest, QuickTunnelRequest, QuickTunnelSaveRequest,
    ServiceAction, ServiceRequest, Snapshot,
};

use crate::error::AppError;
use crate::network::{NetworkClient, NetworkError};
use crate::server::RequestId;
use crate::state::AppState;

/// Send a fixed Pipe operation to the helper and hide internal errors from clients.
pub(super) async fn call(
    client: &NetworkClient,
    operation: Operation,
    request_id: RequestId,
) -> Result<Json<Snapshot>, AppError> {
    let firecrab = matches!(&operation, Operation::ManageFirecrabTunnel { .. });
    client.pipe(operation).await.map(Json).map_err(|error| {
        tracing::warn!(%error, "pipe helper command failed");
        if firecrab && let Some(response) = firecrab_error(&error, request_id.0) {
            return response;
        }
        AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "pipe_unavailable",
            "Pipe operation failed; check the helper and installed engines",
            request_id.0,
        )
    })
}

// Only fixed, non-secret connection errors may cross the helper boundary.
// Unrecognized command output retains the generic failure response.
fn firecrab_error(error: &NetworkError, request_id: uuid::Uuid) -> Option<AppError> {
    use firecrab_helper_protocol::network::HelperFailure;
    let NetworkError::Helper(HelperFailure::Internal { detail }) = error else {
        return None;
    };
    let detail = detail.trim();
    let detail = detail.strip_prefix("[ERROR] ").unwrap_or(detail);
    for (message, status) in [
        (
            "The connection token is invalid or revoked",
            StatusCode::BAD_REQUEST,
        ),
        (
            "Selected Quick Tunnel no longer exists",
            StatusCode::BAD_REQUEST,
        ),
        (
            "Selected Nginx route no longer exists",
            StatusCode::BAD_REQUEST,
        ),
        (
            "The Firecrab address is suspended; check the dashboard",
            StatusCode::FORBIDDEN,
        ),
        (
            "Token verification is rate limited; retry later",
            StatusCode::TOO_MANY_REQUESTS,
        ),
        (
            "Cannot verify the token with the Firecrab connection service",
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            "Firecrab connection API is unavailable; check the service deployment",
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            "The Firecrab connection service could not verify the token",
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            "Invalid token verification response",
            StatusCode::BAD_GATEWAY,
        ),
    ] {
        if detail == message {
            return Some(AppError::new(
                status,
                "firecrab_connection_failed",
                message,
                request_id,
            ));
        }
    }
    None
}

/// Return the persisted manifest and live independent-unit states.
pub async fn inspect(
    Extension(request_id): Extension<RequestId>,
) -> Result<Json<Snapshot>, AppError> {
    call(&NetworkClient::from_env(), Operation::Inspect, request_id).await
}

/// Validate manifest and replacement credentials before requesting a privileged apply.
pub async fn apply(
    Extension(request_id): Extension<RequestId>,
    Json(request): Json<ApplyRequest>,
) -> Result<Json<Snapshot>, AppError> {
    request.config.validate().map_err(|error| {
        AppError::validation(
            std::collections::BTreeMap::from([("config".into(), error.to_string())]),
            request_id.0,
        )
    })?;
    if let Some(credentials) = &request.credentials {
        request
            .config
            .validate_credentials(credentials)
            .map_err(|error| {
                AppError::validation(
                    std::collections::BTreeMap::from([("credentials".into(), error.to_string())]),
                    request_id.0,
                )
            })?;
    }
    if serde_json::to_vec(&request).map_or(true, |bytes| bytes.len() > 48 * 1024) {
        return Err(AppError::validation(
            std::collections::BTreeMap::from([(
                "config".into(),
                "Pipe request exceeds 48 KiB".into(),
            )]),
            request_id.0,
        ));
    }
    call(
        &NetworkClient::from_env(),
        Operation::Apply { request },
        request_id,
    )
    .await
}

/// Validate anonymous app selection before installing and starting its connector.
pub async fn connect_quick_tunnel(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Json(request): Json<QuickConnectRequest>,
) -> Result<Json<Snapshot>, AppError> {
    request.validate().map_err(|error| {
        AppError::validation(
            std::collections::BTreeMap::from([("quickTunnel".into(), error.to_string())]),
            request_id.0,
        )
    })?;
    call(
        &state.network,
        Operation::ConnectQuickTunnel { request },
        request_id,
    )
    .await
}

/// One fixed runtime action for a connector selected by its resource path.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuickTunnelServiceRequest {
    /// Requested lifecycle operation; service identifiers cannot be supplied.
    action: ServiceAction,
}

async fn manage_quick_tunnel(
    state: AppState,
    request_id: RequestId,
    request: QuickTunnelRequest,
) -> Result<Json<Snapshot>, AppError> {
    request.validate().map_err(|error| {
        AppError::validation(
            std::collections::BTreeMap::from([("quickTunnel".into(), error.to_string())]),
            request_id.0,
        )
    })?;
    call(
        &state.network,
        Operation::ManageQuickTunnel { request },
        request_id,
    )
    .await
}

/// Save one connector against the helper's latest configuration.
pub async fn save_quick_tunnel(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Json(request): Json<QuickTunnelSaveRequest>,
) -> Result<Json<Snapshot>, AppError> {
    manage_quick_tunnel(
        state,
        request_id,
        QuickTunnelRequest::Save {
            id: request.id,
            name: request.name,
            target: request.target,
        },
    )
    .await
}

/// Remove one connector without submitting or overwriting unrelated Pipe routes.
pub async fn delete_quick_tunnel(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Extension(request_id): Extension<RequestId>,
) -> Result<Json<Snapshot>, AppError> {
    manage_quick_tunnel(state, request_id, QuickTunnelRequest::Delete { id }).await
}

/// Queue a fixed lifecycle action for one connector.
pub async fn control_quick_tunnel(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Extension(request_id): Extension<RequestId>,
    Json(request): Json<QuickTunnelServiceRequest>,
) -> Result<Json<Snapshot>, AppError> {
    manage_quick_tunnel(
        state,
        request_id,
        QuickTunnelRequest::Control {
            id,
            service_action: request.action,
        },
    )
    .await
}

/// Connect with a dashboard token, or edit an address whose secret is already saved.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirecrabSaveRequest {
    #[serde(default)]
    subdomain: Option<String>,
    #[serde(default)]
    token: Option<firecrab_helper_protocol::firecrab_tunnel::ConnectionToken>,
    target: firecrab_helper_protocol::firecrab_tunnel::Target,
}

/// Forward the connection secret only to the privileged helper; never return it.
pub async fn save_firecrab_tunnel(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Json(request): Json<FirecrabSaveRequest>,
) -> Result<Json<Snapshot>, AppError> {
    use firecrab_helper_protocol::firecrab_tunnel::Request;
    let intent = match (request.token, request.subdomain) {
        (Some(token), None) => Request::Connect {
            token,
            target: request.target,
        },
        (None, Some(subdomain)) => Request::Save {
            subdomain,
            target: request.target,
        },
        _ => {
            return Err(AppError::validation(
                std::collections::BTreeMap::from([(
                    "token".into(),
                    "Provide a connection token or select a saved address to edit".into(),
                )]),
                request_id.0,
            ));
        }
    };
    firecrab_request(state, request_id, intent).await
}
/// Remove one owned address and its exclusively managed connector.
pub async fn delete_firecrab_tunnel(
    State(state): State<AppState>,
    Path(subdomain): Path<String>,
    Extension(request_id): Extension<RequestId>,
) -> Result<Json<Snapshot>, AppError> {
    firecrab_request(
        state,
        request_id,
        firecrab_helper_protocol::firecrab_tunnel::Request::Delete { subdomain },
    )
    .await
}
async fn firecrab_request(
    state: AppState,
    request_id: RequestId,
    request: firecrab_helper_protocol::firecrab_tunnel::Request,
) -> Result<Json<Snapshot>, AppError> {
    request.validate().map_err(|error| {
        AppError::validation(
            std::collections::BTreeMap::from([("firecrabTunnel".into(), error)]),
            request_id.0,
        )
    })?;
    call(
        &state.network,
        Operation::ManageFirecrabTunnel { request },
        request_id,
    )
    .await
}

/// Request one fixed systemd service action while retaining configuration.
pub async fn control(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Json(request): Json<ServiceRequest>,
) -> Result<Json<Snapshot>, AppError> {
    call(&state.network, Operation::Control { request }, request_id).await
}

/// Stop all data-plane units through the helper while retaining configuration.
pub async fn disable(
    Extension(request_id): Extension<RequestId>,
) -> Result<Json<Snapshot>, AppError> {
    call(&NetworkClient::from_env(), Operation::Disable, request_id).await
}

#[cfg(test)]
#[path = "pipe_tests.rs"]
mod tests;

//! Dashboard controls use the privileged helper and retain systemd ownership.

use axum::http::StatusCode;
use axum::{Extension, Json};
use firecrab_helper_protocol::pipe::{ApplyRequest, Operation, Snapshot};

use crate::error::AppError;
use crate::network::NetworkClient;
use crate::server::RequestId;

/// Send a fixed Pipe operation to the helper and hide internal errors from clients.
async fn call(operation: Operation, request_id: RequestId) -> Result<Json<Snapshot>, AppError> {
    NetworkClient::from_env()
        .pipe(operation)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "pipe helper command failed");
            AppError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "pipe_unavailable",
                "Pipe operation failed; check the helper and installed engines",
                request_id.0,
            )
        })
}

/// Return the persisted manifest and live independent-unit states.
pub async fn inspect(
    Extension(request_id): Extension<RequestId>,
) -> Result<Json<Snapshot>, AppError> {
    call(Operation::Inspect, request_id).await
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
    call(Operation::Apply { request }, request_id).await
}

/// Stop both data-plane units through the helper while retaining configuration.
pub async fn disable(
    Extension(request_id): Extension<RequestId>,
) -> Result<Json<Snapshot>, AppError> {
    call(Operation::Disable, request_id).await
}

#[cfg(test)]
#[path = "pipe_tests.rs"]
mod tests;

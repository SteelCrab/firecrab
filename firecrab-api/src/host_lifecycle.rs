//! Target-side host ownership and restart-safe acknowledgement of its API mirror.

use firecrab_helper_protocol::host_iac::{Observation, OwnerChange, Reply, Request};
use firecrab_stack::{ChangeResourceOwner, ManagedResource, ResourceOwnership};

use crate::handlers::stacks::database;
use crate::{error::AppError, state::AppState};
use uuid::Uuid;

#[cfg(test)]
#[path = "host_lifecycle_browser.rs"]
mod browser;
#[cfg(test)]
#[path = "host_lifecycle_tests.rs"]
mod tests;

/// A helper outage must never fall back to an API-local ownership observation.
async fn call(state: &AppState, request_id: Uuid, request: Request) -> Result<Reply, AppError> {
    state.network.host_iac(request).await.map_err(|error| {
        tracing::warn!(%request_id, %error, "host ownership broker unavailable");
        AppError::unavailable(
            "host ownership broker is unavailable; retry the original operation",
            request_id,
        )
    })
}

/// Obtain the authoritative physical target and reject helper replacement after the first admission.
pub(crate) async fn observe(
    state: &AppState,
    request_id: Uuid,
    resource: ManagedResource,
) -> Result<Observation, AppError> {
    firecrab_helper_protocol::host_iac::validate_resource(&resource).map_err(|detail| {
        AppError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "invalid_host_resource",
            detail,
            request_id,
        )
    })?;
    let store = state.store.clone();
    let endpoint_id = database(request_id, move || store.iac_endpoint_id()).await?;
    let Reply::Observation { observation } = call(
        state,
        request_id,
        Request::Observe {
            endpoint_id,
            resource: resource.clone(),
        },
    )
    .await?
    else {
        return Err(AppError::internal(request_id));
    };
    if observation.ownership.endpoint_id != endpoint_id
        || observation.ownership.resource != resource
    {
        return Err(AppError::internal(request_id));
    }
    let store = state.store.clone();
    let host = observation.host.clone();
    database(request_id, move || store.check_iac_host(&host)).await?;
    Ok(observation)
}

/// Reuse the original host intent and receipt after transport loss, API restart or subsequent edits.
pub(crate) async fn change(
    state: AppState,
    request_id: Uuid,
    change: ChangeResourceOwner,
) -> Result<ResourceOwnership, AppError> {
    let operation = change.operation_id;
    let store = state.store.clone();
    let original = database(request_id, move || store.host_owner_intent(operation)).await?;
    let intent = if let Some(original) = original {
        if original.change != change {
            return Err(AppError::conflict(
                "iac_operation_identity",
                "operation ID belongs to another request",
                request_id,
            ));
        }
        let store = state.store.clone();
        let (confirmed, rejected) =
            database(request_id, move || store.host_owner_status(operation)).await?;
        if rejected {
            return Err(rejected_error(request_id));
        }
        if confirmed {
            let store = state.store.clone();
            let retry = change.clone();
            return database(request_id, move || store.change_iac_resource_owner(&retry)).await;
        }
        original
    } else {
        let observed = observe(&state, request_id, change.resource.clone()).await?;
        OwnerChange {
            change,
            host: observed.host,
            configuration_sha256: observed.configuration_sha256,
        }
    };
    let store = state.store.clone();
    let prepare = intent.clone();
    let intent = database(request_id, move || store.prepare_host_owner(prepare)).await?;
    let endpoint_id = intent.change.expected_endpoint_id;
    let reply = call(
        &state,
        request_id,
        Request::ChangeOwner {
            request: Box::new(intent.clone()),
        },
    )
    .await?;
    if let Reply::OwnerRejected { rejection } = reply {
        let store = state.store.clone();
        let request = intent.change;
        database(request_id, move || {
            store.reject_host_owner(&request, rejection)
        })
        .await?;
        return Err(rejected_error(request_id));
    }
    let Reply::OwnerReceipt { receipt } = reply else {
        return Err(AppError::internal(request_id));
    };
    let receipt = receipt.ok_or_else(|| AppError::internal(request_id))?;
    let store = state.store.clone();
    let original = intent.change.clone();
    let result = database(request_id, move || {
        store.commit_host_owner(&original, receipt)
    })
    .await?;
    let Reply::OwnerReceipt {
        receipt: Some(confirmed),
    } = call(
        &state,
        request_id,
        Request::ConfirmOwner {
            endpoint_id,
            operation_id: operation,
        },
    )
    .await?
    else {
        return Err(AppError::internal(request_id));
    };
    if !confirmed.confirmed
        || confirmed.operation_id != operation
        || confirmed.ownership != result
        || confirmed.host != intent.host
    {
        return Err(AppError::internal(request_id));
    }
    let store = state.store.clone();
    database(request_id, move || store.confirm_host_owner(operation)).await?;
    Ok(result)
}

/// Known no-effect conflicts require a new explicit observation and operation identity.
fn rejected_error(request_id: Uuid) -> AppError {
    AppError::conflict(
        "host_ownership_rejected",
        "host ownership changed after observation; inspect it before submitting a new operation",
        request_id,
    )
}

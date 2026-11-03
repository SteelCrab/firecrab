//! Authenticated native installation ownership, separate from guest-local Pipe resources.

use firecrab_helper_protocol::native_iac::{Observation, OwnerChange, Reply, Request};
use firecrab_stack::{ChangeResourceOwner, ManagedResource, ResourceOwnership};

use crate::handlers::stacks::database;
use crate::{error::AppError, state::AppState};
use uuid::Uuid;

#[cfg(test)]
#[path = "native_lifecycle_tests.rs"]
mod tests;

/// Configure one actual native broker; partial settings never enable a fallback transport.
pub(crate) fn configured_client(
    store: &crate::persistence::Store,
) -> Result<Option<firecrab_helper_protocol::native_iac::transport::Client>, firecrab_pki::Error> {
    let values = [
        "FIRECRAB_NATIVE_IAC_ADDRESS",
        "FIRECRAB_NATIVE_IAC_SERVER_NAME",
        "FIRECRAB_NATIVE_IAC_INSTALLATION_ID",
        "FIRECRAB_NATIVE_IAC_CERT",
        "FIRECRAB_NATIVE_IAC_KEY",
        "FIRECRAB_NATIVE_IAC_CA",
    ]
    .map(|key| std::env::var(key).ok());
    if values.iter().all(Option::is_none) {
        return Ok(None);
    }
    let [
        Some(address),
        Some(name),
        Some(installation),
        Some(cert),
        Some(key),
        Some(ca),
    ] = values
    else {
        return Err(firecrab_pki::Error(
            "set all six FIRECRAB_NATIVE_IAC transport references together".into(),
        ));
    };
    if [&address, &name, &installation, &cert, &key, &ca]
        .iter()
        .any(|value| value.is_empty())
    {
        return Err(firecrab_pki::Error(
            "native transport references cannot be empty".into(),
        ));
    }
    let endpoint = store.iac_endpoint_id().map_err(|_| {
        firecrab_pki::Error("native transport requires a persistent API endpoint".into())
    })?;
    let installation = installation
        .parse()
        .map_err(|_| firecrab_pki::Error("invalid native installation UUID".into()))?;
    firecrab_helper_protocol::native_iac::transport::Client::new(
        address,
        name,
        installation,
        endpoint,
        &firecrab_pki::TlsPaths {
            cert: cert.into(),
            key: key.into(),
            ca: ca.into(),
        },
    )
    .map(Some)
    .map_err(|_| firecrab_pki::Error("invalid native mTLS identity or file references".into()))
}

/// Discover the real outer installation without guessing an identity from the guest descriptor.
pub(crate) async fn identity(state: &AppState, request_id: Uuid) -> Result<Reply, AppError> {
    let reply = call(state, request_id, Request::Identity).await?;
    let Reply::Identity { target, kinds } = &reply else {
        return Err(AppError::internal(request_id));
    };
    if target.validate().is_err()
        || kinds
            .iter()
            .any(|kind| !firecrab_helper_protocol::native_iac::native_kind(kind))
    {
        return Err(AppError::internal(request_id));
    }
    let store = state.store.clone();
    let target = target.clone();
    database(request_id, move || store.check_iac_native_host(&target)).await?;
    Ok(reply)
}

/// A helper outage must never fall back to an API-local ownership observation.
async fn call(state: &AppState, request_id: Uuid, request: Request) -> Result<Reply, AppError> {
    state
        .native_host
        .as_ref()
        .ok_or_else(|| {
            AppError::unavailable("native installation broker is not configured", request_id)
        })?
        .call(request)
        .await
        .map_err(|error| {
            tracing::warn!(%request_id, %error, "native installation ownership broker unavailable");
            AppError::unavailable(
                "native installation ownership broker is unavailable; retry the original operation",
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
    let mut canonical = resource.clone();
    canonical
        .normalize()
        .and_then(|()| {
            if canonical == resource
                && firecrab_helper_protocol::native_iac::native_kind(&resource.kind)
            {
                Ok(())
            } else {
                Err("provide the exact native installation UUID and facet")
            }
        })
        .map_err(|detail| {
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
        || firecrab_helper_protocol::native_iac::validate_resource(&resource, &observation.target)
            .is_err()
    {
        return Err(AppError::internal(request_id));
    }
    let store = state.store.clone();
    let host = observation.target.clone();
    database(request_id, move || store.check_iac_native_host(&host)).await?;
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
    let original = database(request_id, move || {
        store.native_host_owner_intent(operation)
    })
    .await?;
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
            target: observed.target,
            configuration_sha256: observed.configuration_sha256,
        }
    };
    let store = state.store.clone();
    let prepare = intent.clone();
    let intent = database(request_id, move || store.prepare_native_host_owner(prepare)).await?;
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
            store.reject_native_host_owner(&request, rejection)
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
        store.commit_native_host_owner(&original, receipt)
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
        || confirmed.target != intent.target
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
        "native_ownership_rejected",
        "native installation ownership changed after observation; inspect it before submitting a new operation",
        request_id,
    )
}

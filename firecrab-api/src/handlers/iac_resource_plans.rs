//! Authenticated shared planning and execution. Configuration stays in private persistence.

use super::stacks::database;
use crate::{
    ai_sandbox::access::Caller, error::AppError, extract::ValidatedJson, server::RequestId,
    state::AppState,
};
use axum::response::IntoResponse;
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use firecrab_stack::{
    ApplyResourcePlan, PlanResource, ResourcePlan, ResourcePlanReceipt, UploadResourceValue,
};
use uuid::Uuid;

fn operator(caller: &Caller, request: Uuid) -> Result<(), AppError> {
    caller.operator().map_err(|_| {
        AppError::new(
            axum::http::StatusCode::FORBIDDEN,
            "iac_operator_required",
            "resource planning and execution require operator access",
            request,
        )
    })
}

/// Metadata refresh never reads application observations or dispatches resource work.
pub(crate) async fn resource(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path((kind, id)): Path<(String, String)>,
    caller: Caller,
) -> Result<axum::response::Response, AppError> {
    operator(&caller, request.0)?;
    let current = database(request.0, move || {
        state.store.resource_state(
            &state.secrets,
            &firecrab_stack::ManagedResource { kind, id },
        )
    })
    .await?;
    let mut response = Json(current).into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

pub(crate) async fn upload(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<Uuid>,
    caller: Caller,
    ValidatedJson(input): ValidatedJson<UploadResourceValue>,
) -> Result<axum::http::StatusCode, AppError> {
    operator(&caller, request.0)?;
    database(request.0, move || {
        state
            .store
            .upload_resource_value(&state.secrets, id, input, crate::secrets::now_ms())
    })
    .await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub(crate) async fn plan(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    caller: Caller,
    ValidatedJson(input): ValidatedJson<PlanResource>,
) -> Result<Json<ResourcePlan>, AppError> {
    operator(&caller, request.0)?;
    database(request.0, move || {
        state
            .store
            .plan_vault_resource(&state.secrets, input, crate::secrets::now_ms())
    })
    .await
    .map(Json)
}

pub(crate) async fn get(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<Uuid>,
) -> Result<Json<ResourcePlan>, AppError> {
    database(request.0, move || state.store.resource_plan(id))
        .await?
        .map(Json)
        .ok_or_else(|| AppError::not_found(request.0))
}

pub(crate) async fn receipt(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<Uuid>,
) -> Result<Json<ResourcePlanReceipt>, AppError> {
    database(request.0, move || state.store.resource_plan_receipt(id))
        .await?
        .map(Json)
        .ok_or_else(|| AppError::not_found(request.0))
}

pub(crate) async fn apply(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<Uuid>,
    caller: Caller,
    ValidatedJson(input): ValidatedJson<ApplyResourcePlan>,
) -> Result<Json<ResourcePlanReceipt>, AppError> {
    operator(&caller, request.0)?;
    let rid = request.0;
    tokio::spawn(async move { execute(state, request, id, input).await })
        .await
        .map_err(|_| AppError::internal(rid))?
        .map(Json)
}

async fn execute(
    state: AppState,
    request: RequestId,
    id: Uuid,
    input: ApplyResourcePlan,
) -> Result<ResourcePlanReceipt, AppError> {
    let _admission = state.stack_apply_admission.lock().await;
    let _ai = state.pipe_ai_mutations.lock().await;
    let store = state.store.clone();
    let plan = database(request.0, move || store.resource_plan(id)).await?;
    let kind = plan
        .as_ref()
        .map(|plan| plan.ownership.resource.kind.as_str());
    let _services = if matches!(kind, Some("secret" | "security_group" | "service_otlp")) {
        Some(state.service_mutations.clone().lock_owned().await)
    } else {
        None
    };
    let _network = if kind == Some("security_group") {
        Some(state.network_mutations.clone().lock_owned().await)
    } else {
        None
    };
    if let Some(plan) = plan.as_ref()
        && kind == Some("security_group")
        && plan.changed_fields.iter().any(|field| field == "rules")
    {
        let store = state.store.clone();
        let completed = database(request.0, move || store.resource_plan_receipt(id))
            .await?
            .is_some();
        if !completed {
            super::security_groups::ensure_idle(
                &state,
                plan.ownership
                    .resource
                    .id
                    .parse()
                    .map_err(|_| AppError::internal(request.0))?,
                request.0,
            )
            .await?;
        }
    }
    let _sandbox = if kind == Some("sandbox_access") {
        Some(state.sandbox.admission.clone().lock_owned().await)
    } else {
        None
    };
    let otlp = if kind == Some("service_otlp") {
        let store = state.store.clone();
        let observed = input.clone();
        let preparation = database(request.0, move || {
            store.resource_plan_otlp_input(id, &observed, crate::secrets::now_ms())
        })
        .await?;
        if let Some(preparation) = preparation {
            Some(crate::service_observation::prepare_plan(&state, preparation, request.0).await?)
        } else {
            None
        }
    } else {
        None
    };
    let store = state.store.clone();
    let observed = input.clone();
    if let Some((mut source, generation)) = database(request.0, move || {
        store.resource_plan_ai_source(id, &observed, crate::secrets::now_ms())
    })
    .await?
    {
        let original = source.clone();
        if state.pipe_ai.refresh(&mut source).await.map_err(|_| {
            AppError::unavailable(
                "original AI credential readiness could not be verified",
                request.0,
            )
        })? {
            let store = state.store.clone();
            database(request.0, move || {
                store.refresh_ai_connection(&original, &source, generation)
            })
            .await?;
        }
    }
    let private = kind == Some("secret");
    let store = state.store.clone();
    let vault = state.secrets.clone();
    let receipt = database(request.0, move || {
        if private {
            store.apply_vault_resource_plan(&vault, id, &input, crate::secrets::now_ms())
        } else {
            store.apply_resource_plan(id, &input, crate::secrets::now_ms())
        }
    })
    .await?;
    if let Some(prepared) = otlp {
        crate::service_observation::activate_plan(&state, &receipt, prepared, request.0).await;
    }
    Ok(receipt)
}

#[cfg(test)]
#[path = "iac_resource_plan_tests.rs"]
mod tests;

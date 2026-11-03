//! Authenticated shared planning and execution. Configuration stays in private persistence.

use super::stacks::database;
use crate::{
    ai_sandbox::access::Caller, error::AppError, extract::ValidatedJson, server::RequestId,
    state::AppState,
};
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
    let kind = database(request.0, move || store.resource_plan(id))
        .await?
        .map(|plan| plan.ownership.resource.kind);
    let _services = if kind.as_deref() == Some("secret") {
        Some(state.service_mutations.clone().lock_owned().await)
    } else {
        None
    };
    let _sandbox = if kind.as_deref() == Some("sandbox_access") {
        Some(state.sandbox.admission.clone().lock_owned().await)
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
    database(request.0, move || {
        if kind.as_deref() == Some("secret") {
            state.store.apply_vault_resource_plan(
                &state.secrets,
                id,
                &input,
                crate::secrets::now_ms(),
            )
        } else {
            state
                .store
                .apply_resource_plan(id, &input, crate::secrets::now_ms())
        }
    })
    .await
}

#[cfg(test)]
#[path = "iac_resource_plan_tests.rs"]
mod tests;

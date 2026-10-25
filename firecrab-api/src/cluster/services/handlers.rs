//! Public service lifecycle and inspection routes.
use super::*;
use crate::{error::AppError, extract::ValidatedJson, server::RequestId};
use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/api/cluster/services", get(list).post(create))
        .route("/api/cluster/services/{id}", get(show).put(update))
        .route("/api/cluster/services/{id}/update", post(update))
        .route("/api/cluster/services/{id}/stop", post(stop))
        .route("/api/cluster/services/{id}/reconcile", post(reconcile))
        .route("/api/cluster/services/{id}/operations", get(operations))
        .route(
            "/api/cluster/worker/services/{id}",
            get(worker::get).put(worker::put),
        )
}

async fn list(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
) -> Result<Json<Vec<ClusterService>>, AppError> {
    let store = state.store.clone();
    stored(move || store.cluster_services())
        .await
        .map(Json)
        .map_err(|e| super::super::handlers::error(e, request))
}
async fn show(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<Uuid>,
) -> Result<Json<ClusterService>, AppError> {
    let store = state.store.clone();
    stored(move || store.cluster_service(id))
        .await
        .map(Json)
        .map_err(|e| super::super::handlers::error(e, request))
}
async fn operations(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<ClusterServiceOperation>>, AppError> {
    let store = state.store.clone();
    stored(move || store.cluster_service_operations(id))
        .await
        .map(Json)
        .map_err(|e| super::super::handlers::error(e, request))
}
async fn submit(
    state: AppState,
    request: RequestId,
    headers: HeaderMap,
    id: Option<Uuid>,
    action: ClusterServiceAction,
    expected: Option<u64>,
    spec: Option<ClusterServiceSpec>,
) -> Result<(StatusCode, Json<ClusterService>), AppError> {
    super::super::handlers::require_transport(&state, request)?;
    let key = headers
        .get("Idempotency-Key")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let store = state.store.clone();
    let s = stored(move || store.cluster_service_admit(&key, id, action, expected, spec, now_ms()))
        .await
        .map_err(|e| super::super::handlers::error(e, request))?;
    wake(state, s.id);
    Ok((StatusCode::ACCEPTED, Json(s)))
}
async fn create(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    headers: HeaderMap,
    ValidatedJson(spec): ValidatedJson<ClusterServiceSpec>,
) -> Result<(StatusCode, Json<ClusterService>), AppError> {
    submit(
        state,
        request,
        headers,
        None,
        ClusterServiceAction::Create,
        None,
        Some(spec),
    )
    .await
}
async fn update(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ValidatedJson(input): ValidatedJson<UpdateClusterServiceRequest>,
) -> Result<(StatusCode, Json<ClusterService>), AppError> {
    submit(
        state,
        request,
        headers,
        Some(id),
        ClusterServiceAction::Update,
        Some(input.expected_generation),
        Some(input.spec),
    )
    .await
}
async fn stop(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ValidatedJson(input): ValidatedJson<StopClusterServiceRequest>,
) -> Result<(StatusCode, Json<ClusterService>), AppError> {
    submit(
        state,
        request,
        headers,
        Some(id),
        ClusterServiceAction::Stop,
        Some(input.expected_generation),
        None,
    )
    .await
}
async fn reconcile(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<ClusterService>), AppError> {
    super::super::handlers::require_transport(&state, request)?;
    let Json(s) = show(State(state.clone()), Extension(request), Path(id)).await?;
    wake(state, id);
    Ok((StatusCode::ACCEPTED, Json(s)))
}

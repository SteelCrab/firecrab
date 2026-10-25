//! Controller management routes; worker enrollment requires a real TLS peer.
use super::{
    now_ms, reconcile_one, stored, validate_report,
    worker::{Peer, peer_id},
};
use crate::{
    error::AppError, extract::ValidatedJson, persistence::ClusterError, server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use firecrab_api_types::*;
use std::collections::BTreeMap;
use uuid::Uuid;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .merge(super::services::router())
        .route("/api/cluster/hosts", get(list_hosts))
        .route("/api/cluster/hosts/register", post(register))
        .route("/api/cluster/hosts/{id}", get(get_host).patch(drain))
        .route("/api/cluster/hosts/{id}/heartbeat", post(heartbeat))
        .route("/api/cluster/placements", get(list_placements).post(create))
        .route(
            "/api/cluster/placements/{id}",
            get(get_placement).delete(delete),
        )
        .route("/api/cluster/placements/{id}/start", post(start))
        .route("/api/cluster/placements/{id}/stop", post(stop))
        .route("/api/cluster/placements/{id}/delete", post(delete))
        .route("/api/cluster/placements/{id}/reconcile", post(reconcile))
        .route(
            "/api/cluster/worker/placements/{id}",
            get(super::worker::get).put(super::worker::put),
        )
}

pub(crate) fn error(error: ClusterError, request: RequestId) -> AppError {
    match error {
        ClusterError::Invalid(message) => {
            AppError::validation(BTreeMap::from([("cluster".into(), message)]), request.0)
        }
        ClusterError::Conflict(_) => AppError::conflict(
            "cluster_conflict",
            "request conflicts with the recorded owner, generation or idempotency key",
            request.0,
        ),
        ClusterError::Missing => AppError::not_found(request.0),
        ClusterError::Capacity => AppError::conflict(
            "insufficient_capacity",
            "no healthy host satisfies the placement constraints and available capacity",
            request.0,
        ),
        other => {
            tracing::error!(error=%other,request_id=%request.0,"cluster persistence operation failed");
            AppError::internal(request.0)
        }
    }
}

async fn list_hosts(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
) -> Result<Json<Vec<ClusterHost>>, AppError> {
    let store = state.store.clone();
    stored(move || store.cluster_hosts(now_ms()))
        .await
        .map(Json)
        .map_err(|e| error(e, request))
}
async fn get_host(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<Uuid>,
) -> Result<Json<ClusterHost>, AppError> {
    let Json(hosts) = list_hosts(State(state), Extension(request)).await?;
    hosts
        .into_iter()
        .find(|h| h.report.id == id)
        .map(Json)
        .ok_or_else(|| AppError::not_found(request.0))
}
async fn register(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    peer: Peer,
    ValidatedJson(mut report): ValidatedJson<HostReport>,
) -> Result<Json<ClusterHost>, AppError> {
    let id = peer_id(peer, request)?;
    if id != report.id {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "host_identity_mismatch",
            "host ID must match the authenticated TLS peer",
            request.0,
        ));
    }
    validate_report(&mut report).map_err(|e| error(e, request))?;
    let store = state.store.clone();
    stored(move || store.cluster_register(&report, now_ms()))
        .await
        .map_err(|e| error(e, request))?;
    get_host(State(state), Extension(request), Path(id)).await
}
async fn heartbeat(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    peer: Peer,
    Path(id): Path<Uuid>,
    ValidatedJson(report): ValidatedJson<HostReport>,
) -> Result<Json<ClusterHost>, AppError> {
    if report.id != id {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "host_identity_mismatch",
            "heartbeat ID must match the registered host",
            request.0,
        ));
    }
    register(
        State(state),
        Extension(request),
        peer,
        ValidatedJson(report),
    )
    .await
}
async fn drain(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<Uuid>,
    ValidatedJson(input): ValidatedJson<DrainHostRequest>,
) -> Result<Json<ClusterHost>, AppError> {
    let store = state.store.clone();
    stored(move || store.cluster_drain(id, input.draining))
        .await
        .map_err(|e| error(e, request))?;
    get_host(State(state), Extension(request), Path(id)).await
}
async fn list_placements(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
) -> Result<Json<Vec<VmPlacement>>, AppError> {
    let store = state.store.clone();
    stored(move || store.cluster_placements())
        .await
        .map(Json)
        .map_err(|e| error(e, request))
}
async fn get_placement(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<Uuid>,
) -> Result<Json<VmPlacement>, AppError> {
    let store = state.store.clone();
    stored(move || store.cluster_placement(id))
        .await
        .map(Json)
        .map_err(|e| error(e, request))
}
async fn create(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    headers: HeaderMap,
    ValidatedJson(spec): ValidatedJson<PlacementSpec>,
) -> Result<(StatusCode, Json<VmPlacement>), AppError> {
    require_transport(&state, request)?;
    let key = headers
        .get("Idempotency-Key")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let store = state.store.clone();
    let p = stored(move || store.cluster_admit(&key, spec, now_ms()))
        .await
        .map_err(|e| error(e, request))?;
    wake(state, p.id);
    Ok((StatusCode::ACCEPTED, Json(p)))
}
fn wake(state: AppState, id: Uuid) {
    tokio::spawn(async move {
        let _ = reconcile_one(state, id, true).await;
    });
}

pub(super) fn require_transport(state: &AppState, request: RequestId) -> Result<(), AppError> {
    if state.cluster.paths.is_none() {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "cluster_tls_required",
            "configure mutual TLS before dispatching remote operations",
            request.0,
        ));
    }
    Ok(())
}
async fn action(
    state: AppState,
    request: RequestId,
    id: Uuid,
    action: PlacementAction,
) -> Result<(StatusCode, Json<VmPlacement>), AppError> {
    require_transport(&state, request)?;
    let store = state.store.clone();
    let p = stored(move || store.cluster_action(id, action))
        .await
        .map_err(|e| error(e, request))?;
    wake(state, id);
    Ok((StatusCode::ACCEPTED, Json(p)))
}
async fn start(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<VmPlacement>), AppError> {
    action(state, request, id, PlacementAction::Start).await
}
async fn stop(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<VmPlacement>), AppError> {
    action(state, request, id, PlacementAction::Stop).await
}
async fn delete(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<VmPlacement>), AppError> {
    action(state, request, id, PlacementAction::Delete).await
}
async fn reconcile(
    State(state): State<AppState>,
    Extension(request): Extension<RequestId>,
    Path(id): Path<Uuid>,
) -> Result<Json<VmPlacement>, AppError> {
    require_transport(&state, request)?;
    reconcile_one(state.clone(), id, true)
        .await
        .map_err(|e| error(e, request))?;
    get_placement(State(state), Extension(request), Path(id)).await
}

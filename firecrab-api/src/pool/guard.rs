//! Keeps pool members out of reach of the ordinary VM routes.

use axum::Extension;
use axum::extract::{Path, Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

use crate::error::AppError;
use crate::model::VmPurpose;
use crate::server::RequestId;
use crate::state::AppState;

/// Refuses any non-read request on a pool-owned VM. A caller holds such a VM
/// only through a lease; the pool alone starts, stops, edits, or deletes it,
/// which is what keeps a released disk from ever being reused.
pub(crate) async fn reject_pool_owned(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Path(id): Path<String>,
    request: Request,
    next: Next,
) -> Response {
    if matches!(*request.method(), Method::GET | Method::HEAD) {
        return next.run(request).await;
    }
    let pool_owned = Uuid::parse_str(&id).is_ok_and(|id| {
        state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .is_some_and(|vm| vm.purpose == VmPurpose::Pool)
    });
    if pool_owned {
        return AppError::conflict(
            "pool_owned",
            "this VM belongs to a pool; release its lease instead",
            request_id.0,
        )
        .into_response();
    }
    next.run(request).await
}

//! Ordinary VM mutation routes cannot bypass CODE ownership or guest-sync fences.
use crate::{error::AppError, server::RequestId, state::AppState};
use axum::{
    Extension,
    extract::{Path, Request, State},
    http::Method,
    middleware::Next,
    response::{IntoResponse, Response},
};
use uuid::Uuid;

pub(crate) async fn reject_vm_mutation(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(vm): Path<String>,
    request: Request,
    next: Next,
) -> Response {
    if matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    ) {
        return next.run(request).await;
    }
    if let Ok(vm) = Uuid::parse_str(&vm) {
        let store = state.store.clone();
        match tokio::task::spawn_blocking(move || store.code_runtime_owner(vm)).await {
            Ok(Ok(None)) => {}
            Ok(Ok(Some(_))) => {
                return AppError::conflict(
                    "code_vm_owned",
                    "Manage this VM through its CODE runtime, or detach it after stop",
                    id.0,
                )
                .into_response();
            }
            _ => return AppError::internal(id.0).into_response(),
        }
    }
    next.run(request).await
}

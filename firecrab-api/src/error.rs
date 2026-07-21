use std::collections::BTreeMap;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use firecrab_api_types::{ApiError, ErrorResponse};
use uuid::Uuid;

use crate::model::VmState;
use crate::persistence::encode_state;

#[derive(Debug)]
pub struct AppError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    fields: BTreeMap<String, String>,
    request_id: Uuid,
}

impl AppError {
    pub fn new(
        status: StatusCode,
        code: &'static str,
        message: &'static str,
        request_id: Uuid,
    ) -> Self {
        Self {
            status,
            code,
            message,
            fields: BTreeMap::new(),
            request_id,
        }
    }

    pub fn validation(fields: BTreeMap<String, String>, request_id: Uuid) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "validation_failed",
            message: "request validation failed",
            fields,
            request_id,
        }
    }

    pub fn invalid_json(request_id: Uuid) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "request body must be one valid JSON object",
            request_id,
        )
    }

    pub fn unsupported_media_type(request_id: Uuid) -> Self {
        Self::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "Content-Type must be application/json",
            request_id,
        )
    }

    pub fn request_too_large(request_id: Uuid) -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_too_large",
            "request body exceeds 64 KiB",
            request_id,
        )
    }

    pub fn forbidden_origin(request_id: Uuid) -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "forbidden_origin",
            "request origin is not allowed",
            request_id,
        )
    }

    pub fn too_many_requests(request_id: Uuid) -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_requests",
            "request concurrency limit exceeded",
            request_id,
        )
    }

    pub fn gateway_timeout(request_id: Uuid) -> Self {
        Self::new(
            StatusCode::GATEWAY_TIMEOUT,
            "request_timeout",
            "request processing timed out",
            request_id,
        )
    }

    pub fn internal(request_id: Uuid) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "internal server error",
            request_id,
        )
    }

    pub fn not_found(request_id: Uuid) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "resource not found",
            request_id,
        )
    }

    /// The VM has no live Firecracker process (and therefore no console to
    /// attach to) — distinct from `not_found`, which means the VM record
    /// itself doesn't exist.
    pub fn vm_not_running(request_id: Uuid) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "vm_not_running",
            "VM has no active console; it must be running",
            request_id,
        )
    }

    pub fn invalid_state(current: VmState, request_id: Uuid) -> Self {
        let mut fields = BTreeMap::new();
        fields.insert("state".to_owned(), encode_state(current));
        Self {
            status: StatusCode::CONFLICT,
            code: "invalid_state",
            message: "current VM state does not allow this operation",
            fields,
            request_id,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = ErrorResponse {
            error: ApiError {
                code: self.code.to_owned(),
                message: self.message.to_owned(),
                fields: self.fields,
                request_id: self.request_id,
            },
        };
        (self.status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use serde_json::Value;

    use super::*;

    #[tokio::test]
    async fn validation_error_returns_a_structured_bad_request() {
        let mut fields = BTreeMap::new();
        fields.insert("template".to_owned(), "is not supported".to_owned());
        let request_id = Uuid::new_v4();

        let response = AppError::validation(fields, request_id).into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "validation_failed");
        assert_eq!(json["error"]["fields"]["template"], "is not supported");
        assert_eq!(json["error"]["requestId"], request_id.to_string());
    }

    #[tokio::test]
    async fn internal_error_does_not_expose_details() {
        let response = AppError::internal(Uuid::new_v4()).into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("internal_error"));
        assert!(!body.contains("path"));
    }
}

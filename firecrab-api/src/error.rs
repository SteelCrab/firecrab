use std::collections::BTreeMap;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use firecrab_api_types::{ApiError, ErrorResponse};
use uuid::Uuid;

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

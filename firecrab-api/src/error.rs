use std::collections::BTreeMap;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::persistence::PersistenceError;
use crate::templates::TemplateError;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("request body is not valid JSON")]
    InvalidJson(#[source] JsonRejection),
    #[error("requested template is not supported")]
    InvalidTemplate,
    #[error("template artifact validation failed")]
    Template(#[from] TemplateError),
    #[error("VM state persistence failed")]
    Persistence(#[from] PersistenceError),
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: ApiError,
}

#[derive(Debug, Serialize)]
struct ApiError {
    code: &'static str,
    message: &'static str,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    fields: BTreeMap<String, String>,
    request_id: Uuid,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let request_id = Uuid::new_v4();
        let mut fields = BTreeMap::new();

        let (status, code, message) = match &self {
            AppError::InvalidJson(rejection) => match rejection.status() {
                StatusCode::UNSUPPORTED_MEDIA_TYPE => (
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    "unsupported_media_type",
                    "content type must be application/json",
                ),
                StatusCode::PAYLOAD_TOO_LARGE => (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "request_too_large",
                    "request body is too large",
                ),
                _ => (
                    StatusCode::BAD_REQUEST,
                    "invalid_json",
                    "request body is not valid JSON",
                ),
            },
            AppError::InvalidTemplate => {
                fields.insert("template".to_owned(), "is not supported".to_owned());
                (
                    StatusCode::BAD_REQUEST,
                    "validation_failed",
                    "request validation failed",
                )
            }
            AppError::Template(_) | AppError::Persistence(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "an internal error occurred",
            ),
        };

        if status.is_server_error() {
            eprintln!("[ERROR] request_id={request_id} {self}");
            if let Some(source) = std::error::Error::source(&self) {
                eprintln!("[ERROR] request_id={request_id} caused by: {source}");
            }
        }

        let mut response = (
            status,
            Json(ErrorResponse {
                error: ApiError {
                    code,
                    message,
                    fields,
                    request_id,
                },
            }),
        )
            .into_response();

        if let Ok(value) = HeaderValue::from_str(&request_id.to_string()) {
            response.headers_mut().insert("x-request-id", value);
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;

    use axum::body::to_bytes;
    use serde_json::Value;

    use super::*;

    #[tokio::test]
    async fn invalid_template_returns_a_structured_bad_request() {
        let response = AppError::InvalidTemplate.into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(response.headers().contains_key("x-request-id"));

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "validation_failed");
        assert_eq!(json["error"]["fields"]["template"], "is not supported");
        assert!(json["error"]["request_id"].is_string());
    }

    #[tokio::test]
    async fn internal_error_does_not_expose_persistence_details() {
        let error = PersistenceError::Read {
            path: PathBuf::from("/secret/data/vms.json"),
            source: io::Error::new(io::ErrorKind::PermissionDenied, "sensitive detail"),
        };

        let response = AppError::from(error).into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("internal_error"));
        assert!(!body.contains("/secret"));
        assert!(!body.contains("sensitive detail"));
    }
}

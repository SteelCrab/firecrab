use axum::Json;
use axum::extract::{FromRequest, Request, rejection::JsonRejection};
use serde::de::DeserializeOwned;

use crate::error::AppError;
use crate::server::request_id;

pub struct ValidatedJson<T>(pub T);

impl<S, T> FromRequest<S> for ValidatedJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = AppError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let request_id = request_id(&request);
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(JsonRejection::MissingJsonContentType(_)) => {
                Err(AppError::unsupported_media_type(request_id))
            }
            Err(JsonRejection::BytesRejection(_)) => Err(AppError::request_too_large(request_id)),
            Err(JsonRejection::JsonDataError(_) | JsonRejection::JsonSyntaxError(_)) => {
                Err(AppError::invalid_json(request_id))
            }
            Err(_) => Err(AppError::invalid_json(request_id)),
        }
    }
}

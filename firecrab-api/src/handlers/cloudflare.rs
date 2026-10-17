//! Tokens are used only for this request; setup runs in the root helper's CLI.
use crate::{
    error::AppError,
    network::{NetworkClient, NetworkError},
    server::RequestId,
};
use axum::{Extension, Json, http::StatusCode};
use firecrab_helper_protocol::cloudflare::{AccessRequest, ConnectRequest, Zone, identifier};
use firecrab_helper_protocol::network::HelperFailure;
use firecrab_helper_protocol::pipe::{Operation, Snapshot};
use std::collections::BTreeMap;
use std::time::Duration;

/// Attach a sanitized setup error to the Cloudflare form and current request ID.
fn invalid(message: impl Into<String>, request_id: RequestId) -> AppError {
    AppError::validation(
        BTreeMap::from([("cloudflare".into(), message.into())]),
        request_id.0,
    )
}

/// List at most 500 accessible active domains without following redirects or saving tokens.
async fn read_zones(token: &str, base: &str) -> Result<Vec<Zone>, &'static str> {
    let mut authorization = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| "Invalid Cloudflare API token")?;
    authorization.set_sensitive(true);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| "Cannot initialize the Cloudflare connection")?;
    let mut zones = Vec::new();
    for page in 1..=10 {
        let mut response = client
            .get(format!("{base}zones?per_page=50&page={page}"))
            .header(reqwest::header::AUTHORIZATION, authorization.clone())
            .send()
            .await
            .map_err(|_| "Cannot reach Cloudflare; check network access and try again")?;
        if matches!(response.status().as_u16(), 401 | 403) {
            return Err(
                "Cloudflare rejected the API token. Grant Zone / Zone / Read and include the domain you want to connect",
            );
        }
        if response.status().as_u16() == 429 {
            return Err("Cloudflare rate limit reached; retry later");
        }
        if !response.status().is_success() {
            return Err("Cloudflare could not list domains; try again later");
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "Cannot read the Cloudflare response")?
        {
            if bytes.len() + chunk.len() > 1024 * 1024 {
                return Err("Cloudflare response exceeded the size limit");
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid Cloudflare response")?;
        if value.get("success").and_then(serde_json::Value::as_bool) != Some(true) {
            return Err("Cloudflare rejected the token or domain permissions");
        }
        let batch: Vec<Zone> =
            serde_json::from_value(value.get("result").cloned().unwrap_or_default())
                .map_err(|_| "Invalid Cloudflare domain response")?;
        zones.extend(batch.into_iter().filter(|zone| {
            zone.status == "active" && identifier(&zone.id) && identifier(&zone.account.id)
        }));
        let pages = value
            .pointer("/result_info/total_pages")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1);
        if pages > 10 {
            return Err("Scope the API token to at most 500 domains");
        }
        if page >= pages {
            break;
        }
    }
    zones.sort_by(|a, b| a.name.cmp(&b.name));
    if zones.is_empty() {
        return Err(
            "No active domains are available. Add a domain to Cloudflare, activate its nameservers, and include it in the API token",
        );
    }
    Ok(zones)
}

/// Validate a transient API token and return active domains for the setup selector.
pub async fn zones(
    Extension(request_id): Extension<RequestId>,
    Json(request): Json<AccessRequest>,
) -> Result<Json<Vec<Zone>>, AppError> {
    request
        .api_token
        .validate()
        .map_err(|error| invalid(error, request_id))?;
    read_zones(
        request.api_token.value(),
        "https://api.cloudflare.com/client/v4/",
    )
    .await
    .map(Json)
    .map_err(|error| invalid(error, request_id))
}

/// Validate onboarding input and delegate the full setup transaction to the root helper.
pub async fn connect(
    Extension(request_id): Extension<RequestId>,
    Json(request): Json<ConnectRequest>,
) -> Result<Json<Snapshot>, AppError> {
    request
        .validate()
        .map_err(|error| invalid(error, request_id))?;
    NetworkClient::from_env()
        .pipe(Operation::ConnectCloudflare { request })
        .await
        .map(Json)
        .map_err(|error| match error {
            NetworkError::Helper(
                HelperFailure::InvalidRequest { detail } | HelperFailure::Internal { detail },
            ) => invalid(detail, request_id),
            _ => AppError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "cloudflare_setup_unavailable",
                "Cannot reach the setup helper; check Pipe status before retrying",
                request_id.0,
            ),
        })
}

#[cfg(test)]
#[path = "cloudflare_tests.rs"]
mod tests;

//! Account verification only. Model use belongs to the AI Sandbox adapter.
use super::{Failure, response_bytes, transport_failure};
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

pub const CREDENTIAL_ENDPOINT: &str = "https://api.anthropic.com/v1/models";
pub const API_VERSION: &str = "2026-10-21";

#[derive(Debug, Clone)]
pub struct Anthropic {
    client: Client,
    credential_endpoint: String,
}

impl Anthropic {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(45))
                .build()
                .expect("Anthropic HTTP client"),
            credential_endpoint: CREDENTIAL_ENDPOINT.into(),
        }
    }

    #[cfg(test)]
    pub fn at(credential_endpoint: String) -> Self {
        Self {
            credential_endpoint,
            ..Self::new()
        }
    }

    pub async fn verify_credentials(&self, api_key: &str) -> Result<(), Failure> {
        let response = self
            .client
            .get(&self.credential_endpoint)
            .header("x-api-key", api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .send()
            .await
            .map_err(transport_failure)?;
        match response.status().as_u16() {
            200 => {}
            401 | 403 => return Err(Failure::Authentication),
            429 => return Err(Failure::RateLimited),
            _ => return Err(Failure::Provider),
        }
        let document: Value = serde_json::from_slice(&response_bytes(response).await?)
            .map_err(|_| Failure::Provider)?;
        if !document["data"].is_array() || !document["has_more"].is_boolean() {
            return Err(Failure::Provider);
        }
        Ok(())
    }
}

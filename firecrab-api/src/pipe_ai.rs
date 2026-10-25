//! Fixed provider endpoints and disabled redirects keep credentials scoped.
use std::time::Duration;

use firecrab_api_types::{AiConnection, AiConnectionInput};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub mod anthropic;
mod oauth;
pub use oauth::{LoginRegistry, OAuthRegistration, OAuthSession};

/// Fixed text inference endpoint for the OpenAI adapter.
pub const ENDPOINT: &str = "https://api.openai.com/v1/responses";
/// Credential-only verification endpoint; listing models does not prove inference access.
pub const CREDENTIAL_ENDPOINT: &str = "https://api.openai.com/v1/models";
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Private server record. No Debug implementation is allowed to print its credentials.
#[derive(Clone, Deserialize, Serialize)]
pub struct SavedConnection {
    /// Public metadata that may be returned to Dashboard, CLI, and MCP clients.
    pub connection: AiConnection,
    /// Provider API key or current OAuth access token; never sent to clients.
    pub api_key: String,
    /// Optional private refresh session for a ChatGPT account.
    #[serde(default)]
    pub oauth: Option<OAuthSession>,
}

impl SavedConnection {
    /// Replace saved access and refresh credentials in user input or returned text.
    pub fn redact(&self, text: &str) -> String {
        let mut text = if self.api_key.is_empty() {
            text.to_owned()
        } else {
            text.replace(&self.api_key, "[REDACTED]")
        };
        if let Some(session) = &self.oauth
            && !session.refresh_token.is_empty()
        {
            text = text.replace(&session.refresh_token, "[REDACTED]");
        }
        text
    }
}

/// Bounded, fixed-endpoint OpenAI client without automatic redirects or inference retries.
#[derive(Debug, Clone)]
pub struct OpenAi {
    /// Fixed-policy HTTP client shared with the Sandbox native adapter.
    pub(crate) client: Client,
    /// Official Responses endpoint, replaced only by test fixtures.
    pub(crate) endpoint: String,
    /// Official model catalogue endpoint.
    pub(crate) credential_endpoint: String,
    auth_base: String,
}

/// Sanitized provider failures shared by connection and model operations.
#[derive(Debug, Clone, Copy)]
pub enum Failure {
    /// Client input is invalid or the provider rejected its request parameters.
    InvalidInput,
    /// The provider rejected or expired the supplied credential.
    Authentication,
    /// The selected model is unavailable to this account.
    ModelDenied,
    /// Provider quota or request limits were reached.
    RateLimited,
    /// The provider deadline expired and the request outcome may be uncertain.
    Timeout,
    /// Transport, status, or response shape did not match the provider contract.
    Provider,
    /// Inference did not yield a complete assistant text response.
    NoText,
    /// Authorization state or token identity validation failed.
    Login,
    /// Required account plan usage consent was not granted.
    Consent,
}

impl Failure {
    /// Public HTTP status, stable code, and message without provider secrets.
    pub fn details(self) -> (StatusCode, &'static str, &'static str) {
        match self {
            Self::Login => (
                StatusCode::BAD_REQUEST,
                "ai_login_failed",
                "ChatGPT login was rejected or expired; start a new login",
            ),
            Self::Consent => (
                StatusCode::FORBIDDEN,
                "ai_consent_required",
                "ChatGPT plan usage permission was not granted",
            ),
            Self::InvalidInput => (
                StatusCode::BAD_REQUEST,
                "ai_invalid_input",
                "Invalid AI input",
            ),
            Self::Authentication => (
                StatusCode::UNAUTHORIZED,
                "ai_authentication_expired",
                "Provider authentication was rejected; reconnect the account",
            ),
            Self::ModelDenied => (
                StatusCode::FORBIDDEN,
                "ai_model_denied",
                "The provider denied access to the selected model",
            ),
            Self::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "ai_rate_limited",
                "Provider quota or rate limit reached",
            ),
            Self::Timeout => (
                StatusCode::GATEWAY_TIMEOUT,
                "ai_timeout",
                "Provider request timed out; the outcome may be uncertain",
            ),
            Self::Provider => (
                StatusCode::BAD_GATEWAY,
                "ai_provider_failed",
                "Provider request failed",
            ),
            Self::NoText => (
                StatusCode::BAD_GATEWAY,
                "ai_no_text",
                "The selected model did not return a complete text response",
            ),
        }
    }
}

impl OpenAi {
    /// Construct the production client using pinned HTTPS provider endpoints.
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(45))
                .build()
                .expect("OpenAI HTTP client"),
            endpoint: ENDPOINT.into(),
            credential_endpoint: CREDENTIAL_ENDPOINT.into(),
            auth_base: "https://auth.openai.com".into(),
        }
    }

    #[cfg(test)]
    /// Point provider requests at an isolated local test fixture.
    pub fn at(endpoint: String) -> Self {
        let auth_base = endpoint.trim_end_matches("/v1/responses").to_owned();
        Self {
            credential_endpoint: format!("{auth_base}/v1/models"),
            endpoint,
            auth_base,
            ..Self::new()
        }
    }

    /// Check the bearer credential without choosing a model or generating text.
    pub async fn verify_credentials(&self, api_key: &str) -> Result<(), Failure> {
        let response = self
            .client
            .get(&self.credential_endpoint)
            .bearer_auth(api_key)
            .send()
            .await
            .map_err(transport_failure)?;
        match response.status().as_u16() {
            200 => {}
            401 | 403 => return Err(Failure::Authentication),
            429 => return Err(Failure::RateLimited),
            _ => return Err(Failure::Provider),
        }
        let bytes = response_bytes(response).await?;
        let document: Value = serde_json::from_slice(&bytes).map_err(|_| Failure::Provider)?;
        if document["object"] != "list" || !document["data"].is_array() {
            return Err(Failure::Provider);
        }
        Ok(())
    }

    /// Both verification and normal inference use the same permission check.
    /// No tools, redirects, retries, credentials in input, or provider-side storage.
    pub async fn respond(
        &self,
        api_key: &str,
        model: &str,
        input: &str,
    ) -> Result<String, Failure> {
        self.infer(api_key, model, input, false).await
    }

    /// Run a bounded text request with the credential's API-key or ChatGPT contract.
    pub async fn infer(
        &self,
        api_key: &str,
        model: &str,
        input: &str,
        chatgpt: bool,
    ) -> Result<String, Failure> {
        validate_model(model)?;
        if input.trim().is_empty() || input.len() > 32_768 {
            return Err(Failure::InvalidInput);
        }
        let mut body = json!({
            "model": model, "input": input, "store": false, "stream": false,
            "max_output_tokens": 1024,
        });
        if chatgpt {
            body["input"] = json!([{ "role": "user", "content": input }]);
            body["stream"] = json!(true);
            body.as_object_mut().unwrap().remove("max_output_tokens");
        }
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(api_key)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(transport_failure)?;
        match response.status().as_u16() {
            200..=299 => {}
            401 => return Err(Failure::Authentication),
            403 | 404 => return Err(Failure::ModelDenied),
            429 => return Err(Failure::RateLimited),
            400 | 422 => return Err(Failure::InvalidInput),
            _ => return Err(Failure::Provider),
        }
        let bytes = response_bytes(response).await?;
        let document = if chatgpt {
            completed_stream(&bytes)?
        } else {
            serde_json::from_slice(&bytes).map_err(|_| Failure::Provider)?
        };
        if document["status"] != "completed" {
            return Err(Failure::NoText);
        }
        let output = document["output"].as_array().ok_or(Failure::NoText)?;
        let mut text = String::new();
        for item in output {
            if item["type"] != "message" || item["role"] != "assistant" {
                continue;
            }
            for content in item["content"].as_array().into_iter().flatten() {
                if content["type"] == "output_text" {
                    text.push_str(content["text"].as_str().ok_or(Failure::NoText)?);
                }
            }
        }
        if text.trim().is_empty() {
            return Err(Failure::NoText);
        }
        Ok(text.replace(api_key, "[REDACTED]"))
    }
}

/// Reject oversized provider bodies before parsing or exposing them.
pub(crate) async fn response_bytes(mut response: reqwest::Response) -> Result<Vec<u8>, Failure> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_failure)? {
        if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(Failure::Provider);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// A successful HTTP status or partial text is insufficient for plan inference.
pub(crate) fn completed_stream(bytes: &[u8]) -> Result<Value, Failure> {
    let text = std::str::from_utf8(bytes).map_err(|_| Failure::Provider)?;
    let text = text.replace("\r\n", "\n");
    let mut completed = None;
    let mut finished = std::collections::BTreeMap::new();
    let mut pending = std::collections::BTreeSet::new();
    for event in text.split("\n\n") {
        let data = event
            .lines()
            .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let value: Value = serde_json::from_str(&data).map_err(|_| Failure::Provider)?;
        match value["type"].as_str() {
            Some("response.output_item.added") => {
                let index = value["output_index"]
                    .as_u64()
                    .filter(|v| *v < 256)
                    .ok_or(Failure::Provider)?;
                pending.insert(index);
            }
            Some("response.output_item.done") => {
                let index = value["output_index"]
                    .as_u64()
                    .filter(|v| *v < 256)
                    .ok_or(Failure::Provider)?;
                if !value["item"].is_object()
                    || finished.insert(index, value["item"].clone()).is_some()
                {
                    return Err(Failure::Provider);
                }
                pending.remove(&index);
            }
            Some("response.completed") => completed = Some(value["response"].clone()),
            Some("response.failed") | Some("error") => {
                let code = value
                    .pointer("/response/error/code")
                    .or_else(|| value.get("code"))
                    .and_then(Value::as_str);
                return Err(match code {
                    Some("subscription_sharing_usage_limit_exceeded")
                    | Some("rate_limit_exceeded") => Failure::RateLimited,
                    Some("subscription_sharing_user_not_eligible") => Failure::Consent,
                    _ => Failure::Provider,
                });
            }
            Some("response.incomplete") => return Err(Failure::NoText),
            _ => {}
        }
    }
    let mut response = completed.ok_or(Failure::NoText)?;
    // ChatGPT plan streams can put complete items only in output_item.done and leave
    // response.completed.output empty. Never promote argument/text deltas to completion.
    if response["output"].as_array().is_none_or(Vec::is_empty) && !finished.is_empty() {
        if !pending.is_empty() || finished.keys().copied().ne(0..finished.len() as u64) {
            return Err(Failure::NoText);
        }
        response["output"] = serde_json::Value::Array(finished.into_values().collect());
    }
    Ok(response)
}

/// Report transport failures without reflecting request authentication.
pub(crate) fn transport_failure(error: reqwest::Error) -> Failure {
    if error.is_timeout() {
        Failure::Timeout
    } else {
        Failure::Provider
    }
}

/// Restrict model identifiers to a bounded provider-safe character set.
pub fn validate_model(model: &str) -> Result<(), Failure> {
    if model.is_empty()
        || model.len() > 200
        || !model
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.:/".contains(&c))
    {
        return Err(Failure::InvalidInput);
    }
    Ok(())
}

/// Reject malformed or secret-bearing metadata before contacting the selected provider.
pub fn validate_connection(input: &AiConnectionInput) -> Result<(), Failure> {
    if input.provider == firecrab_api_types::AiProvider::Anthropic && input.model.is_some() {
        return Err(Failure::InvalidInput);
    }
    if let Some(model) = &input.model {
        validate_model(model)?;
    }
    if input.name.trim().is_empty()
        || input.name.len() > 100
        || input.name.chars().any(char::is_control)
        || !(8..=4096).contains(&input.api_key.len())
        || !input.api_key.bytes().all(|c| c.is_ascii_graphic())
        || input.name.contains(&input.api_key)
        || input
            .model
            .as_ref()
            .is_some_and(|model| model.contains(&input.api_key))
    {
        return Err(Failure::InvalidInput);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_stream_requires_completed_and_reports_late_failures() {
        let complete =
            json!({"type":"response.completed","response":{"status":"completed","output":[]}});
        let stream = format!(
            "event: response.created\r\ndata: {{\"type\":\"response.created\"}}\r\n\r\ndata: {complete}\r\n\r\n"
        );
        assert_eq!(
            completed_stream(stream.as_bytes()).unwrap()["status"],
            "completed"
        );
        assert!(matches!(
            completed_stream(
                b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n"
            ),
            Err(Failure::NoText)
        ));
        let failed = json!({"type":"response.failed","response":{"error":{"code":"subscription_sharing_usage_limit_exceeded"}}});
        assert!(matches!(
            completed_stream(format!("data: {complete}\n\ndata: {failed}\n\n").as_bytes()),
            Err(Failure::RateLimited)
        ));
        assert!(completed_stream(b"data: broken\n\n").is_err());
        assert!(matches!(
            completed_stream(b"data: {\"type\":\"response.incomplete\"}\n\n"),
            Err(Failure::NoText)
        ));
    }
    #[test]
    fn plan_stream_reconstructs_only_completed_items_and_still_rejects_late_failure() {
        let done = json!({"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","namespace":"sandbox","name":"sandbox_ready","call_id":"probe","arguments":"{}"}});
        let completed =
            json!({"type":"response.completed","response":{"status":"completed","output":[]}});
        let stream = format!("data: {done}\n\ndata: {completed}\n\n");
        assert_eq!(
            completed_stream(stream.as_bytes()).unwrap()["output"][0]["name"],
            "sandbox_ready"
        );
        let failed =
            json!({"type":"response.failed","response":{"error":{"code":"rate_limit_exceeded"}}});
        assert!(matches!(
            completed_stream(format!("{stream}data: {failed}\n\n").as_bytes()),
            Err(Failure::RateLimited)
        ));
        let unfinished =
            json!({"type":"response.output_item.added","output_index":1,"item":{"type":"message"}});
        assert!(completed_stream(format!("data: {unfinished}\n\n{stream}").as_bytes()).is_err());
    }
}

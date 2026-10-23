//! Fixed provider endpoints and disabled redirects keep credentials scoped.
use std::time::Duration;

use firecrab_api_types::{AiConnection, AiConnectionInput};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub mod anthropic;
mod oauth;
pub use oauth::{LoginRegistry, OAuthRegistration, OAuthSession};

pub const ENDPOINT: &str = "https://api.openai.com/v1/responses";
pub const CREDENTIAL_ENDPOINT: &str = "https://api.openai.com/v1/models";
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

// No Debug: the private record must never print a credential.
#[derive(Clone, Deserialize, Serialize)]
pub struct SavedConnection {
    pub connection: AiConnection,
    pub api_key: String,
    #[serde(default)]
    pub oauth: Option<OAuthSession>,
}

impl SavedConnection {
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

#[derive(Debug, Clone)]
pub struct OpenAi {
    client: Client,
    endpoint: String,
    credential_endpoint: String,
    auth_base: String,
}

#[derive(Debug, Clone, Copy)]
pub enum Failure {
    InvalidInput,
    Authentication,
    ModelDenied,
    RateLimited,
    Timeout,
    Provider,
    NoText,
    Login,
    Consent,
}

impl Failure {
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

async fn response_bytes(mut response: reqwest::Response) -> Result<Vec<u8>, Failure> {
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
fn completed_stream(bytes: &[u8]) -> Result<Value, Failure> {
    let text = std::str::from_utf8(bytes).map_err(|_| Failure::Provider)?;
    let text = text.replace("\r\n", "\n");
    let mut completed = None;
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
    completed.ok_or(Failure::NoText)
}

fn transport_failure(error: reqwest::Error) -> Failure {
    if error.is_timeout() {
        Failure::Timeout
    } else {
        Failure::Provider
    }
}

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
}

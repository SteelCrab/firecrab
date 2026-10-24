//! Pipe AI wire types. Credentials appear only in the connection input.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Private input for verifying and atomically saving an API key connection.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiConnectionInput {
    /// User-visible connection label, independent of any model or project.
    pub name: String,
    /// Provider whose fixed endpoint receives this credential.
    pub provider: AiProvider,
    /// Secret accepted only on input and retained on the API host.
    pub api_key: String,
    /// Optional inference probe for API clients. Account connection needs no model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Provider identity used to keep credentials scoped to one service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiProvider {
    /// OpenAI API key or ChatGPT public-client authentication.
    Openai,
    /// Official Anthropic API key authentication for connection management.
    Anthropic,
}

/// Authentication method represented in public connection metadata.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiAuthentication {
    /// Provider key verified before it is saved.
    #[default]
    ApiKey,
    /// ChatGPT account with a signed identity and server-managed token session.
    Chatgpt,
}

/// Secret-free metadata returned for a saved provider connection.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConnection {
    /// Stable client-generated identity used for replacement and disconnect.
    pub id: Uuid,
    /// Display label supplied by the connection owner.
    pub name: String,
    /// Service that owns the saved credentials.
    pub provider: AiProvider,
    /// Credential mechanism; older records default to an API key.
    #[serde(default)]
    pub authentication: AiAuthentication,
    /// Fixed provider endpoint associated with this connection's capabilities.
    pub endpoint: String,
    /// Only models that returned text through the configured credentials.
    pub models: Vec<String>,
    /// Whether this adapter exposes incremental responses to its clients.
    pub streaming: bool,
    /// Whether this adapter supports model-requested tool execution.
    pub tool_calls: bool,
}

/// Saved or request-specific reference to an inference-verified model.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiModelSelection {
    /// Saved account connection whose credentials authorize the request.
    pub connection_id: Uuid,
    /// Provider model identifier previously verified on that connection.
    pub model: String,
}

/// Explicit request for a potentially billed model permission probe.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiVerifyRequest {
    /// Model that must return a complete text response to become selectable.
    pub model: String,
}

/// Text request using an explicit model selection or a saved project default.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiResponseRequest {
    /// Identity owned by the caller's project. This slice stores model settings only.
    pub project_id: Uuid,
    /// Request/task override. Omission uses the saved project default.
    pub selection: Option<AiModelSelection>,
    /// User text; saved credentials are redacted before provider submission.
    pub input: String,
}

/// Buffered text response and the model identity that produced it.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiResponse {
    /// Effective connection and model after applying any request override.
    pub selection: AiModelSelection,
    /// Complete assistant text with saved credentials redacted.
    pub text: String,
}

/// Start a ChatGPT login without accepting a password or token from the caller.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiLoginRequest {
    /// Label to save only after account authentication succeeds.
    pub name: String,
    /// Inference permission is separate from account authentication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Browser handoff for a bounded, single-use ChatGPT login attempt.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiLoginStart {
    /// Opaque attempt identity used for polling and callback state validation.
    pub login_id: Uuid,
    /// Official authorization URL containing state and a PKCE challenge.
    pub authorization_url: String,
    /// Login lifetime in seconds; attempts do not survive an API restart.
    pub expires_in: u64,
}

/// Pollable login progress with sanitized failure information.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiLoginStatus {
    /// Pending, working, connected, or failed state of the login attempt.
    pub status: String,
    /// Public error message; provider responses and credentials are omitted.
    pub message: Option<String>,
}

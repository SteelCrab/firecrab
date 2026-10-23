//! Pipe AI wire types. Credentials appear only in the connection input.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiConnectionInput {
    pub name: String,
    pub provider: AiProvider,
    pub api_key: String,
    /// Optional inference probe for API clients. Account connection needs no model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiProvider {
    Openai,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiAuthentication {
    #[default]
    ApiKey,
    Chatgpt,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConnection {
    pub id: Uuid,
    pub name: String,
    pub provider: AiProvider,
    #[serde(default)]
    pub authentication: AiAuthentication,
    pub endpoint: String,
    /// Only models that returned text through the configured credentials.
    pub models: Vec<String>,
    pub streaming: bool,
    pub tool_calls: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiModelSelection {
    pub connection_id: Uuid,
    pub model: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiVerifyRequest {
    pub model: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiResponseRequest {
    /// Identity owned by the caller's project. This slice stores model settings only.
    pub project_id: Uuid,
    /// Request/task override. Omission uses the saved project default.
    pub selection: Option<AiModelSelection>,
    pub input: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiResponse {
    pub selection: AiModelSelection,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiLoginRequest {
    pub name: String,
    /// Inference permission is separate from account authentication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiLoginStart {
    pub login_id: Uuid,
    pub authorization_url: String,
    pub expires_in: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiLoginStatus {
    pub status: String,
    pub message: Option<String>,
}

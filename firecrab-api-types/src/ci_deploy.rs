//! Narrow CI credentials and image-only Deploy requests. No service configuration is exposed.
use crate::{DeployStrategy, ServicePhase};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Public credential metadata. The secret is returned only at creation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeployToken {
    pub id: Uuid,
    pub service_id: Uuid,
    pub name: String,
    pub repository: String,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateDeployToken {
    pub name: String,
    #[serde(default = "default_lifetime")]
    pub expires_in_seconds: u32,
}

fn default_lifetime() -> u32 {
    30 * 86400
}

/// One-time response; deliberately has no Debug implementation.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedDeployToken {
    pub credential: DeployToken,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CiDeployRequest {
    pub operation_id: Uuid,
    pub expected_release_id: Uuid,
    pub image: String,
    #[serde(default)]
    pub strategy: DeployStrategy,
}

/// Current release information, without environment, bindings, VM details or logs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CiServiceStatus {
    pub service_id: Uuid,
    pub release_id: Uuid,
    pub image: String,
    pub phase: ServicePhase,
}

/// Exact CI attempt outcome; restored older releases cannot turn failure into success.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CiDeployStatus {
    pub service_id: Uuid,
    pub operation_id: Uuid,
    pub expected_release_id: Uuid,
    pub image: String,
    pub strategy: DeployStrategy,
    pub phase: ServicePhase,
    pub completed: bool,
    pub succeeded: bool,
    pub digest: Option<String>,
}

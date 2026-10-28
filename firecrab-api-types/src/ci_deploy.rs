//! Narrow CI credentials and image-only Deploy requests. No service configuration is exposed.
use crate::{DeployStrategy, ServicePhase};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Public credential metadata. The secret is returned only at creation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeployToken {
    /// Revocation identity; never a usable credential by itself.
    pub id: Uuid,
    /// Exact service authorized by the bearer secret.
    pub service_id: Uuid,
    /// Operator label, limited to 64 bytes without control characters.
    pub name: String,
    /// Canonical registry/repository captured from the running service.
    pub repository: String,
    /// Issuance time as Unix seconds.
    pub created_at: i64,
    /// Exclusive expiry in Unix seconds; there are no permanent tokens.
    pub expires_at: i64,
}

/// Operator-only issuance; the service and repository cannot be chosen in this body.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateDeployToken {
    /// Human-readable purpose shown in the operator's token list.
    pub name: String,
    /// Finite lifetime: 60 seconds to 90 days, defaulting to 30 days.
    #[serde(default = "default_lifetime")]
    pub expires_in_seconds: u32,
}

/// Omitting lifetime must still produce an expiring credential.
fn default_lifetime() -> u32 {
    30 * 86400
}

/// One-time response; deliberately has no Debug implementation.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedDeployToken {
    /// Safe metadata that remains available after the one-time response.
    pub credential: DeployToken,
    /// Plaintext bearer value, which the server never stores or reveals again.
    pub token: String,
}

/// Image-only request with an explicit compare-and-swap release and retry identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CiDeployRequest {
    /// Non-nil identity reused unchanged after a lost acknowledgement.
    pub operation_id: Uuid,
    /// Active release reviewed by this pipeline; stale submissions fail.
    pub expected_release_id: Uuid,
    /// Fully qualified image pinned by SHA-256 in the token's exact repository.
    pub image: String,
    /// Rolling by default; blue-green still requires full surge capacity.
    #[serde(default)]
    pub strategy: DeployStrategy,
}

/// Current release information, without environment, bindings, VM details or logs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CiServiceStatus {
    /// Stable service identity fixed by the token and ingress route.
    pub service_id: Uuid,
    /// Current journal identity to use as the submission's expected release.
    pub release_id: Uuid,
    /// Image reference only; other configuration stays private.
    pub image: String,
    /// Current progress, which may belong to another accepted operation.
    pub phase: ServicePhase,
}

/// Exact CI attempt outcome; restored older releases cannot turn failure into success.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CiDeployStatus {
    /// Service owning the exact CI request.
    pub service_id: Uuid,
    /// Durable attempt identity; this is never inferred from the active release.
    pub operation_id: Uuid,
    /// Original compare-and-swap release retained for retry validation.
    pub expected_release_id: Uuid,
    /// Original immutable image request, including an OCI index pin when used.
    pub image: String,
    /// Admitted rollout strategy, immutable across retries.
    pub strategy: DeployStrategy,
    /// Observed progress, frozen when the first terminal receipt is saved.
    pub phase: ServicePhase,
    /// Whether CI has a terminal result, including failed recovery or cancellation.
    pub completed: bool,
    /// True only after the requested Deploy reached a successful release.
    pub succeeded: bool,
    /// Verified host-platform manifest; it can differ from a requested OCI index.
    pub digest: Option<String>,
}

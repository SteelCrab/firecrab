//! Shared typed resource plans. Public results omit desired configuration values.

use crate::{ManagedResource, ResourceOwner, ResourceOwnership};
use firecrab_api_types::{AiModelSelection, CreateShellRequest, SandboxAccessPolicy};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Desired persistent configuration. Unknown resource variants fail before admission.
/// This type intentionally has no Debug implementation.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    content = "configuration",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ResourceConfiguration {
    /// Versioned guest script, including all persisted user-writable metadata.
    Shell(CreateShellRequest),
    /// Project-local default referencing an inference-verified connection model.
    AiModel(AiModelConfiguration),
    /// Exact delegated-host access policy; application resources remain borrowed.
    SandboxAccess(SandboxAccessConfiguration),
}

impl ResourceConfiguration {
    /// Stable inventory namespace for this typed configuration.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Shell(_) => "shell",
            Self::AiModel(_) => "ai_model",
            Self::SandboxAccess(_) => "sandbox_access",
        }
    }
}

/// Complete model-default scope and its exact verified connection reference.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AiModelConfiguration {
    /// Logical project scope; configuring a default does not create a project.
    pub project_id: Uuid,
    /// Selected connection UUID and inference-verified provider model name.
    pub selection: AiModelSelection,
}

/// Full operator-managed access policy on the selected controller.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxAccessConfiguration {
    /// Exact mTLS host UUID; policies can explicitly reserve future project UUIDs.
    pub principal: Uuid,
    /// Rights over exact resources. No right is implied by another permission.
    pub policy: SandboxAccessPolicy,
    /// Explicit new operator intent before re-creating a previously revoked policy.
    /// Omission acknowledges only the never-managed generation zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation_generation: Option<u64>,
}

/// Read-only reference proof without credential values or fingerprints.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourcePlanDependency {
    /// Original dependency endpoint, identity and ownership generation.
    pub ownership: ResourceOwnership,
    /// Source configuration version; automatic session refresh preserves it.
    pub configuration_generation: u64,
}

/// Immutable input to an advisory plan; a retry must use the same normalized request.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanResource {
    /// Stable plan identity, retained after a lost HTTP response.
    pub operation_id: Uuid,
    /// Persistent API database identity, independent of its URL.
    pub expected_endpoint_id: Uuid,
    /// Exact workflow address. Existing resources require prior explicit adoption.
    pub owner: ResourceOwner,
    /// None requests a new identity. Existing identities are never inferred by name.
    pub resource: Option<ManagedResource>,
    /// None requests deletion of the exact existing owned resource.
    pub desired: Option<ResourceConfiguration>,
}

/// The action approved by a frozen plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourcePlanAction {
    /// Create the reserved exact identity and its ownership atomically.
    Create,
    /// Preserve identity and update only the reviewed configuration.
    Update,
    /// Configuration already matches; no revision or runtime action is created.
    Noop,
    /// Delete only the exact owned configuration after reference checks.
    Delete,
}

/// Public advisory result; field names replace desired and observed values.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourcePlan {
    /// Original retained plan identity.
    pub operation_id: Uuid,
    /// Hash of the complete frozen plan, required for execution.
    pub plan_sha256: String,
    /// Exact target, endpoint and authority observed during planning.
    pub ownership: ResourceOwnership,
    /// Workflow address requested by the authenticated operator.
    pub owner: ResourceOwner,
    /// Reviewed resource action.
    pub action: ResourcePlanAction,
    /// Names of changed configuration fields, without their values.
    pub changed_fields: Vec<String>,
    /// Server deadline for first admission. Completed acknowledgements never expire.
    pub expires_at_ms: u64,
    /// Exact dependency authority and source versions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<ResourcePlanDependency>,
}

/// Execute only the previously reviewed plan; no replacement configuration is accepted.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplyResourcePlan {
    /// Exact namespace observed while reviewing the plan.
    pub expected_endpoint_id: Uuid,
    /// Exact digest returned with the original advisory plan.
    pub plan_sha256: String,
}

/// Immutable completion receipt, readable after later drift, transfer or deletion.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourcePlanReceipt {
    /// Original approved plan.
    pub plan: ResourcePlan,
    /// Ownership committed by that operation, independent of the current owner.
    pub ownership: ResourceOwnership,
    /// Original output revision, when the configuration has immutable revisions.
    pub revision_id: Option<Uuid>,
}

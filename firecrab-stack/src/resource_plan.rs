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
    /// Encrypted host Secret metadata and a separate, versioned private value input.
    Secret(SecretConfiguration),
    /// Complete ordered IPv4 allow policy on one immutable network identity.
    SecurityGroup(SecurityGroupConfiguration),
    /// Explicit local service telemetry export; null endpoint disables it.
    ServiceOtlp(ServiceOtlpConfiguration),
}

impl ResourceConfiguration {
    /// Stable inventory namespace for this typed configuration.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Shell(_) => "shell",
            Self::AiModel(_) => "ai_model",
            Self::SandboxAccess(_) => "sandbox_access",
            Self::Secret(_) => "secret",
            Self::SecurityGroup(_) => "security_group",
            Self::ServiceOtlp(_) => "service_otlp",
        }
    }
}

/// Independent, opt-in collector configuration for an existing local service.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceOtlpConfiguration {
    /// Existing local service UUID; this setting does not create a service.
    pub service_id: Uuid,
    /// Credential-free HTTP(S) base URL, or explicit null to disable export.
    #[serde(deserialize_with = "explicit_endpoint")]
    pub endpoint: Option<String>,
}

fn explicit_endpoint<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

/// Saved policy configuration; rules activate when an affected VM next starts.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecurityGroupConfiguration {
    /// Display name, with the ordinary 1–64 byte policy-name limit.
    pub name: String,
    /// Existing IPv4-only network. Changing it requires a separate replacement.
    pub micro_network_id: Uuid,
    /// Complete ordered allow list. Empty means default deny.
    pub rules: Vec<firecrab_api_types::SecurityGroupRule>,
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

/// Credential references and write-only rotation markers never contain values.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CredentialInput {
    /// Copy an exact existing host Secret version during first admission.
    Secret {
        /// Exact UUID on the selected API endpoint.
        id: Uuid,
        /// Original version; no lookup by name or fallback to a newer value.
        version: u64,
    },
    /// Supply private bytes through the plan-bound input endpoint during apply.
    WriteOnly {
        /// Non-secret user-chosen rotation marker; change it to request new input.
        version: u64,
    },
}

/// Complete user-controlled metadata and optional value provisioning for a Secret.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecretConfiguration {
    /// Unique local name, following the ordinary PipeSecrets naming contract.
    pub name: String,
    /// Omission sets an empty description.
    #[serde(default)]
    pub description: String,
    /// Omission retains an existing value; a newly created Secret requires input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<CredentialInput>,
}

/// Explicit data removal consent, frozen before any destructive work.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceRemovalPolicy {
    /// Omission retains application/private data; deletion requires explicit consent.
    #[serde(default)]
    pub delete_data: bool,
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
    /// Resource-specific data consent for removal; omitted policies retain data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removal: Option<ResourceRemovalPolicy>,
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
    /// Exact reviewed data policy for resources with retained private data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removal: Option<ResourceRemovalPolicy>,
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

/// Private input endpoint body. Values must never be logged, planned or returned.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UploadResourceValue {
    /// Exact endpoint retained by the reviewed plan.
    pub expected_endpoint_id: Uuid,
    /// Exact reviewed scope; a value cannot be moved between plans or owners.
    pub plan_sha256: String,
    /// Marker from the plan's write-only credential input.
    pub version: u64,
    /// Write-only private value, including an explicitly supplied empty value.
    pub value: String,
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

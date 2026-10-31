//! Cross-workflow ownership identities. Configuration and credentials are not stored here.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ResourceKind, logical_name};

/// A resource identity on the selected API host or controller.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ManagedResource {
    /// Stable resource kind from the v0.4.0 inventory.
    pub kind: String,
    /// Exact UUID or the documented stable identity for non-UUID resources.
    pub id: String,
}

impl ManagedResource {
    /// Reject unknown kinds and normalize UUID aliases before ownership lookup.
    pub fn normalize(&mut self) -> Result<(), &'static str> {
        if !resource_kinds().contains(&self.kind)
            || self.id.is_empty()
            || self.id.len() > 512
            || self.id.trim() != self.id
            || self.id.chars().any(char::is_control)
        {
            return Err("provide an inventory kind and an exact resource identity");
        }
        if let Ok(id) = Uuid::parse_str(&self.id) {
            if id.is_nil() {
                return Err("resource UUID must be non-nil");
            }
            self.id = id.to_string();
        }
        if self.kind == "auth" && !matches!(self.id.as_str(), "firecrab" | "aws" | "github") {
            return Err("use the exact saved Auth provider key");
        }
        if self.kind == "registry" {
            self.id = self.id.to_ascii_lowercase();
            if self.id == "registry-1.docker.io" {
                self.id = "docker.io".into();
            }
            if self.id.len() > 255
                || !self
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b".:-[]".contains(&byte))
                || !(self.id.contains('.') || self.id.contains(':') || self.id == "localhost")
            {
                return Err("use the exact registry hostname and optional port");
            }
        }
        if matches!(
            self.kind.as_str(),
            "network"
                | "vm"
                | "service"
                | "storage_root"
                | "volume"
                | "security_group"
                | "shell"
                | "schedule"
                | "pool"
                | "queue"
                | "database_connection"
                | "storage_connection"
                | "secret"
                | "secret_connection"
                | "ai_connection"
                | "ai_model"
                | "sandbox_environment"
                | "sandbox_project"
                | "stack_source"
        ) && Uuid::parse_str(&self.id).is_err()
        {
            return Err("this resource kind requires a UUID");
        }
        Ok(())
    }

    /// Existing native Stack namespaces, kept for backwards-compatible bindings.
    pub fn legacy_kind(&self) -> Option<ResourceKind> {
        self.kind.parse().ok()
    }
}

/// Workflow responsible for desired configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Manager {
    /// Native Stack or its configured Git source.
    Stack,
    /// A specific Terraform state owner.
    Terraform,
}

/// A stable workflow identity and its logical resource address.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ResourceOwner {
    /// Workflow type; transport credentials do not serve as ownership IDs.
    pub manager: Manager,
    /// Stack registration UUID or Terraform state-owner UUID.
    pub id: Uuid,
    /// Native logical key or Terraform resource address within that owner.
    pub key: String,
}

impl ResourceOwner {
    /// Validate public metadata without accepting command or credential values.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.id.is_nil()
            || self.key.is_empty()
            || self.key.len() > 256
            || self.key.trim() != self.key
            || self.key.chars().any(char::is_control)
            || (self.manager == Manager::Stack && !logical_name(&self.key))
        {
            return Err("provide a non-nil owner UUID and a valid logical address");
        }
        Ok(())
    }
}

/// Versioned ownership observation. A released record keeps its generation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceOwnership {
    /// Persistent identity of the selected API database, independent of its URL.
    pub endpoint_id: Uuid,
    /// Identity observed on this endpoint, independent of its display name.
    pub resource: ManagedResource,
    /// Increases when ownership changes, including release and native rebind.
    pub generation: u64,
    /// Current writer; null does not assert that the resource exists.
    pub owner: Option<ResourceOwner>,
}

/// Immutable acknowledgement of a committed ownership operation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipOperation {
    /// Caller-retained retry identity.
    pub operation_id: Uuid,
    /// Original result, even if a later operation changed the current owner.
    pub ownership: ResourceOwnership,
}

/// Atomic ownership change and its durable lost-acknowledgement retry identity.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ChangeResourceOwner {
    /// Caller-retained UUID; replay must use the same normalized request.
    pub operation_id: Uuid,
    /// Observed endpoint identity; a different host must reject the request.
    pub expected_endpoint_id: Uuid,
    /// Exact resource to adopt, transfer or release.
    pub resource: ManagedResource,
    /// Last observed generation; protects against release/reclaim races.
    pub expected_generation: u64,
    /// Last observed exact owner; no owner may be overwritten implicitly.
    pub expected_owner: Option<ResourceOwner>,
    /// Desired owner, or null for explicit release while retaining the resource.
    pub owner: Option<ResourceOwner>,
}

impl ChangeResourceOwner {
    /// Normalize retry identity and reject invalid requests before persistence.
    pub fn validate(&mut self) -> Result<(), &'static str> {
        self.resource.normalize()?;
        if self.operation_id.is_nil() || self.expected_endpoint_id.is_nil() {
            return Err("operation and endpoint UUIDs must be non-nil");
        }
        for owner in [&self.expected_owner, &self.owner].into_iter().flatten() {
            owner.validate()?;
        }
        Ok(())
    }
}

/// All service kinds in the frozen contract; runtime support is reported separately.
pub fn resource_kinds() -> &'static [String] {
    #[derive(Deserialize)]
    struct Catalog {
        resources: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        id: String,
    }
    static KINDS: OnceLock<Vec<String>> = OnceLock::new();
    KINDS.get_or_init(|| {
        let catalog: Catalog =
            serde_json::from_str(include_str!("../../packaging/iac-v0.4.0.json"))
                .expect("the build includes the checked IaC inventory");
        catalog
            .resources
            .into_iter()
            .map(|entry| entry.id)
            .collect()
    })
}

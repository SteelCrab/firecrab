//! Public PipeStorage metadata. AWS signing material never belongs in these responses.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Source of signing material, resolved only on the API host.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StorageCredentialRef {
    /// Reuse the AWS connection in Auth.
    #[default]
    PipeAuthAws,
    /// Use the private credential attached to this storage connection.
    Connection,
}

/// Configuration for one existing AWS S3 general-purpose bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PipeStorageSpec {
    /// Display name of this connection.
    pub name: String,
    /// Existing bucket name, without a scheme or path.
    pub bucket: String,
    /// Region used for the endpoint and signing scope.
    pub region: String,
    /// Optional directory prefix, ending in `/` when nonempty.
    #[serde(default)]
    pub prefix: String,
    /// Credential reference; the default reuses Auth AWS.
    #[serde(default)]
    pub credential_ref: StorageCredentialRef,
}

/// Service using a saved connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageServiceRef {
    /// Stable service identifier.
    pub id: Uuid,
    /// Display name of the service.
    pub name: String,
}

/// Saved connection and current service references, with no credential values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipeStorageResponse {
    /// Identifier used by storage routes and service bindings.
    pub id: Uuid,
    /// Saved configuration.
    #[serde(flatten)]
    pub spec: PipeStorageSpec,
    /// Services that must be unbound before editing or disconnecting.
    pub services: Vec<StorageServiceRef>,
}

/// A key returned by ListObjectsV2. Keys include the configured prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageObject {
    /// Full S3 object key.
    pub key: String,
    /// Object size in bytes.
    pub size: u64,
    /// Last modification time as reported by S3.
    pub last_modified: String,
}

/// One bounded listing page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageObjectPage {
    /// Listed directory prefix.
    pub prefix: String,
    /// Objects immediately inside the selected prefix.
    pub objects: Vec<StorageObject>,
    /// Child directory prefixes.
    pub prefixes: Vec<String>,
    /// Opaque S3 continuation token for the same prefix.
    pub next_cursor: Option<String>,
}

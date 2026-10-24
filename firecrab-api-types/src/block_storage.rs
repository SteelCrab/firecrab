//! Persistent data disks and their exclusive MicroVM attachments.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Body for `POST /api/microblockstorage`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CreateMicroBlockStorageRequest {
    /// Unique host-local name, using the same syntax as VM names.
    pub name: String,
    /// Disk capacity in GiB (1–65535).
    pub size_gib: u16,
    /// Registered storage root; omitted uses the default root.
    #[serde(default)]
    pub storage_root: Option<String>,
}

/// An existing volume selected at VM creation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct VmBlockStorageMount {
    /// MicroBlockStorage UUID.
    pub volume_id: Uuid,
    /// Absolute directory inside the guest.
    pub mount_path: String,
}

/// Body for `POST /api/microblockstorage/{id}/attach`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AttachMicroBlockStorageRequest {
    /// Inactive user VM that will own the disk.
    pub vm_id: Uuid,
    /// Absolute directory inside the guest.
    pub mount_path: String,
}

/// Durable volume metadata; host paths are derived rather than exposed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MicroBlockStorageResponse {
    /// Stable disk identifier, also used as its filesystem UUID.
    pub id: Uuid,
    /// Unique host-local volume name.
    pub name: String,
    /// Virtual disk capacity in GiB.
    pub size_gib: u16,
    /// Storage pool identifier; never an arbitrary host path.
    pub storage_root: String,
    /// `creating`, `available`, `attached`, or `deleting`.
    pub state: String,
    /// Remains reserved while its VM is stopped or the API restarts.
    pub attached_vm_id: Option<Uuid>,
    /// Requested directory, absent for an unattached volume.
    pub mount_path: Option<String>,
}

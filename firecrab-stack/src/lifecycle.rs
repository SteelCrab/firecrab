//! Explicit destructive-change policies and retained disk identities.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Destructive work requires a reviewed manifest opt-in, including in Git mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LifecyclePolicy {
    /// Delete and recreate resources whose immutable configuration changed.
    #[serde(default)]
    pub replace: bool,
    /// Delete exact owned resources removed from the declaration.
    #[serde(default)]
    pub prune: bool,
    /// Also remove standalone VM disk artifacts. Defaults to retaining them.
    #[serde(default)]
    pub delete_vm_disks: bool,
}

impl LifecyclePolicy {
    /// Whether omitting the policy preserves the same canonical declaration.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Recoverable disk location without host paths, guest data or environment values.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VmDiskIdentity {
    /// Registered storage ID whose `vms` directory contains this VM's artifacts.
    pub storage_root: String,
    /// Rootfs generation, when the VM has prepared a disk.
    pub generation: Option<Uuid>,
}

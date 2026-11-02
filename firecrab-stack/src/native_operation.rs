//! Durable native mutations that must keep authority beyond an HTTP waiter.

use crate::ResourceOwnership;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A typed backend action; public callers cannot supply arbitrary commands or paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeAction {
    /// Install the original immutable guest kernel and retain its verified source.
    InstallKernel,
    /// Remove only an unused original guest kernel artifact.
    DeleteKernel,
    /// Prepare managed delivery using the original broker UUIDs and retained disks.
    PrepareDatabaseBroker,
    /// Reconcile or repair the original owned delivery cluster.
    RepairDatabaseBroker,
    /// Disconnect delivery and stop owned guests while retaining their disks.
    DisconnectDatabaseBroker,
    /// Verify and select a delivery broker without declaring or deleting queues.
    ConfigureDatabaseBroker,
    /// Set up owned event capture, delivery queues and a private inbox.
    CreateDatabaseEvents,
    /// Change the saved delivery pause state without replaying application data.
    PauseDatabaseEvents,
    /// Remove owned capture while retaining its outbox and delivery evidence.
    DetachDatabaseEvents,
    /// Remove only drained, detached delivery configuration and owned queues.
    DeleteDatabaseEvents,
    /// Provision one private service account with stable generated credentials.
    CreateDatabaseBinding,
    /// Revoke only the exact binding-owned service account.
    DeleteDatabaseBinding,
    /// Verify and save one connection using its reserved UUID.
    CreateDatabaseConnection,
    /// Verify and replace connection settings without changing its UUID.
    UpdateDatabaseConnection,
    /// Remove one inactive VM after recording its immutable disk-retention policy.
    DeleteVm,
    /// Revoke one scoped credential and invalidate its previous management authority.
    RevokeDeployToken,
    /// Install the broker using its one reserved VM and disk identity.
    InstallQueueEngine,
    /// Start or repair the same broker; never replace an existing disk.
    StartQueueEngine,
    /// Stop the broker while retaining all declarations and messages.
    StopQueueEngine,
    /// Delete one project's private workspace and metadata; borrowed sources survive.
    DeleteSandboxProject,
    /// Disconnect Git configuration without deleting its Stack or infrastructure.
    DeleteStackSource,
    /// Remove delegated permissions and invalidate the previous management authority.
    RevokeSandboxAccess,
}

/// Native work remains fenced until completion or explicit recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeOperationState {
    /// A server worker may be changing the resource.
    Running,
    /// The outcome is uncertain; ownership and identity stay reserved.
    NeedsRecovery,
    /// The recorded action completed; this acknowledgement never changes.
    Succeeded,
    /// Verification failed before any managed side effect; corrected input may use a new operation.
    Failed,
}

/// Public operation metadata contains no desired values, credentials or filesystem paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeOperation {
    /// Stable operation UUID retained across recovery.
    pub operation_id: Uuid,
    /// Exact endpoint, resource and authority captured before dispatch.
    pub ownership: ResourceOwnership,
    /// Explicit backend action accepted by the server.
    pub action: NativeAction,
    /// Durable completion or recovery status.
    pub state: NativeOperationState,
    /// Static failure category, without upstream messages or supplied values.
    pub error_code: Option<String>,
    /// Recovery advances the attempt fence; workers from an older attempt cannot commit.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub execution_generation: u64,
    /// Original configuration generation for a versioned metadata removal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration_generation: Option<u64>,
    /// Immutable VM artifact policy. Other native actions omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain_disks: Option<bool>,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// A bounded page of active work, including resources without an IaC owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeOperationsPage {
    /// Running and uncertain operations only; completed receipts remain readable by UUID.
    pub operations: Vec<NativeOperation>,
    /// Fetch the next page with `before`; ordering uses canonical operation UUIDs.
    pub next_before: Option<uuid::Uuid>,
}

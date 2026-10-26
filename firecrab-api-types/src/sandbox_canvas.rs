//! Contract-only Canvas handoff. This creates no Canvas sessions, agents or Runtime PTYs.
use crate::{
    SandboxArtifact, SandboxEnvironmentRef, SandboxExecutionMode, SandboxPermission,
    SandboxRuntimeRef, SandboxTask, SandboxTaskState,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Canvas roles restrict an already authorized transport caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxCanvasRole {
    /// Read authorized task and result projections.
    Viewer,
    /// Edit shared content; execution still needs explicit grants.
    Editor,
    /// Manage Canvas sharing within the caller's existing authority.
    Owner,
}
/// Proposed Canvas operation whose resource rights must be checked by the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxCanvasAction {
    /// Read an existing Sandbox task.
    ViewTask,
    /// Add text to the Canvas shared conversation.
    WriteSharedChat,
    /// Request the terminal input-control lease.
    AcquireTerminalControl,
    /// Send terminal input while holding its control lease.
    InputTerminal,
    /// Start an agent through the existing Sandbox admission API.
    RunAgent,
    /// Request cancellation of the referenced agent attempt.
    CancelAgent,
    /// Apply reviewed project changes through Sandbox.
    ApplyChanges,
    /// Change Canvas membership or sharing settings.
    ManageSharing,
}
impl SandboxCanvasAction {
    /// Existing Sandbox rights must be checked on every exact referenced resource.
    pub fn permission(self) -> SandboxPermission {
        match self {
            Self::ViewTask | Self::WriteSharedChat => SandboxPermission::Read,
            Self::ApplyChanges => SandboxPermission::Apply,
            Self::ManageSharing => SandboxPermission::Write,
            _ => SandboxPermission::Execute,
        }
    }
}
impl SandboxCanvasRole {
    /// `caller_authorized` is derived by the server's existing Caller/resource checks.
    /// It is never accepted from a share link, browser input or Canvas role.
    pub fn allows(
        self,
        action: SandboxCanvasAction,
        caller_authorized: bool,
        explicit_agent_grant: bool,
        terminal_controller: bool,
    ) -> bool {
        if !caller_authorized {
            return false;
        }
        if action == SandboxCanvasAction::ViewTask {
            return true;
        }
        if self == Self::Viewer {
            return false;
        }
        match action {
            SandboxCanvasAction::ManageSharing => self == Self::Owner,
            SandboxCanvasAction::RunAgent | SandboxCanvasAction::CancelAgent => {
                explicit_agent_grant
            }
            SandboxCanvasAction::InputTerminal => terminal_controller,
            _ => true,
        }
    }
}
/// Immutable references to one engine-owned attempt; roles do not replace these identities.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxCanvasTaskRef {
    /// Project whose current permissions authorize access.
    pub project_id: Uuid,
    /// Authoritative Sandbox task identity.
    pub task_id: Uuid,
    /// Immutable execution attempt; reconnect cannot select another attempt.
    pub attempt_id: Uuid,
    /// Admitted VM generation, absent for tasks that do not execute in a guest.
    pub runtime: Option<SandboxRuntimeRef>,
    /// Managed environment generation and role captured at admission.
    pub environment: Option<SandboxEnvironmentRef>,
}
/// Results remain owned by Sandbox; Canvas stores references and authorized projections.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxCanvasResultRef {
    /// Task that owns the result.
    pub task_id: Uuid,
    /// Attempt that produced the result and its artifacts.
    pub attempt_id: Uuid,
    /// Current Sandbox lifecycle state, independent of individual test outcomes.
    pub state: SandboxTaskState,
    /// Last authoritative task update in Unix milliseconds.
    pub updated_at: u64,
    /// Artifact metadata; downloads remain subject to Sandbox authorization.
    pub artifacts: Vec<SandboxArtifact>,
}
/// A cursor identifies an attempt and ordered persisted Sandbox events, never another shell.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxCanvasEventCursor {
    /// Task whose persisted event stream is being observed.
    pub task_id: Uuid,
    /// Attempt that owns the cursor's sequence numbers.
    pub attempt_id: Uuid,
    /// Last observed event, absent when no events have been retained.
    pub sequence: Option<usize>,
    /// Task revision timestamp in Unix milliseconds.
    pub updated_at: u64,
}
/// Reconnect to persisted task history without creating or re-executing work.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxCanvasReconnect {
    /// Exact authorized attempt to reconnect.
    pub task: SandboxCanvasTaskRef,
    /// Last event projection known to the client.
    pub after: SandboxCanvasEventCursor,
    /// Refetch the authoritative task when a requested replay range is no longer retained.
    pub replay_reset_required: bool,
}
/// Cancellation only refers to the existing task/attempt. It never deletes a VM or Canvas.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxCanvasCancelRef {
    /// Task to cancel through Sandbox's existing lifecycle.
    pub task_id: Uuid,
    /// Expected attempt, checked before cancellation.
    pub attempt_id: Uuid,
}
/// Explicit policy for agent observation when a Canvas client disconnects.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxCanvasDisconnectPolicy {
    /// Browser disconnect performs no execution or resource mutation.
    #[default]
    ObserveOnly,
    /// Continuation still requires the user's saved execution policy and current grants.
    ContinueAuthorizedAgent,
    /// Request cancellation; termination still needs confirmation from Sandbox.
    RequestAgentCancellation,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// Resource ownership and export requirements carried into the Canvas projection.
pub struct SandboxCanvasRetention {
    /// Environment that owns managed execution resources, when present.
    pub environment: Option<SandboxEnvironmentRef>,
    /// Captured guest execution and persistence mode.
    pub execution_mode: Option<SandboxExecutionMode>,
    /// Artifacts that must be exported before temporary resource cleanup.
    pub required_artifact_ids: Vec<Uuid>,
    /// Cleanup must first confirm durable export of required results.
    pub export_required: bool,
    /// The runtime is borrowed and must survive task or Canvas deletion.
    pub borrowed_runtime: bool,
    /// Final retained data deletion needs explicit user authorization.
    pub workspace_deletion_requires_authorization: bool,
    /// Disconnect behavior; defaults to observation without mutation.
    pub disconnect_policy: SandboxCanvasDisconnectPolicy,
}
/// Read-only handoff snapshot. There is no second admission or task state machine.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxCanvasHandoff {
    /// Contract version; currently 1.
    pub version: u16,
    /// Immutable authoritative task, attempt and runtime references.
    pub task: SandboxCanvasTaskRef,
    /// Authorized result projection from the same attempt.
    pub result: SandboxCanvasResultRef,
    /// Cursor and replay-reset guidance for reconnecting clients.
    pub reconnect: SandboxCanvasReconnect,
    /// Cancellation target; does not authorize resource deletion.
    pub cancellation: SandboxCanvasCancelRef,
    /// Export, ownership and data retention boundaries.
    pub retention: SandboxCanvasRetention,
}
impl SandboxCanvasHandoff {
    /// Build references only from an authorized authoritative Sandbox task response.
    pub fn from_task(
        task: &SandboxTask,
        execution_mode: Option<SandboxExecutionMode>,
        required_artifact_ids: Vec<Uuid>,
    ) -> Self {
        let reference = SandboxCanvasTaskRef {
            project_id: task.project_id,
            task_id: task.id,
            attempt_id: task.attempt_id,
            runtime: task.runtime,
            environment: task.environment.clone(),
        };
        Self {
            version: 1,
            task: reference.clone(),
            result: SandboxCanvasResultRef {
                task_id: task.id,
                attempt_id: task.attempt_id,
                state: task.state,
                updated_at: task.updated_at,
                artifacts: task.artifacts.clone(),
            },
            reconnect: SandboxCanvasReconnect {
                task: reference,
                after: SandboxCanvasEventCursor {
                    task_id: task.id,
                    attempt_id: task.attempt_id,
                    sequence: task.events.last().map(|e| e.sequence),
                    updated_at: task.updated_at,
                },
                replay_reset_required: false,
            },
            cancellation: SandboxCanvasCancelRef {
                task_id: task.id,
                attempt_id: task.attempt_id,
            },
            retention: SandboxCanvasRetention {
                environment: task.environment.clone(),
                workspace_deletion_requires_authorization: execution_mode
                    == Some(SandboxExecutionMode::PersistentWorkspace)
                    || task.environment.is_none(),
                execution_mode,
                export_required: task.environment.is_some(),
                borrowed_runtime: task.runtime.is_some() && task.environment.is_none(),
                required_artifact_ids,
                disconnect_policy: Default::default(),
            },
        }
    }
    /// Reject mixed projects, attempts, boot generations or artifact owners before replay.
    pub fn validate(&self) -> Result<(), &'static str> {
        let task = &self.task;
        if self.version != 1
            || task.project_id.is_nil()
            || task.task_id.is_nil()
            || task.attempt_id.is_nil()
        {
            return Err("Invalid handoff identity or version");
        }
        if task.environment.is_some() && task.runtime.is_none() {
            return Err("An environment attempt requires its admitted runtime");
        }
        if self.result.task_id != task.task_id
            || self.result.attempt_id != task.attempt_id
            || self.reconnect.task != *task
            || self.reconnect.after.task_id != task.task_id
            || self.reconnect.after.attempt_id != task.attempt_id
            || self.cancellation.task_id != task.task_id
            || self.cancellation.attempt_id != task.attempt_id
            || self.retention.environment != task.environment
        {
            return Err("Handoff references must identify one authoritative attempt");
        }
        if self
            .result
            .artifacts
            .iter()
            .any(|a| a.task_id != task.task_id || a.attempt_id != task.attempt_id)
            || self
                .retention
                .required_artifact_ids
                .iter()
                .any(|id| !self.result.artifacts.iter().any(|a| a.id == *id))
        {
            return Err("Handoff artifacts must belong to the authoritative attempt");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn roles_cannot_exceed_current_caller_rights_or_implicit_agent_and_input_grants() {
        let actions = [
            SandboxCanvasAction::ViewTask,
            SandboxCanvasAction::WriteSharedChat,
            SandboxCanvasAction::AcquireTerminalControl,
            SandboxCanvasAction::InputTerminal,
            SandboxCanvasAction::RunAgent,
            SandboxCanvasAction::CancelAgent,
            SandboxCanvasAction::ApplyChanges,
            SandboxCanvasAction::ManageSharing,
        ];
        for role in [
            SandboxCanvasRole::Viewer,
            SandboxCanvasRole::Editor,
            SandboxCanvasRole::Owner,
        ] {
            for action in actions {
                assert!(!role.allows(action, false, true, true));
            }
        }
        for action in actions
            .iter()
            .filter(|a| **a != SandboxCanvasAction::ViewTask)
        {
            assert!(!SandboxCanvasRole::Viewer.allows(*action, true, true, true));
        }
        assert!(!SandboxCanvasRole::Editor.allows(
            SandboxCanvasAction::RunAgent,
            true,
            false,
            true
        ));
        assert!(!SandboxCanvasRole::Owner.allows(
            SandboxCanvasAction::CancelAgent,
            true,
            false,
            true
        ));
        assert!(!SandboxCanvasRole::Owner.allows(
            SandboxCanvasAction::InputTerminal,
            true,
            true,
            false
        ));
        assert!(!SandboxCanvasRole::Editor.allows(
            SandboxCanvasAction::ManageSharing,
            true,
            true,
            true
        ));
        assert!(SandboxCanvasRole::Owner.allows(SandboxCanvasAction::RunAgent, true, true, false));
        assert!(SandboxCanvasRole::Editor.allows(
            SandboxCanvasAction::InputTerminal,
            true,
            false,
            true
        ));
    }
    #[test]
    fn reconnect_and_cancel_references_cannot_rebind_an_attempt_or_delete_its_vm() {
        let task:SandboxTask=serde_json::from_value(json!({"id":Uuid::from_u128(1),"projectId":Uuid::from_u128(2),"attemptId":Uuid::from_u128(3),"runtime":{"vmId":Uuid::from_u128(4),"runtimeId":Uuid::from_u128(5)},"request":{"kind":"code","input":"Test"},"selection":{"connectionId":Uuid::from_u128(6),"model":"model"},"state":"result","createdAt":1,"updatedAt":2,"result":"Done","errorCode":null,"error":null,"events":[],"changes":[]})).unwrap();
        let handoff = SandboxCanvasHandoff::from_task(
            &task,
            Some(SandboxExecutionMode::PersistentWorkspace),
            vec![],
        );
        handoff.validate().unwrap();
        assert!(handoff.retention.borrowed_runtime);
        assert!(handoff.retention.workspace_deletion_requires_authorization);
        assert_eq!(
            handoff.retention.disconnect_policy,
            SandboxCanvasDisconnectPolicy::ObserveOnly
        );
        let mut stale = handoff.clone();
        stale.reconnect.task.runtime.as_mut().unwrap().runtime_id = Uuid::from_u128(7);
        assert!(stale.validate().is_err());
        let mut stale = handoff.clone();
        stale.cancellation.attempt_id = Uuid::from_u128(8);
        assert!(stale.validate().is_err());
        let mut forged = serde_json::to_value(handoff.cancellation).unwrap();
        forged["deleteVm"] = json!(true);
        assert!(serde_json::from_value::<SandboxCanvasCancelRef>(forged).is_err());
    }
}

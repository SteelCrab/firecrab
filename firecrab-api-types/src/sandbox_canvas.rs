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
    Viewer,
    Editor,
    Owner,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxCanvasAction {
    ViewTask,
    WriteSharedChat,
    AcquireTerminalControl,
    InputTerminal,
    RunAgent,
    CancelAgent,
    ApplyChanges,
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
    pub project_id: Uuid,
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub runtime: Option<SandboxRuntimeRef>,
    pub environment: Option<SandboxEnvironmentRef>,
}
/// Results remain owned by Sandbox; Canvas stores references and authorized projections.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxCanvasResultRef {
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub state: SandboxTaskState,
    pub updated_at: u64,
    pub artifacts: Vec<SandboxArtifact>,
}
/// A cursor identifies an attempt and ordered persisted Sandbox events, never another shell.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxCanvasEventCursor {
    pub task_id: Uuid,
    pub attempt_id: Uuid,
    pub sequence: Option<usize>,
    pub updated_at: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxCanvasReconnect {
    pub task: SandboxCanvasTaskRef,
    pub after: SandboxCanvasEventCursor,
    /// Refetch the authoritative task when a requested replay range is no longer retained.
    pub replay_reset_required: bool,
}
/// Cancellation only refers to the existing task/attempt. It never deletes a VM or Canvas.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxCanvasCancelRef {
    pub task_id: Uuid,
    pub attempt_id: Uuid,
}
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
pub struct SandboxCanvasRetention {
    pub environment: Option<SandboxEnvironmentRef>,
    pub execution_mode: Option<SandboxExecutionMode>,
    pub required_artifact_ids: Vec<Uuid>,
    pub export_required: bool,
    pub borrowed_runtime: bool,
    pub workspace_deletion_requires_authorization: bool,
    pub disconnect_policy: SandboxCanvasDisconnectPolicy,
}
/// Read-only handoff snapshot. There is no second admission or task state machine.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxCanvasHandoff {
    pub version: u16,
    pub task: SandboxCanvasTaskRef,
    pub result: SandboxCanvasResultRef,
    pub reconnect: SandboxCanvasReconnect,
    pub cancellation: SandboxCanvasCancelRef,
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

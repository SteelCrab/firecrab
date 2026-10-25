//! AI Sandbox projects and durable executions. Provider credentials are never part of these types.
use crate::AiModelSelection;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Transport-authenticated caller. Clients cannot supply this in project/task inputs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxPrincipal {
    /// Operator on the controller's loopback listener.
    #[default]
    Local,
    /// Host UUID from a verified mTLS certificate.
    Host(Uuid),
}

/// Immutable MicroVM generation selected when an attempt is admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxRuntimeRef {
    /// Borrowed MicroVM.
    #[serde(alias = "vm_id")]
    pub vm_id: Uuid,
    /// Captured runtime generation; never silently replaced for execution.
    #[serde(alias = "runtime_id")]
    pub runtime_id: Uuid,
}

/// Resource authorization is independent of model inference permission proofs.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SandboxResource {
    /// Saved project, or an explicitly reserved UUID for project creation.
    Project { id: Uuid },
    /// Saved provider connection.
    Connection { id: Uuid },
    /// Borrowed VM; execution still checks its current runtime identity.
    Vm { id: Uuid },
    /// Exact canonical API-host source directory, without recursive implicit grants.
    Source { path: String },
}

/// Explicit rights; one right does not imply another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxPermission {
    /// Inspect metadata, results, or source context.
    Read,
    /// Change project configuration or original source files.
    Write,
    /// Admit work, invoke a provider/VM, cancel, or reconcile.
    Execute,
    /// Apply reviewed changes from a project.
    Apply,
}

/// One exact resource and its allowed operations.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxResourceGrant {
    /// Authorized resource identity.
    pub resource: SandboxResource,
    /// Explicit allowed operations.
    pub permissions: Vec<SandboxPermission>,
}

/// Operator-managed policy for one authenticated host; missing policy denies access.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxAccessPolicy {
    /// Bounded resource grants, persisted in the private server database.
    pub grants: Vec<SandboxResourceGrant>,
}

/// User-controlled project settings. A missing source or VM permits conversation only.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SandboxProjectSpec {
    /// Display name.
    pub name: String,
    /// Explicitly authorized local project directory on the API host.
    pub source_path: Option<String>,
    /// Existing MicroVM borrowed exclusively for this project's command execution.
    pub vm_id: Option<Uuid>,
    /// Default verified model; each execution may override it.
    pub selection: Option<AiModelSelection>,
    /// Project instructions sent with each conversation or code task.
    #[serde(default)]
    pub instructions: String,
}

/// Saved project identity and configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxProject {
    /// Stable project identity.
    pub id: Uuid,
    /// Creator derived from the authenticated transport, never the request body.
    #[serde(default)]
    pub owner: SandboxPrincipal,
    /// Project settings without credentials.
    pub spec: SandboxProjectSpec,
    /// Creation time in Unix milliseconds.
    pub created_at: u64,
}

/// Execution mode. Only a code task receives file and guest-command tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxTaskKind {
    /// Multi-turn text conversation without tools.
    Chat,
    /// Isolated project edits and bounded MicroVM commands.
    Code,
}

/// Durable execution states. Interrupted work is never automatically repeated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxTaskState {
    /// Accepted durably, waiting for a worker.
    Waiting,
    /// Preparing model and project context.
    Accepted,
    /// Provider or guest execution is in progress.
    InProgress,
    /// Final response and reviewable changes are available.
    Result,
    /// All selected changes were applied to the source project.
    Merged,
    /// The attempt failed; its logs and changes remain available.
    Failed,
    /// Cancellation was confirmed.
    Cancelled,
    /// Restart or uncertain guest termination requires reconciliation.
    Interrupted,
}

impl SandboxTaskState {
    /// Whether the attempt may still be running on the controller.
    pub fn active(self) -> bool {
        matches!(self, Self::Waiting | Self::Accepted | Self::InProgress)
    }
}

/// Client-generated task identities make admission retries idempotent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SandboxTaskInput {
    /// Conversation or code execution.
    pub kind: SandboxTaskKind,
    /// User instruction.
    pub input: String,
    /// Explicit override; omission uses the project default at admission.
    pub selection: Option<AiModelSelection>,
    /// Optional failed/cancelled attempt this task explicitly retries.
    pub retry_of: Option<Uuid>,
}

/// One redacted tool invocation with its actual execution result.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxEvent {
    /// Stable event sequence in the attempt.
    pub sequence: usize,
    /// Event timestamp in Unix milliseconds.
    pub at: u64,
    /// Tool or lifecycle event name.
    pub name: String,
    /// Sanitized arguments or progress description.
    pub input: String,
    /// Bounded stdout or tool result.
    pub output: String,
    /// Bounded stderr, separate from stdout.
    pub stderr: String,
    /// Guest exit code when a command ran.
    pub exit_code: Option<i32>,
}

/// Text file changes stay reviewable even after failure or cancellation.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxFileChange {
    /// Relative, confined project path.
    pub path: String,
    /// Initial file contents; absent for a new file.
    pub before: Option<String>,
    /// Result contents; absent for a deletion.
    pub after: Option<String>,
    /// Original portable permission bits, checked during apply.
    pub before_mode: Option<u32>,
    /// Result permissions, retaining source read/write bits and reviewed executable bits.
    pub after_mode: Option<u32>,
    /// Whether this exact change has been applied to the original project.
    pub applied: bool,
}

/// A persisted task and its single immutable attempt identity.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxTask {
    /// Client-generated task identity.
    pub id: Uuid,
    /// Owning project.
    pub project_id: Uuid,
    /// Unique execution identity used for guest ownership and reconciliation.
    pub attempt_id: Uuid,
    /// Original runtime generation captured before dispatch; absent for conversation.
    #[serde(default)]
    pub runtime: Option<SandboxRuntimeRef>,
    /// Original task request, with credentials redacted.
    pub request: SandboxTaskInput,
    /// Effective connection and model fixed at admission.
    pub selection: AiModelSelection,
    /// Current durable state.
    pub state: SandboxTaskState,
    /// Creation timestamp in Unix milliseconds.
    pub created_at: u64,
    /// Last persisted update timestamp.
    pub updated_at: u64,
    /// Complete assistant response, never synthesized from a partial stream.
    pub result: String,
    /// Stable sanitized failure code.
    pub error_code: Option<String>,
    /// Sanitized failure description.
    pub error: Option<String>,
    /// Ordered tool results shared by conversation and task board.
    pub events: Vec<SandboxEvent>,
    /// Reviewable modifications to the isolated project copy.
    pub changes: Vec<SandboxFileChange>,
}

/// Provider catalogue entries are distinct from verified inference permissions.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxModel {
    /// Provider model identity.
    pub id: String,
    /// Human-readable model label.
    pub name: String,
    /// A complete text request succeeded with the current connection.
    pub verified: bool,
    /// A native tool-call round trip succeeded with the current connection.
    pub tools_verified: bool,
}

/// Model discovery can fall back to configured-model verification without inventing a catalogue.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxModelCatalog {
    /// Actual provider entries plus previously verified models.
    pub models: Vec<SandboxModel>,
    /// Whether the provider's discovery endpoint was available to this credential.
    pub discovery_available: bool,
}

/// Explicit, potentially billed model permission check.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SandboxVerifyModel {
    /// Model identity supplied by the user or provider catalogue.
    pub model: String,
    /// Also verify native function calls with a harmless local probe.
    pub tools: bool,
}

/// Apply only the user-selected files after verifying the source has not changed.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SandboxApply {
    /// Relative paths selected in review.
    pub paths: Vec<String>,
}

/// Lightweight conversation and board record. File contents and tool logs require task inspection.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxTaskSummary {
    /// Client-generated task identity.
    pub id: Uuid,
    /// Owning project.
    pub project_id: Uuid,
    /// Unique execution identity used for guest ownership and reconciliation.
    pub attempt_id: Uuid,
    /// Original runtime generation for this attempt.
    #[serde(default)]
    pub runtime: Option<SandboxRuntimeRef>,
    /// Original task request, with credentials redacted.
    pub request: SandboxTaskInput,
    /// Effective connection and model fixed at admission.
    pub selection: AiModelSelection,
    /// Current durable state.
    pub state: SandboxTaskState,
    /// Creation timestamp in Unix milliseconds.
    pub created_at: u64,
    /// Last persisted update timestamp.
    pub updated_at: u64,
    /// Complete assistant response, never synthesized from a partial stream.
    pub result: String,
    /// Stable sanitized failure code.
    pub error_code: Option<String>,
    /// Sanitized failure description.
    pub error: Option<String>,
    /// Number of tool events; full output is available from the task detail endpoint.
    pub event_count: usize,
    /// Number of changed files; contents are loaded only for explicit review.
    pub change_count: usize,
    /// Most recent tool name for execution feedback.
    pub last_tool: Option<String>,
}

impl From<&SandboxTask> for SandboxTaskSummary {
    fn from(task: &SandboxTask) -> Self {
        Self {
            id: task.id,
            project_id: task.project_id,
            attempt_id: task.attempt_id,
            runtime: task.runtime,
            request: task.request.clone(),
            selection: task.selection.clone(),
            state: task.state,
            created_at: task.created_at,
            updated_at: task.updated_at,
            result: task.result.clone(),
            error_code: task.error_code.clone(),
            error: task.error.clone(),
            event_count: task.events.len(),
            change_count: task.changes.len(),
            last_tool: task.events.last().map(|e| e.name.clone()),
        }
    }
}

//! Durable project PTYs have one explicit input controller and immutable runtime identities.
use crate::SandboxPrincipal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

fn default_rows() -> u16 {
    24
}
fn default_columns() -> u16 {
    80
}

/// Idempotent session creation uses the UUID in the request URL.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateProjectTerminal {
    /// Human-readable name, limited to 64 characters.
    pub label: String,
    /// Workspace-relative existing directory; empty means the project root.
    #[serde(default)]
    pub working_directory: String,
    /// Initial character rows. Later resizing does not change the creation request.
    #[serde(default = "default_rows")]
    pub rows: u16,
    /// Initial character columns.
    #[serde(default = "default_columns")]
    pub columns: u16,
}

/// A reboot ends process continuity; reconnect alone never dispatches a new shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectTerminalStatus {
    Pending,
    Running,
    Exited,
    Interrupted,
}

/// Public lease metadata deliberately excludes its input token.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTerminalController {
    /// Verified transport principal holding control.
    pub principal: SandboxPrincipal,
    /// Browser client identity supplied when claiming control.
    pub client_id: Uuid,
    /// Monotonic fence; old connections cannot release or use a newer lease.
    pub generation: u64,
    /// Unix expiration time. Heartbeats renew the lease for 30 seconds.
    pub expires_at: u64,
}

/// Session metadata and bounded durable output remain queryable with a stopped VM.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTerminal {
    pub id: Uuid,
    pub project_id: Uuid,
    pub vm_id: Uuid,
    pub runtime_id: Uuid,
    pub label: String,
    pub working_directory: String,
    pub creator: SandboxPrincipal,
    pub status: ProjectTerminalStatus,
    /// Kernel boot identity confirmed by the guest supervisor.
    pub boot_id: Option<Uuid>,
    pub exit_code: Option<u32>,
    /// Earliest replay byte retained by the controller.
    pub output_start: u64,
    /// Exclusive byte cursor already persisted in project SQLite.
    pub output_end: u64,
    /// Exclusive committed cursor reported by the guest.
    pub guest_output_end: u64,
    pub truncated: bool,
    /// The supervisor could not confirm that all owned processes ended.
    pub cleanup_required: bool,
    pub control_generation: u64,
    pub controller: Option<ProjectTerminalController>,
}

/// Taking control is explicit and optimistic; simultaneous claims cannot both succeed.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClaimProjectTerminal {
    pub client_id: Uuid,
    pub expected_generation: u64,
    /// Explicitly replace another active controller when current permissions allow it.
    #[serde(default)]
    pub take_over: bool,
}

/// Sent only to the successful controller, never in session listings or replay messages.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTerminalLease {
    pub client_id: Uuid,
    pub generation: u64,
    pub token: Uuid,
    pub expires_at: u64,
}

/// Input, resizing, termination and lease renewal require the same current proof.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectTerminalControl {
    pub client_id: Uuid,
    pub generation: u64,
    pub token: Uuid,
}

/// Each output page starts at an explicit absolute cursor.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTerminalOutput {
    pub start: u64,
    pub next: u64,
    pub end: u64,
    pub truncated: bool,
    pub content_base64: String,
}

/// JSON WebSocket messages keep input tokens out of URLs, cookies and logs.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ProjectTerminalMessage {
    Input {
        control: ProjectTerminalControl,
        content_base64: String,
    },
    Resize {
        control: ProjectTerminalControl,
        rows: u16,
        columns: u16,
    },
    Renew {
        control: ProjectTerminalControl,
    },
}

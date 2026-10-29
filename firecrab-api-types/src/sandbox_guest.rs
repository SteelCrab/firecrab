//! Versioned guest operations over the authenticated, runtime-pinned channel.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Hard execution bounds; the guest must reject unsupported limits.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SandboxGuestLimits {
    /// Wall clock deadline, 1–120 seconds.
    pub wall_seconds: u64,
    /// CPU time ceiling, 1–120 seconds.
    pub cpu_seconds: u64,
    /// Address-space ceiling in MiB, 16–65536.
    pub memory_mib: u32,
    /// Per-stream retained output ceiling, 1–65536 bytes.
    pub output_bytes: usize,
}

/// One argv invocation; no shell interpolation is performed on these fields.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SandboxGuestCommand {
    /// Contract protocol version, currently 1.
    pub version: u16,
    /// Program followed by its exact arguments.
    pub argv: Vec<String>,
    /// Relative workspace directory, checked after resolving symlinks.
    pub cwd: String,
    /// Explicit guest environment, never inherited from controller/provider state.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// UTF-8 stdin bytes; empty closes stdin.
    #[serde(default)]
    pub stdin: String,
    /// Requested, enforced bounds.
    pub limits: SandboxGuestLimits,
}

/// Capabilities are probed; an OS name alone never implies support.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxGuestCapabilities {
    /// Negotiated protocol version.
    pub version: u16,
    /// Detected init system: systemd, openrc, or distroless.
    pub init: String,
    /// Authenticated channel can execute bounded argv commands.
    pub execution: bool,
    /// Docker daemon and digest-pinned images can be used.
    pub docker: bool,
    /// nspawn and content-addressed roots can be used.
    pub nspawn: bool,
    /// Explicit unsupported reason, if execution is unavailable.
    pub unsupported: Option<String>,
}

/// Boot/PID/start-time identity prevents recycled-PID signals.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SandboxGuestProcess {
    /// Guest PID.
    pub pid: u32,
    /// Linux process start time in clock ticks.
    pub start_ticks: u64,
    /// Guest boot UUID.
    pub boot_id: String,
    /// Linux process state.
    pub state: String,
}

/// Only bounded termination signals are supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxGuestSignal {
    /// SIGTERM.
    Terminate,
    /// SIGKILL.
    Kill,
}

/// Signal one exact process owned by the admitted attempt.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SandboxGuestSignalRequest {
    /// Exact process identity returned by inspection.
    pub process: SandboxGuestProcess,
    /// Requested termination signal.
    pub signal: SandboxGuestSignal,
}

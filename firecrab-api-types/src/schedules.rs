//! Host-local time-based VM operations shared by the API and CLI.
//!
//! JSON uses camelCase field names. Timestamps ending in `Ms` are Unix
//! milliseconds. Configuration and execution history are separate resources.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Existing VM lifecycle operation invoked for each scheduled occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleAction {
    /// Start the VM and wait for its startup pipeline to finish.
    Start,
    /// Force-stop the VM through the existing stop handler.
    Stop,
}

/// One absolute RFC 3339 instant, or five cron fields in an IANA time zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScheduleTiming {
    /// Execute once at an absolute instant, independent of the host's time zone.
    Once {
        /// RFC 3339 timestamp containing `Z` or an explicit numeric UTC offset.
        at: String,
    },
    /// Repeat at matching local minutes in the specified time zone.
    Cron {
        /// Minute, hour, day of month, month and weekday; use named weekdays.
        expression: String,
        /// IANA zone such as `Asia/Seoul`; daylight-saving gaps are skipped.
        /// Repeated local minutes use only the earlier UTC instant.
        #[serde(rename = "timeZone")]
        time_zone: String,
    },
}

/// Full replacement body for `POST /api/schedules` and `PUT /api/schedules/{id}`.
///
/// Enabling requires a future occurrence. Saving a disabled schedule still
/// validates the timing syntax and requires an existing target VM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScheduleSpec {
    /// Display name containing 1–64 UTF-8 bytes and no control characters.
    /// Whitespace-only names are rejected.
    pub name: String,
    /// Existing MicroVM to operate on; serialized as `vmId`.
    pub vm_id: Uuid,
    /// Lifecycle operation to perform when an occurrence is claimed.
    pub action: ScheduleAction,
    /// One-time instant or recurring local calendar rule.
    pub timing: ScheduleTiming,
    /// Whether future occurrences may be claimed; defaults to `true` in JSON.
    /// Disabling does not cancel an already claimed operation.
    #[serde(default = "enabled")]
    pub enabled: bool,
}

/// Serde default for requests that omit the `enabled` field.
fn enabled() -> bool {
    true
}

/// Saved configuration, scheduling state, and the latest execution result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleResponse {
    /// Stable identifier used by schedule configuration and history routes.
    pub id: Uuid,
    /// Saved configuration, flattened into the response JSON object.
    #[serde(flatten)]
    pub spec: ScheduleSpec,
    /// Next due Unix millisecond; absent while running, disabled, or consumed.
    pub next_run_at_ms: Option<i64>,
    /// Unix milliseconds when the schedule was created.
    pub created_at_ms: i64,
    /// Unix milliseconds of creation or the last user configuration change.
    pub updated_at_ms: i64,
    /// Most recently claimed execution, or `None` before the first claim.
    pub last_run: Option<ScheduleRun>,
}

/// Durable execution outcome; interrupted side effects are never replayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleRunStatus {
    /// The occurrence is claimed and its completion has not been recorded.
    Running,
    /// The VM lifecycle handler completed successfully.
    Succeeded,
    /// The target was missing or its lifecycle handler returned an error.
    Failed,
    /// The API restarted before recording the outcome; side effects are unknown.
    Interrupted,
}

/// One claimed occurrence retained in a schedule's bounded execution history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRun {
    /// Execution identifier, also used to correlate the VM handler's request.
    pub id: Uuid,
    /// Schedule that claimed this occurrence.
    pub schedule_id: Uuid,
    /// Original due instant in Unix milliseconds; may precede the actual claim.
    pub scheduled_at_ms: i64,
    /// Unix milliseconds when the occurrence was claimed before side effects.
    pub started_at_ms: i64,
    /// Completion or restart-recovery time in Unix milliseconds; absent while running.
    pub finished_at_ms: Option<i64>,
    /// Current durable state of this execution.
    pub status: ScheduleRunStatus,
    /// Lifecycle failure or interruption reason; absent for a successful run.
    pub error: Option<String>,
}

/// Separate enable/disable control preserves all other schedule fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetScheduleEnabledRequest {
    /// Resume at a future occurrence, or pause future claims without cancellation.
    pub enabled: bool,
}

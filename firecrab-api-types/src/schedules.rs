//! Host-local time-based VM operations.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleAction {
    Start,
    Stop,
}

/// One absolute RFC 3339 instant, or five cron fields in an IANA time zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScheduleTiming {
    Once {
        at: String,
    },
    Cron {
        expression: String,
        #[serde(rename = "timeZone")]
        time_zone: String,
    },
}

/// Full replacement body for POST /api/schedules and PUT /api/schedules/{id}.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScheduleSpec {
    pub name: String,
    pub vm_id: Uuid,
    pub action: ScheduleAction,
    pub timing: ScheduleTiming,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleResponse {
    pub id: Uuid,
    #[serde(flatten)]
    pub spec: ScheduleSpec,
    pub next_run_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub last_run: Option<ScheduleRun>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleRunStatus {
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRun {
    pub id: Uuid,
    pub schedule_id: Uuid,
    pub scheduled_at_ms: i64,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub status: ScheduleRunStatus,
    pub error: Option<String>,
}

/// Separate enable/disable control preserves all other schedule fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetScheduleEnabledRequest {
    pub enabled: bool,
}

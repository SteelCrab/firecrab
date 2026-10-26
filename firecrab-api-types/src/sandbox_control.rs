//! Shared bounded client wait result. Inspection never repeats task effects.
use crate::SandboxTask;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxTaskWaitResult {
    /// A final result/failure/confirmed cancellation was observed before the deadline.
    pub reached: bool,
    /// Last authoritative REST record, including interrupted or timed-out states.
    pub task: SandboxTask,
}

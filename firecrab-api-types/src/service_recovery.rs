//! Durable single-host replica observations and repair intent. No credential values.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ServiceRouteTransition;

/// Health history belongs to the active deployment, not to its immutable release.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceRecovery {
    /// Consecutive observations and retry budget for each logical replica.
    pub replicas: BTreeMap<Uuid, ReplicaHealth>,
    /// One persisted repair at a time; all its VM IDs remain deployment-owned.
    pub repair: Option<ReplicaRepair>,
}

/// Retry history follows a replica through replacement until it stays healthy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaHealth {
    /// Consecutive failed probes, capped at the failure threshold.
    pub failures: u8,
    /// Consecutive successful probes, capped at the recovery threshold.
    pub successes: u8,
    /// Repair attempts since the replica last stayed healthy for one minute.
    pub attempts: u8,
    /// Earliest automatic retry, as a Unix timestamp in seconds.
    pub retry_at: i64,
    /// Beginning of uninterrupted healthy observations.
    pub healthy_since: Option<i64>,
}

/// Intent is saved before stopping, starting, creating, or deleting a VM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaRepair {
    /// Exact worker identity used to reject stale queued work.
    pub id: Uuid,
    /// Failed replica; retained until replacement routing and drain succeed.
    pub vm_id: Uuid,
    /// Reserved replacement VM, also present in the deployment's instances.
    pub replacement_id: Option<Uuid>,
    /// Next idempotent step to execute after queue admission or API recovery.
    pub stage: ReplicaRepairStage,
    /// Saved route transition, including old Nginx worker identities.
    pub transition: Option<ServiceRouteTransition>,
}

/// A failed step retains its intent and ownership for bounded retry or Stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicaRepairStage {
    /// Withdraw a newly unhealthy endpoint after its repair budget is exhausted.
    Paused,
    /// Remove the failed endpoint and verify existing connections drained.
    Withdraw,
    /// Stop the original VM before attempting one restart.
    Stop,
    /// Boot the original VM and check readiness.
    Start,
    /// Reserve and prepare a replacement from the pinned deployment.
    Replace,
    /// Publish the ready endpoint and verify the route generation.
    Publish,
    /// Delete the retired VM only after verified drain.
    Cleanup,
}

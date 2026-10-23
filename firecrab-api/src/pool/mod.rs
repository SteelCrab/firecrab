//! Host-local warm MicroVM pools (issue #291).
//!
//! A pool keeps `minReady` booted, never-used members for callers to lease.
//! A member is used once: when its lease is released or expires, the VM is
//! stopped, deleted, and replaced by a fresh one, so no caller ever gets a
//! disk another caller modified.

pub(crate) mod guard;
mod reconcile;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use firecrab_api_types::{PoolLeaseResponse, PoolMemberResponse, PoolResponse};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::persistence::{LeaseRow, MemberRow, PoolRow};
use crate::state::AppState;

#[cfg(test)]
pub(crate) use reconcile::reconcile;
pub(crate) use reconcile::spawn_reconciler;

/// How long a pool whose member failed to create or boot waits before the
/// reconciler tries again, so a broken image does not churn VMs every tick.
const FAILURE_BACKOFF: Duration = Duration::from_secs(30);

/// Pool state that lives only in memory: the reconciler's wake-up signal and
/// each pool's most recent member failure.
#[derive(Debug, Clone, Default)]
pub(crate) struct PoolRuntime {
    wake: Arc<Notify>,
    failures: Arc<Mutex<HashMap<Uuid, Failure>>>,
}

#[derive(Debug, Clone)]
struct Failure {
    message: String,
    retry_at: Instant,
}

impl PoolRuntime {
    /// Runs the reconciler now instead of at its next tick, e.g. after a
    /// release so the replacement starts booting right away.
    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    /// The pool's most recent member failure, if it has not recovered.
    pub(crate) fn last_error(&self, pool_id: Uuid) -> Option<String> {
        self.lock()
            .get(&pool_id)
            .map(|failure| failure.message.clone())
    }

    fn record_failure(&self, pool_id: Uuid, message: String) {
        tracing::warn!(%pool_id, message, "pool member failed");
        self.lock().insert(
            pool_id,
            Failure {
                message,
                retry_at: Instant::now() + FAILURE_BACKOFF,
            },
        );
    }

    fn clear_failure(&self, pool_id: Uuid) {
        self.lock().remove(&pool_id);
    }

    fn backing_off(&self, pool_id: Uuid) -> bool {
        self.lock()
            .get(&pool_id)
            .is_some_and(|failure| Instant::now() < failure.retry_at)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Failure>> {
        self.failures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Pool names become part of member VM names (`pool-{name}-{suffix}`), so
/// they follow VM naming with room left for the prefix and suffix.
pub(crate) fn valid_pool_name(name: &str) -> bool {
    (1..=40).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn member_vm_name(pool_name: &str) -> String {
    format!(
        "pool-{pool_name}-{}",
        &Uuid::new_v4().simple().to_string()[..8]
    )
}

pub(crate) fn pool_response(
    state: &AppState,
    row: &PoolRow,
    members: &[MemberRow],
) -> PoolResponse {
    PoolResponse {
        id: row.id,
        name: row.name.clone(),
        template: row.template.clone(),
        template_version: row.template_version.clone(),
        cpu: row.cpu,
        ram: row.ram,
        disk_gb: row.disk_gb,
        egress_policy: row.egress_policy,
        micro_network_id: row.micro_network_id,
        storage_root: row.storage_root.clone(),
        min_ready: row.min_ready,
        max_size: row.max_size,
        lease_ttl_seconds: row.lease_ttl_seconds,
        deleting: row.deleting,
        members: members
            .iter()
            .map(|member| PoolMemberResponse {
                vm_id: member.vm_id,
                state: member.state,
                created_at_ms: member.created_at_ms,
            })
            .collect(),
        last_error: state.pools.last_error(row.id),
        created_at_ms: row.created_at_ms,
    }
}

/// A lease with its VM's current address and state while the VM exists.
pub(crate) async fn lease_response(state: &AppState, lease: LeaseRow) -> PoolLeaseResponse {
    let record = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&lease.vm_id)
        .cloned();
    let vm = match record {
        Some(record) => {
            let address = crate::handlers::vms::lease_for(state, record.id).await;
            Some(crate::handlers::vms::vm_response(
                state,
                &record,
                address.as_ref(),
            ))
        }
        None => None,
    };
    PoolLeaseResponse {
        id: lease.id,
        pool_id: lease.pool_id,
        vm_id: lease.vm_id,
        state: lease.state,
        acquired_at_ms: lease.acquired_at_ms,
        expires_at_ms: lease.expires_at_ms,
        ended_at_ms: lease.ended_at_ms,
        vm,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_names_leave_room_for_the_member_prefix_and_suffix() {
        assert!(valid_pool_name("ci-runners.v2"));
        assert!(!valid_pool_name(""));
        assert!(!valid_pool_name("has space"));
        assert!(!valid_pool_name(&"a".repeat(41)));

        let member = member_vm_name(&"a".repeat(40));
        assert!(member.len() <= 64, "{member}");
        assert!(member.starts_with(&format!("pool-{}-", "a".repeat(40))));
    }
}

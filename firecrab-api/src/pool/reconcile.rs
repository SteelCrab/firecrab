//! Converges every pool on its limits: end expired leases, stop and delete
//! used or dead members, then boot fresh ones up to `minReady` without ever
//! holding more than `maxSize` VMs, draining ones included.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use axum::Extension;
use axum::extract::{Path, State};
use firecrab_api_types::{CreateVmRequest, PoolLeaseState, PoolMemberState};
use uuid::Uuid;

use super::{member_vm_name, now_ms};
use crate::model::{VmPurpose, VmState};
use crate::persistence::{MemberRow, PoolRow, PoolStoreError};
use crate::server::RequestId;
use crate::state::AppState;

/// Upper bound between passes; releases and pool changes wake it sooner.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(2);

/// How long ended leases, and so their idempotency keys, are remembered.
const LEASE_RETENTION_MS: u64 = 24 * 60 * 60 * 1000;

/// Runs [`reconcile`] for the life of the process.
pub(crate) fn spawn_reconciler(state: AppState) {
    tokio::spawn(async move {
        loop {
            reconcile(&state, now_ms()).await;
            tokio::select! {
                () = state.pools.wake.notified() => {}
                () = tokio::time::sleep(RECONCILE_INTERVAL) => {}
            }
        }
    });
}

/// One pass over every pool at `now_ms`.
pub(crate) async fn reconcile(state: &AppState, now_ms: u64) {
    let store = state.store.clone();
    let pools = blocking(move || {
        for lease in store.expired_pool_leases(now_ms)? {
            store.end_pool_lease(lease.id, PoolLeaseState::Expired, now_ms)?;
        }
        store.prune_ended_pool_leases(now_ms.saturating_sub(LEASE_RETENTION_MS))?;
        store.list_pools()
    })
    .await;
    let pools = match pools {
        Ok(pools) => pools,
        Err(error) => {
            tracing::warn!(error, "pool reconcile could not load pools");
            return;
        }
    };

    let mut owned = HashSet::new();
    let mut complete = true;
    for pool in &pools {
        match reconcile_pool(state, pool, now_ms).await {
            Ok(members) => owned.extend(members),
            Err(error) => {
                complete = false;
                tracing::warn!(pool_id = %pool.id, error, "pool reconcile failed");
            }
        }
    }
    // Only with every pool's members known is a pool VM with no member row
    // really an orphan.
    if complete {
        sweep_orphans(state, &owned).await;
    }
}

/// Settles, trims, and refills one pool. Returns the VMs it still owns.
async fn reconcile_pool(
    state: &AppState,
    pool: &PoolRow,
    now_ms: u64,
) -> Result<Vec<Uuid>, String> {
    for member in load_members(state, pool.id).await? {
        // One stuck member must not hold up the rest of the pool; it is
        // retried on the next pass.
        if let Err(error) = settle(state, pool, member).await {
            tracing::warn!(pool_id = %pool.id, vm_id = %member.vm_id, error, "pool member not settled");
        }
    }
    let mut members = load_members(state, pool.id).await?;

    if pool.deleting {
        // Deletion is refused while a lease is active, so everything left is
        // warm or already draining.
        for member in &members {
            match member.state {
                PoolMemberState::Draining => drain(state, member.vm_id).await?,
                _ => retire(state, member.vm_id, WARM).await?,
            }
        }
        if load_members(state, pool.id).await?.is_empty() {
            let store = state.store.clone();
            let id = pool.id;
            blocking(move || store.delete_pool(id)).await?;
            state.pools.clear_failure(pool.id);
            tracing::info!(pool_id = %pool.id, "pool deleted");
        }
        return Ok(Vec::new());
    }

    let count = |state: PoolMemberState| members.iter().filter(|m| m.state == state).count();
    let warm = count(PoolMemberState::Ready) + count(PoolMemberState::Provisioning);
    let live = members.len() - count(PoolMemberState::Draining);
    let excess = warm
        .saturating_sub(usize::from(pool.min_ready))
        .max(live.saturating_sub(usize::from(pool.max_size)));
    // After a limit was lowered: give up the newest warm members first.
    let surplus: Vec<Uuid> = members
        .iter()
        .rev()
        .filter(|m| m.state == PoolMemberState::Ready)
        .chain(
            members
                .iter()
                .rev()
                .filter(|m| m.state == PoolMemberState::Provisioning),
        )
        .take(excess)
        .map(|m| m.vm_id)
        .collect();
    for vm_id in surplus {
        retire(state, vm_id, WARM).await?;
    }
    members = load_members(state, pool.id).await?;

    let warm = members
        .iter()
        .filter(|m| {
            matches!(
                m.state,
                PoolMemberState::Ready | PoolMemberState::Provisioning
            )
        })
        .count();
    // Draining members still hold their disk and address, so they count
    // against maxSize until they are gone.
    let room = usize::from(pool.max_size).saturating_sub(members.len());
    let wanted = usize::from(pool.min_ready).saturating_sub(warm).min(room);
    if wanted > 0 && !state.pools.backing_off(pool.id) {
        for _ in 0..wanted {
            if let Err(message) = create_member(state, pool, now_ms).await {
                state.pools.record_failure(pool.id, message);
                break;
            }
        }
        members = load_members(state, pool.id).await?;
    }
    Ok(members.into_iter().map(|m| m.vm_id).collect())
}

/// Moves one member along according to its VM's actual state.
async fn settle(state: &AppState, pool: &PoolRow, member: MemberRow) -> Result<(), String> {
    use PoolMemberState::{Draining, Leased, Provisioning, Ready};

    let Some(vm_state) = vm_state(state, member.vm_id) else {
        // The VM record is gone (or was never written): end any lease on
        // it and forget the member.
        end_active_lease(state, member.vm_id).await?;
        let store = state.store.clone();
        return blocking(move || store.delete_pool_member(member.vm_id)).await;
    };
    match (member.state, vm_state) {
        (Draining, _) => drain(state, member.vm_id).await,
        (Provisioning, VmState::Running) => {
            if set_state(state, member.vm_id, &[Provisioning], Ready).await? {
                state.pools.clear_failure(pool.id);
            }
            Ok(())
        }
        (Provisioning, VmState::Starting) | (Ready | Leased, VmState::Running) => Ok(()),
        (Provisioning, other) => {
            state.pools.record_failure(
                pool.id,
                format!(
                    "member {} did not boot (it is {})",
                    member.vm_id,
                    crate::persistence::encode_state(other)
                ),
            );
            retire(state, member.vm_id, &[Provisioning]).await
        }
        (Ready, _) => retire(state, member.vm_id, &[Ready]).await,
        // The VM stopped under its lease (it crashed, or the API restarted
        // and every guest went with it): the lease is over.
        (Leased, _) => {
            end_active_lease(state, member.vm_id).await?;
            drain(state, member.vm_id).await
        }
    }
}

/// Stops (if running) and deletes a draining member's VM, then forgets the
/// member. A VM mid-start or mid-stop is left for the next pass.
async fn drain(state: &AppState, vm_id: Uuid) -> Result<(), String> {
    let request_id = RequestId(Uuid::new_v4());
    match vm_state(state, vm_id) {
        Some(VmState::Starting | VmState::Stopping) => return Ok(()),
        Some(VmState::Running) => {
            let _stopped = crate::handlers::vms::stop_vm(
                State(state.clone()),
                Extension(request_id),
                Path(vm_id.to_string()),
            )
            .await
            .map_err(|_| format!("could not stop pool VM {vm_id}"))?;
        }
        _ => {}
    }
    if vm_state(state, vm_id).is_some() {
        crate::handlers::vms::delete_vm(
            State(state.clone()),
            Extension(request_id),
            Path(vm_id.to_string()),
        )
        .await
        .map_err(|_| format!("could not delete pool VM {vm_id}"))?;
        tracing::info!(%vm_id, "pool member deleted");
    }
    let store = state.store.clone();
    blocking(move || store.delete_pool_member(vm_id)).await
}

/// Creates and starts one member from the pool's pinned image. The member
/// row is written before the VM, so a crash in between leaves a row the
/// next pass cleans up rather than an untracked VM.
async fn create_member(state: &AppState, pool: &PoolRow, now_ms: u64) -> Result<(), String> {
    let template = state
        .templates
        .resolve_version(&pool.template, &pool.template_version)
        .ok_or_else(|| {
            format!(
                "image {} ({}) is no longer installed",
                pool.template, pool.template_version
            )
        })?;
    let request = member_request(pool);
    let fields = crate::handlers::vms::validate_create(&request, state);
    if !fields.is_empty() {
        let detail: Vec<String> = fields
            .iter()
            .map(|(field, message)| format!("{field} {message}"))
            .collect();
        return Err(format!("member spec rejected: {}", detail.join("; ")));
    }

    let vm_id = Uuid::new_v4();
    let store = state.store.clone();
    let member = MemberRow {
        vm_id,
        pool_id: pool.id,
        state: PoolMemberState::Provisioning,
        created_at_ms: now_ms,
    };
    blocking(move || store.insert_pool_member(&member)).await?;

    let request_id = RequestId(Uuid::new_v4());
    if let Err(error) = crate::handlers::vms::insert_vm(
        state,
        request_id,
        vm_id,
        request,
        &template,
        VmPurpose::Pool,
    )
    .await
    {
        tracing::warn!(pool_id = %pool.id, %vm_id, ?error, "pool member VM could not be created");
        let store = state.store.clone();
        blocking(move || store.delete_pool_member(vm_id)).await?;
        return Err("could not create a member VM".to_owned());
    }
    if crate::handlers::vms::start_vm_request(
        State(state.clone()),
        Extension(request_id),
        Path(vm_id.to_string()),
    )
    .await
    .is_err()
    {
        set_state(
            state,
            vm_id,
            &[PoolMemberState::Provisioning],
            PoolMemberState::Draining,
        )
        .await?;
        return Err(format!("could not start member {vm_id}"));
    }
    tracing::info!(pool_id = %pool.id, %vm_id, "pool member booting");
    Ok(())
}

/// The VM a member is created as: the pool's spec, nothing per-caller.
pub(crate) fn member_request(pool: &PoolRow) -> CreateVmRequest {
    CreateVmRequest {
        name: member_vm_name(&pool.name),
        template: pool.template.clone(),
        ram: pool.ram,
        cpu: pool.cpu,
        disk_gb: pool.disk_gb,
        egress_policy: pool.egress_policy,
        micro_network_id: pool.micro_network_id,
        storage_root: Some(pool.storage_root.clone()),
        shell_ids: Vec::new(),
        port_forwards: Vec::new(),
        env: BTreeMap::new(),
    }
}

/// Deletes pool VMs no member row accounts for, so no failure path can leak
/// a hidden VM, its disk, or its address.
async fn sweep_orphans(state: &AppState, owned: &HashSet<Uuid>) {
    let orphans: Vec<Uuid> = state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .values()
        .filter(|vm| vm.purpose == VmPurpose::Pool && !owned.contains(&vm.id))
        .map(|vm| vm.id)
        .collect();
    for vm_id in orphans {
        tracing::warn!(%vm_id, "deleting a pool VM no pool owns");
        if let Err(error) = drain(state, vm_id).await {
            tracing::warn!(%vm_id, error, "orphaned pool VM could not be deleted");
        }
    }
}

async fn load_members(state: &AppState, pool_id: Uuid) -> Result<Vec<MemberRow>, String> {
    let store = state.store.clone();
    blocking(move || store.pool_members(pool_id)).await
}

/// Moves the member only if it is still in one of `from`; `false` means a
/// concurrent change (such as an acquire) got there first.
async fn set_state(
    state: &AppState,
    vm_id: Uuid,
    from: &'static [PoolMemberState],
    to: PoolMemberState,
) -> Result<bool, String> {
    let store = state.store.clone();
    blocking(move || store.set_pool_member_state(vm_id, from, to)).await
}

/// Members that are booted or booting and not handed to anyone.
const WARM: &[PoolMemberState] = &[PoolMemberState::Provisioning, PoolMemberState::Ready];

/// Sends a member still in one of `from` to draining and drains it. A member
/// a concurrent acquire leased since it was read is left alone.
async fn retire(
    state: &AppState,
    vm_id: Uuid,
    from: &'static [PoolMemberState],
) -> Result<(), String> {
    if set_state(state, vm_id, from, PoolMemberState::Draining).await? {
        drain(state, vm_id).await?;
    }
    Ok(())
}

async fn end_active_lease(state: &AppState, vm_id: Uuid) -> Result<(), String> {
    let store = state.store.clone();
    blocking(move || {
        if let Some(lease) = store.active_pool_lease_for_vm(vm_id)? {
            store.end_pool_lease(lease.id, PoolLeaseState::Expired, now_ms())?;
        }
        Ok(())
    })
    .await
}

fn vm_state(state: &AppState, vm_id: Uuid) -> Option<VmState> {
    state
        .vms
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&vm_id)
        .map(|vm| vm.state)
}

async fn blocking<T, F>(work: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, PoolStoreError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use firecrab_api_types::EgressPolicy;

    use super::*;
    use crate::firecracker::test_support::short_tempdir;
    use crate::handlers::vms::test_support::{record, seed_vm, test_state};
    use crate::persistence::PoolRow;

    fn pool_row() -> PoolRow {
        PoolRow {
            id: Uuid::new_v4(),
            name: "ci".to_owned(),
            template: "ubuntu-rootfs-26.04".to_owned(),
            template_version: "v1".to_owned(),
            cpu: 1,
            ram: 128,
            disk_gb: 1,
            egress_policy: EgressPolicy::Internet,
            micro_network_id: Uuid::from_u128(1),
            storage_root: "default".to_owned(),
            min_ready: 0,
            max_size: 2,
            lease_ttl_seconds: 600,
            deleting: false,
            created_at_ms: 1,
        }
    }

    /// A member seen as ready (and dead) can be leased by a concurrent
    /// acquire before the reconciler acts on it. Draining it then would
    /// delete a VM a caller now holds.
    #[tokio::test]
    async fn a_member_leased_since_it_was_read_is_not_drained() {
        let directory = short_tempdir();
        let state = test_state(directory.path()).await;
        let pool = pool_row();
        state.store.insert_pool(&pool).unwrap();
        let mut vm = record("pool-ci-1", Uuid::new_v4());
        vm.purpose = VmPurpose::Pool;
        vm.state = VmState::Stopped;
        seed_vm(&state, &vm);
        let seen = MemberRow {
            vm_id: vm.id,
            pool_id: pool.id,
            state: PoolMemberState::Ready,
            created_at_ms: 1,
        };
        state.store.insert_pool_member(&seen).unwrap();
        state
            .store
            .acquire_pool_lease(pool.id, None, 0, |_| true)
            .unwrap();

        settle(&state, &pool, seen).await.unwrap();

        assert!(
            state.vms.lock().unwrap().contains_key(&vm.id),
            "the leased VM must survive"
        );
    }
}

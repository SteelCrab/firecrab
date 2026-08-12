//! Shared plumbing for "builder" VMs: the short-lived, `list_vms`-hidden
//! instances a long-running image job drives from the outside.
//!
//! A builder VM is a completely ordinary VM — same create/start/console/
//! delete code path as any dashboard-created instance — tagged
//! `VmPurpose::Builder` so `list_vms` hides it. Only
//! `handlers::bootstrap` (from-scratch distro sessions) builds one today;
//! these helpers live apart from it so the naming/sizing/tagging rules stay
//! one thing rather than being inlined into a single session flow.

use uuid::Uuid;

use crate::error::AppError;
use crate::model::VmPurpose;
use crate::state::AppState;

/// Names a builder VM so it's recognizable if an operator inspects
/// `data/firecrab.db` directly; not shown anywhere in the dashboard since
/// `list_vms` filters `Builder` records out.
pub(crate) fn builder_vm_name(alias: &str) -> String {
    format!("builder-{alias}-{}", &Uuid::new_v4().to_string()[..8])
}

/// Builder VMs need headroom beyond the source rootfs to install new
/// packages into — a fixed 2 GiB margin over the template's own floor,
/// matching `handlers::images::min_disk_gb_for`'s ceiling logic.
/// `handlers::bootstrap` takes the larger of this floor and its own
/// per-target build budget.
pub(crate) fn builder_disk_gb(rootfs_bytes: u64) -> u16 {
    const GIB: u64 = 1024 * 1024 * 1024;
    let floor: u16 = rootfs_bytes.div_ceil(GIB).try_into().unwrap_or(u16::MAX);
    floor.saturating_add(2)
}

/// Picks the first MicroNetwork with internet egress enabled — a builder
/// needs to reach the guest's package repositories. Fails clearly rather
/// than silently picking an isolated network a package install would hang
/// against.
pub(crate) async fn builder_micro_network_id(
    state: &AppState,
    request_id: Uuid,
) -> Result<Uuid, AppError> {
    let store = state.store.clone();
    let networks = tokio::task::spawn_blocking(move || store.list_micro_networks())
        .await
        .map_err(|_| AppError::internal(request_id))?
        .map_err(|_| AppError::internal(request_id))?;
    networks
        .into_iter()
        .find(|network| network.internet_enabled)
        .map(|network| network.id)
        .ok_or_else(|| {
            AppError::unavailable(
                "no MicroNetwork with internet access exists — create one before building images",
                request_id,
            )
        })
}

/// Flags the just-created VM as a builder so `list_vms` hides it, then
/// persists that change the same way `handlers::vms::persist_update` does.
pub(crate) async fn mark_as_builder(
    state: &AppState,
    vm_id: Uuid,
    request_id: Uuid,
) -> Result<(), AppError> {
    let record = {
        let mut vms = state
            .vms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let vm = vms
            .get_mut(&vm_id)
            .ok_or_else(|| AppError::internal(request_id))?;
        vm.purpose = VmPurpose::Builder;
        vm.clone()
    };
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || store.update(&record))
        .await
        .map_err(|_| AppError::internal(request_id))?
        .map_err(|_| AppError::internal(request_id))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use tempfile::tempdir;

    use super::*;
    use crate::handlers::vms::test_support::{record, seed_vm, test_state};

    #[test]
    fn builder_vm_name_is_recognizable_and_unique_per_call() {
        let first = builder_vm_name("alpine-3.24.1");
        let second = builder_vm_name("alpine-3.24.1");

        assert!(first.starts_with("builder-alpine-3.24.1-"));
        assert_ne!(first, second, "two builders must not collide on a name");
    }

    #[test]
    fn builder_disk_gb_leaves_install_headroom_over_the_rootfs_floor() {
        const GIB: u64 = 1024 * 1024 * 1024;

        // Ceiled to the next whole GiB, then +2 for packages to land in.
        assert_eq!(builder_disk_gb(GIB), 3);
        assert_eq!(builder_disk_gb(GIB + 1), 4);
        // A rootfs big enough to overflow the u16 ceiling saturates instead
        // of wrapping around to a disk far smaller than the source.
        assert_eq!(builder_disk_gb(u64::MAX), u16::MAX);
    }

    #[tokio::test]
    async fn builder_micro_network_id_picks_the_internet_enabled_network() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let seeded =
            crate::handlers::micro_networks::test_support::seed_internet_micro_network(&state);

        let picked = builder_micro_network_id(&state, Uuid::new_v4())
            .await
            .unwrap();

        assert_eq!(picked, seeded);
    }

    #[tokio::test]
    async fn builder_micro_network_id_reports_unavailable_when_none_has_internet() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        // Deliberately no `seed_internet_micro_network` call: a builder with
        // no route to the package repositories must fail loudly here rather
        // than boot onto an isolated network and hang mid-install.

        let error = builder_micro_network_id(&state, Uuid::new_v4())
            .await
            .unwrap_err();

        assert_eq!(
            error.into_response().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn mark_as_builder_hides_the_vm_and_persists_the_change() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;
        let vm = record("builder-vm", Uuid::new_v4());
        seed_vm(&state, &vm);

        mark_as_builder(&state, vm.id, Uuid::new_v4())
            .await
            .unwrap();

        assert_eq!(
            state.vms.lock().unwrap().get(&vm.id).unwrap().purpose,
            VmPurpose::Builder
        );
        // Persisted, not just in-memory: a restart must not resurrect the
        // builder as an ordinary VM in the dashboard's list.
        let stored = state.store.load_all().unwrap();
        assert_eq!(stored.get(&vm.id).unwrap().purpose, VmPurpose::Builder);

        let axum::Json(listed) = crate::handlers::vms::list_vms(axum::extract::State(state)).await;
        assert!(!listed.iter().any(|entry| entry.id == vm.id));
    }

    #[tokio::test]
    async fn mark_as_builder_fails_when_the_vm_record_is_gone() {
        let directory = tempdir().unwrap();
        let state = test_state(directory.path()).await;

        let error = mark_as_builder(&state, Uuid::new_v4(), Uuid::new_v4())
            .await
            .unwrap_err();

        assert_eq!(
            error.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}

//! Shared application state: the in-memory VM record cache, live process
//! table, and runtime configuration every handler operates against.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::firecracker::{self, VmProcess};
use crate::image_install::ImageInstallTracker;
use crate::model::VmRecord;
use crate::network::NetworkClient;
use crate::persistence::{self, PersistenceError, Store};
use crate::storage::StorageRegistry;
use crate::templates::TemplateRegistry;

/// Environment-derived settings for spawning/stopping Firecracker.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Default root directory for per-VM state (disk, config, console log).
    /// Prefer [`AppState::vms_dir_for`] so multi-disk VMs resolve correctly.
    pub vms_dir: PathBuf,
    /// Path to the Firecracker binary.
    pub firecracker_binary: PathBuf,
    /// How long to wait for the API socket to become ready on start.
    pub ready_timeout: Duration,
    /// Grace period before escalating SIGTERM to SIGKILL on stop.
    pub stop_grace: Duration,
    /// How long to wait for the guest's network-readiness sentinel on its
    /// serial console before failing the start (see
    /// `handlers::vms::wait_for_network_ready`).
    pub network_ready_timeout: Duration,
}

impl RuntimeConfig {
    /// Builds config from environment-derived defaults.
    fn from_defaults(storage: &StorageRegistry) -> Self {
        Self {
            vms_dir: storage.default_vms_dir(),
            firecracker_binary: firecracker::default_firecracker_binary(),
            ready_timeout: Duration::from_secs(10),
            stop_grace: Duration::from_secs(5),
            network_ready_timeout: Duration::from_secs(30),
        }
    }
}

/// How many `run_start` calls may copy/grow a rootfs disk at once. Each
/// copy is a multi-GB sequential read+write; letting every concurrently
/// starting VM race for disk bandwidth at once makes all of them slower
/// (seek thrashing) instead of finishing any of them sooner, which reads to
/// dashboard users as VMs "stuck" in the disk-prep step
/// (`docs/40-tests/vm-startup-progress.md`).
const DISK_PREP_CONCURRENCY: usize = 2;

/// Shared state handed to every handler: in-memory VM cache, live process
/// table, template registry, and runtime config.
#[derive(Debug, Clone)]
pub struct AppState {
    /// In-memory cache of every VM record, mirrored to [`AppState::store`].
    pub vms: Arc<Mutex<HashMap<Uuid, VmRecord>>>,
    /// Verified boot template registry.
    pub templates: Arc<TemplateRegistry>,
    /// SQLite-backed durable storage.
    pub(crate) store: Store,
    /// Live VM id -> its running Firecracker process handle.
    pub(crate) processes: Arc<Mutex<HashMap<Uuid, VmProcess>>>,
    /// Environment-derived runtime settings.
    pub(crate) runtime: Arc<RuntimeConfig>,
    /// Admin-registered storage roots (`FIRECRAB_STORAGE_ROOTS`).
    pub(crate) storage: Arc<StorageRegistry>,
    /// Bounds how many `start_vm` calls copy/grow a rootfs disk at once.
    pub(crate) disk_prep_permits: Arc<Semaphore>,
    /// Client for the privileged `firecrab-net-helper` (bridge/TAP/firewall).
    pub(crate) network: NetworkClient,
    /// Async image registration jobs (`POST /api/images/{alias}/install`).
    pub(crate) image_installs: ImageInstallTracker,
    /// Async package acquisition jobs (`POST /api/images/{alias}/package`).
    pub(crate) image_packages: ImageInstallTracker,
    /// Async image-build sessions (`POST /api/images/{alias}/build`).
    pub(crate) builds: crate::builds::BuildTracker,
}

impl AppState {
    /// Builds state backed by the default SQLite database path.
    pub async fn new(templates: TemplateRegistry) -> Result<Self, PersistenceError> {
        Self::with_db_file(templates, persistence::default_db_file()).await
    }

    /// Builds state backed by an explicit database path, resetting any
    /// leftover live states from a previous run to `Stopped` and loading
    /// every record into the in-memory cache.
    pub(crate) async fn with_db_file(
        templates: TemplateRegistry,
        db_file: PathBuf,
    ) -> Result<Self, PersistenceError> {
        let (store, vms) = tokio::task::spawn_blocking(move || {
            let store = Store::open(&db_file)?;
            // A fresh server has no processes, so live states from the
            // previous run are ghosts — demote them before serving.
            store.reset_active_states()?;
            let vms = store.load_all()?;
            Ok::<_, PersistenceError>((store, vms))
        })
        .await
        .expect("persistence startup task panicked")?;

        let storage = StorageRegistry::from_env();
        let runtime = RuntimeConfig::from_defaults(&storage);
        Ok(AppState {
            vms: Arc::new(Mutex::new(vms)),
            templates: Arc::new(templates),
            store,
            processes: Arc::new(Mutex::new(HashMap::new())),
            runtime: Arc::new(runtime),
            storage: Arc::new(storage),
            disk_prep_permits: Arc::new(Semaphore::new(DISK_PREP_CONCURRENCY)),
            network: NetworkClient::from_env(),
            image_installs: ImageInstallTracker::from_env(),
            image_packages: ImageInstallTracker::from_env(),
            builds: crate::builds::BuildTracker::default(),
        })
    }

    /// `{storage_root}/vms` for this VM. Resolves env/default roots first,
    /// then MicroStorage rows in the DB. Falls back to the default root if
    /// the id vanished (delete still works; start fails with a clear error
    /// when capacity/path is checked at create).
    pub(crate) fn vms_dir_for(&self, storage_root: &str) -> PathBuf {
        if let Ok(dir) = self.storage.vms_dir(storage_root) {
            return dir;
        }
        if let Ok(uuid) = Uuid::parse_str(storage_root)
            && let Ok(Some((_, _, path))) = self.store.micro_storage(uuid)
        {
            return PathBuf::from(path).join("vms");
        }
        self.runtime.vms_dir.clone()
    }

    /// Resolves a selectable storage id to its host path (not the `vms/`
    /// child). Used for free-space checks and list labels.
    pub(crate) fn storage_path_for(
        &self,
        storage_root: &str,
    ) -> Result<PathBuf, crate::storage::StorageError> {
        if let Some(root) = self.storage.get(storage_root) {
            return Ok(root.path.clone());
        }
        if let Ok(uuid) = Uuid::parse_str(storage_root)
            && let Ok(Some((_, _, path))) = self.store.micro_storage(uuid)
        {
            return Ok(PathBuf::from(path));
        }
        Err(crate::storage::StorageError::UnknownRoot(
            storage_root.to_owned(),
        ))
    }

    /// Whether `storage_root` is known (env/default or MicroStorage).
    pub(crate) fn storage_root_known(&self, storage_root: &str) -> bool {
        self.storage_path_for(storage_root).is_ok()
    }

    /// Free-space gate for create/assign.
    pub(crate) fn ensure_storage_capacity(
        &self,
        storage_root: &str,
        need_bytes: u64,
    ) -> Result<(), crate::storage::StorageError> {
        let path = self.storage_path_for(storage_root)?;
        crate::storage::StorageRegistry::ensure_capacity_at(&path, need_bytes)
    }

    /// Overrides the net-helper client, for tests that spin up a fake
    /// net-helper (see `network::test_support`) instead of a real one.
    #[cfg(test)]
    pub(crate) fn with_test_network(mut self, network: NetworkClient) -> Self {
        self.network = network;
        self
    }

    /// Overrides the runtime config, for tests that need a fake Firecracker
    /// binary or a short timeout. Also rewires storage so the default root's
    /// `vms` dir matches `runtime.vms_dir`.
    #[cfg(test)]
    pub(crate) fn with_test_runtime(mut self, runtime: RuntimeConfig) -> Self {
        let root_path = runtime
            .vms_dir
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| runtime.vms_dir.clone());
        self.storage = Arc::new(StorageRegistry::single("default", root_path));
        self.runtime = Arc::new(runtime);
        self
    }

    /// Replaces the storage registry (multi-disk create tests).
    #[cfg(test)]
    pub(crate) fn with_test_storage(mut self, storage: StorageRegistry) -> Self {
        let runtime = RuntimeConfig {
            vms_dir: storage.default_vms_dir(),
            firecracker_binary: self.runtime.firecracker_binary.clone(),
            ready_timeout: self.runtime.ready_timeout,
            stop_grace: self.runtime.stop_grace,
            network_ready_timeout: self.runtime.network_ready_timeout,
        };
        self.storage = Arc::new(storage);
        self.runtime = Arc::new(runtime);
        self
    }
}

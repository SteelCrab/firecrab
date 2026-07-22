//! Shared application state: the in-memory VM record cache, live process
//! table, and runtime configuration every handler operates against.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::firecracker::{self, VmProcess};
use crate::model::VmRecord;
use crate::persistence::{self, PersistenceError, Store};
use crate::rootfs;
use crate::templates::TemplateRegistry;

/// Environment-derived settings for spawning/stopping Firecracker.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Root directory for per-VM state (disk, config, console log).
    pub vms_dir: PathBuf,
    /// Path to the Firecracker binary.
    pub firecracker_binary: PathBuf,
    /// How long to wait for the API socket to become ready on start.
    pub ready_timeout: Duration,
    /// Grace period before escalating SIGTERM to SIGKILL on stop.
    pub stop_grace: Duration,
}

impl RuntimeConfig {
    /// Builds config from environment-derived defaults.
    fn from_defaults() -> Self {
        Self {
            vms_dir: rootfs::default_vms_dir(),
            firecracker_binary: firecracker::default_firecracker_binary(),
            ready_timeout: Duration::from_secs(10),
            stop_grace: Duration::from_secs(5),
        }
    }
}

/// How many `run_start` calls may copy/grow a rootfs disk at once. Each
/// copy is a multi-GB sequential read+write; letting every concurrently
/// starting VM race for disk bandwidth at once makes all of them slower
/// (seek thrashing) instead of finishing any of them sooner, which reads to
/// dashboard users as VMs "stuck" in the disk-prep step
/// (`docs/tests/vm-startup-progress.md`).
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
    /// Bounds how many `start_vm` calls copy/grow a rootfs disk at once.
    pub(crate) disk_prep_permits: Arc<Semaphore>,
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

        Ok(AppState {
            vms: Arc::new(Mutex::new(vms)),
            templates: Arc::new(templates),
            store,
            processes: Arc::new(Mutex::new(HashMap::new())),
            runtime: Arc::new(RuntimeConfig::from_defaults()),
            disk_prep_permits: Arc::new(Semaphore::new(DISK_PREP_CONCURRENCY)),
        })
    }

    /// Overrides the runtime config, for tests that need a fake Firecracker
    /// binary or a short timeout.
    #[cfg(test)]
    pub(crate) fn with_test_runtime(mut self, runtime: RuntimeConfig) -> Self {
        self.runtime = Arc::new(runtime);
        self
    }
}

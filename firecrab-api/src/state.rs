use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

use crate::firecracker::{self, VmProcess};
use crate::model::VmRecord;
use crate::persistence::{self, PersistenceError, Store};
use crate::rootfs;
use crate::templates::TemplateRegistry;

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub vms_dir: PathBuf,
    pub firecracker_binary: PathBuf,
    pub ready_timeout: Duration,
    pub stop_grace: Duration,
}

impl RuntimeConfig {
    fn from_defaults() -> Self {
        Self {
            vms_dir: rootfs::default_vms_dir(),
            firecracker_binary: firecracker::default_firecracker_binary(),
            ready_timeout: Duration::from_secs(10),
            stop_grace: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub vms: Arc<Mutex<HashMap<Uuid, VmRecord>>>,
    pub templates: Arc<TemplateRegistry>,
    pub(crate) store: Store,
    pub(crate) processes: Arc<Mutex<HashMap<Uuid, VmProcess>>>,
    pub(crate) runtime: Arc<RuntimeConfig>,
}

impl AppState {
    pub async fn new(templates: TemplateRegistry) -> Result<Self, PersistenceError> {
        Self::with_db_file(templates, persistence::default_db_file()).await
    }

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
        })
    }

    #[cfg(test)]
    pub(crate) fn with_test_runtime(mut self, runtime: RuntimeConfig) -> Self {
        self.runtime = Arc::new(runtime);
        self
    }
}

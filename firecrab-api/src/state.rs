use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use crate::model::VmRecord;
use crate::persistence::{self, PersistenceError, Store};
use crate::templates::TemplateRegistry;

#[derive(Debug, Clone)]
pub struct AppState {
    pub vms: Arc<Mutex<HashMap<Uuid, VmRecord>>>,
    pub templates: Arc<TemplateRegistry>,
    pub(crate) store: Store,
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
            let vms = store.load_all()?;
            Ok::<_, PersistenceError>((store, vms))
        })
        .await
        .expect("persistence startup task panicked")?;

        Ok(AppState {
            vms: Arc::new(Mutex::new(vms)),
            templates: Arc::new(templates),
            store,
        })
    }
}

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::model::VmRecord;
use crate::persistence::{self, PersistenceError};
use crate::templates::TemplateRegistry;

#[derive(Debug, Clone)]
pub struct AppState {
    pub vms: Arc<Mutex<HashMap<Uuid, VmRecord>>>,
    pub templates: Arc<TemplateRegistry>,
    pub(crate) data_file: Arc<PathBuf>,
    pub(crate) persistence_writer: Arc<AsyncMutex<()>>,
}

impl AppState {
    pub async fn new(templates: TemplateRegistry) -> Result<Self, PersistenceError> {
        Self::with_data_file(templates, persistence::default_data_file()).await
    }

    pub(crate) async fn with_data_file(
        templates: TemplateRegistry,
        data_file: PathBuf,
    ) -> Result<Self, PersistenceError> {
        let vms = persistence::load(&data_file).await?;
        Ok(AppState {
            vms: Arc::new(Mutex::new(vms)),
            templates: Arc::new(templates),
            data_file: Arc::new(data_file),
            persistence_writer: Arc::new(AsyncMutex::new(())),
        })
    }
}

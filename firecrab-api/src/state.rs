use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use crate::model::VmRecord;
use crate::persistence;
use crate::templates::TemplateRegistry;

#[derive(Debug, Clone)]
pub struct AppState {
    pub vms: Arc<Mutex<HashMap<Uuid, VmRecord>>>,
    pub templates: Arc<TemplateRegistry>,
}

impl AppState {
    pub fn new(templates: TemplateRegistry) -> Self {
        AppState {
            vms: Arc::new(Mutex::new(persistence::load())),
            templates: Arc::new(templates),
        }
    }
}

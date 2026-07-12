use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use crate::model::VmRecord;
use crate::persistence;

#[derive(Debug, Clone, Default)]
pub struct AppState {
    pub vms: Arc<Mutex<HashMap<Uuid, VmRecord>>>,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            vms: Arc::new(Mutex::new(persistence::load())),
        }
    }
}

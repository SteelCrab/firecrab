use std::collections::HashMap;
use std::fs;
use std::path::Path;

use uuid::Uuid;

use crate::model::VmRecord;

const DATA_FILE: &str = "data/vms.json";

pub fn load() -> HashMap<Uuid, VmRecord> {
    match fs::read_to_string(DATA_FILE) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

pub fn save(vms: &HashMap<Uuid, VmRecord>) {
    if let Some(dir) = Path::new(DATA_FILE).parent() {
        fs::create_dir_all(dir).expect("failed to create data directory");
    }
    let json = serde_json::to_string_pretty(vms).expect("failed to serialize vms");
    fs::write(DATA_FILE, json).expect("failed to write vms.json");
}

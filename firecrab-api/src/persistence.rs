use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;
use tokio::fs;
use uuid::Uuid;

use crate::model::VmRecord;

const DATA_FILE: &str = "data/vms.json";

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("failed to read VM data from {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to deserialize VM data from {path}: {source}")]
    Deserialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to serialize VM data for {path}: {source}")]
    Serialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to create VM data directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to write VM data to {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub fn default_data_file() -> PathBuf {
    PathBuf::from(DATA_FILE)
}

pub async fn load(path: &Path) -> Result<HashMap<Uuid, VmRecord>, PersistenceError> {
    match fs::read(path).await {
        Ok(content) => {
            serde_json::from_slice(&content).map_err(|source| PersistenceError::Deserialize {
                path: path.to_owned(),
                source,
            })
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(HashMap::new()),
        Err(source) => Err(PersistenceError::Read {
            path: path.to_owned(),
            source,
        }),
    }
}

pub async fn save(path: &Path, vms: &HashMap<Uuid, VmRecord>) -> Result<(), PersistenceError> {
    let json = serde_json::to_vec_pretty(vms).map_err(|source| PersistenceError::Serialize {
        path: path.to_owned(),
        source,
    })?;

    if let Some(directory) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(directory).await.map_err(|source| {
            PersistenceError::CreateDirectory {
                path: directory.to_owned(),
                source,
            }
        })?;
    }

    fs::write(path, json)
        .await
        .map_err(|source| PersistenceError::Write {
            path: path.to_owned(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::model::VmState;

    fn record(id: Uuid) -> VmRecord {
        VmRecord {
            id,
            name: "test-vm".to_owned(),
            state: VmState::Created,
            template: "ubuntu-rootfs-26.04".to_owned(),
            template_version: "v1".to_owned(),
            template_kernel_sha256: "kernel".to_owned(),
            template_rootfs_sha256: "rootfs".to_owned(),
            template_boot_args_sha256: "args".to_owned(),
            cpu: 1,
            ram: 512,
        }
    }

    #[tokio::test]
    async fn missing_file_loads_an_empty_store() {
        let directory = tempdir().unwrap();
        let loaded = load(&directory.path().join("missing.json")).await.unwrap();

        assert!(loaded.is_empty());
    }

    #[tokio::test]
    async fn malformed_json_is_not_treated_as_an_empty_store() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("vms.json");
        fs::write(&path, b"{invalid").await.unwrap();

        assert!(matches!(
            load(&path).await,
            Err(PersistenceError::Deserialize { .. })
        ));
    }

    #[tokio::test]
    async fn non_not_found_read_error_is_returned() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("vms.json");
        fs::create_dir(&path).await.unwrap();

        assert!(matches!(
            load(&path).await,
            Err(PersistenceError::Read { .. })
        ));
    }

    #[tokio::test]
    async fn records_round_trip() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("nested/vms.json");
        let id = Uuid::new_v4();
        let records = HashMap::from([(id, record(id))]);

        save(&path, &records).await.unwrap();
        let loaded = load(&path).await.unwrap();

        assert_eq!(loaded.get(&id).unwrap().name, "test-vm");
    }

    #[tokio::test]
    async fn directory_creation_failure_is_returned_instead_of_panicking() {
        let directory = tempdir().unwrap();
        let blocker = directory.path().join("not-a-directory");
        fs::write(&blocker, b"file").await.unwrap();

        assert!(matches!(
            save(&blocker.join("vms.json"), &HashMap::new()).await,
            Err(PersistenceError::CreateDirectory { .. })
        ));
    }
}

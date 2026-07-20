use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, TransactionBehavior, params};
use thiserror::Error;
use uuid::Uuid;

use crate::ipam::{self, IpamError};
use crate::model::{Lease, VmRecord, VmState};

const DB_FILE: &str = "data/firecrab.db";
const LEGACY_FILE_NAME: &str = "vms.json";

const CREATE_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS vms (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    state TEXT NOT NULL,
    template TEXT NOT NULL,
    template_version TEXT NOT NULL,
    template_kernel_sha256 TEXT NOT NULL,
    template_rootfs_sha256 TEXT NOT NULL,
    template_boot_args_sha256 TEXT NOT NULL,
    cpu INTEGER NOT NULL,
    ram INTEGER NOT NULL
) STRICT";

const SELECT_ALL_SQL: &str = "SELECT id, name, state, template, template_version, \
    template_kernel_sha256, template_rootfs_sha256, template_boot_args_sha256, cpu, ram \
    FROM vms";

const INSERT_SQL: &str = "INSERT INTO vms (id, name, state, template, template_version, \
    template_kernel_sha256, template_rootfs_sha256, template_boot_args_sha256, cpu, ram) \
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";

const IMPORT_SQL: &str = "INSERT OR REPLACE INTO vms (id, name, state, template, \
    template_version, template_kernel_sha256, template_rootfs_sha256, \
    template_boot_args_sha256, cpu, ram) \
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";

const UPDATE_SQL: &str = "UPDATE vms SET name = ?2, state = ?3, template = ?4, \
    template_version = ?5, template_kernel_sha256 = ?6, template_rootfs_sha256 = ?7, \
    template_boot_args_sha256 = ?8, cpu = ?9, ram = ?10 \
    WHERE id = ?1";

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("failed to create VM data directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to open VM database {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("VM database operation failed: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("VM database record {id} is invalid: {reason}")]
    CorruptRecord { id: String, reason: String },
    #[error("VM {id} does not exist in the database")]
    MissingVm { id: Uuid },
    #[error("failed to read legacy VM data from {path}: {source}")]
    LegacyRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to deserialize legacy VM data from {path}: {source}")]
    LegacyDeserialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to archive imported legacy VM data {path}: {source}")]
    LegacyArchive {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub fn default_db_file() -> PathBuf {
    PathBuf::from(DB_FILE)
}

#[derive(Debug, Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, PersistenceError> {
        if let Some(directory) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(directory).map_err(|source| PersistenceError::CreateDirectory {
                path: directory.to_owned(),
                source,
            })?;
        }

        let conn = Connection::open(path).map_err(|source| PersistenceError::Open {
            path: path.to_owned(),
            source,
        })?;
        let _mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute(CREATE_TABLE_SQL, [])?;
        conn.execute(ipam::CREATE_LEASES_TABLE_SQL, [])?;
        for index_sql in ipam::CREATE_LEASES_INDEXES_SQL {
            conn.execute(index_sql, [])?;
        }

        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.import_legacy(&path.with_file_name(LEGACY_FILE_NAME))?;
        Ok(store)
    }

    pub fn load_all(&self) -> Result<HashMap<Uuid, VmRecord>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare(SELECT_ALL_SQL)?;
        let mut rows = statement.query([])?;
        let mut vms = HashMap::new();
        while let Some(row) = rows.next()? {
            let id_text: String = row.get(0)?;
            let id =
                Uuid::parse_str(&id_text).map_err(|_| PersistenceError::CorruptRecord {
                    id: id_text.clone(),
                    reason: "id is not a UUID".to_owned(),
                })?;
            let state_text: String = row.get(2)?;
            vms.insert(
                id,
                VmRecord {
                    id,
                    name: row.get(1)?,
                    state: decode_state(&id_text, &state_text)?,
                    template: row.get(3)?,
                    template_version: row.get(4)?,
                    template_kernel_sha256: row.get(5)?,
                    template_rootfs_sha256: row.get(6)?,
                    template_boot_args_sha256: row.get(7)?,
                    cpu: row.get(8)?,
                    ram: row.get(9)?,
                    startup_step: None,
                },
            );
        }
        Ok(vms)
    }

    pub fn insert(&self, vm: &VmRecord) -> Result<(), PersistenceError> {
        execute_record(&self.lock(), INSERT_SQL, vm)?;
        Ok(())
    }

    pub fn update(&self, vm: &VmRecord) -> Result<(), PersistenceError> {
        if execute_record(&self.lock(), UPDATE_SQL, vm)? == 0 {
            return Err(PersistenceError::MissingVm { id: vm.id });
        }
        Ok(())
    }

    pub fn delete(&self, id: Uuid) -> Result<(), PersistenceError> {
        let changed = self
            .lock()
            .execute("DELETE FROM vms WHERE id = ?1", params![id.to_string()])?;
        if changed == 0 {
            return Err(PersistenceError::MissingVm { id });
        }
        Ok(())
    }

    /// Allocate an IPv4 + MAC for `vm_id` inside a `BEGIN IMMEDIATE`
    /// transaction, serializing concurrent allocations on the same lock.
    pub fn allocate_lease(&self, vm_id: Uuid) -> Result<Lease, IpamError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let lease = ipam::allocate(&tx, vm_id)?;
        tx.commit()?;
        Ok(lease)
    }

    /// Release `vm_id`'s active lease; the row stays as history. Call only
    /// after VM cleanup (policy, TAP, artifacts) has fully succeeded.
    pub fn release_lease(&self, vm_id: Uuid) -> Result<(), IpamError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ipam::release(&tx, vm_id)?;
        tx.commit()?;
        Ok(())
    }

    /// Startup cleanup: a VM left in a live state by a previous run has no
    /// process behind it anymore, so demote it to stopped.
    pub fn reset_active_states(&self) -> Result<usize, PersistenceError> {
        let changed = self.lock().execute(
            "UPDATE vms SET state = ?1 WHERE state IN (?2, ?3, ?4)",
            params![
                encode_state(VmState::Stopped),
                encode_state(VmState::Starting),
                encode_state(VmState::Running),
                encode_state(VmState::Stopping),
            ],
        )?;
        Ok(changed)
    }

    fn import_legacy(&self, legacy: &Path) -> Result<(), PersistenceError> {
        let content = match fs::read(legacy) {
            Ok(content) => content,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(PersistenceError::LegacyRead {
                    path: legacy.to_owned(),
                    source,
                });
            }
        };
        let records: HashMap<Uuid, VmRecord> = serde_json::from_slice(&content).map_err(
            |source| PersistenceError::LegacyDeserialize {
                path: legacy.to_owned(),
                source,
            },
        )?;

        {
            let mut conn = self.lock();
            let tx = conn.transaction()?;
            for vm in records.values() {
                execute_record(&tx, IMPORT_SQL, vm)?;
            }
            tx.commit()?;
        }

        fs::rename(legacy, legacy.with_extension("json.imported")).map_err(|source| {
            PersistenceError::LegacyArchive {
                path: legacy.to_owned(),
                source,
            }
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[cfg(test)]
    pub(crate) fn break_for_tests(&self) {
        self.lock().execute("DROP TABLE vms", []).unwrap();
    }
}

fn execute_record(
    conn: &Connection,
    sql: &str,
    vm: &VmRecord,
) -> Result<usize, rusqlite::Error> {
    conn.execute(
        sql,
        params![
            vm.id.to_string(),
            vm.name,
            encode_state(vm.state),
            vm.template,
            vm.template_version,
            vm.template_kernel_sha256,
            vm.template_rootfs_sha256,
            vm.template_boot_args_sha256,
            vm.cpu,
            vm.ram,
        ],
    )
}

// Encode/decode through serde so the DB text stays in lockstep with the API wire format.
pub(crate) fn encode_state(state: VmState) -> String {
    match serde_json::to_value(state) {
        Ok(serde_json::Value::String(name)) => name,
        _ => unreachable!("VmState serializes to a string"),
    }
}

fn decode_state(id: &str, name: &str) -> Result<VmState, PersistenceError> {
    serde_json::from_value(serde_json::Value::String(name.to_owned())).map_err(|_| {
        PersistenceError::CorruptRecord {
            id: id.to_owned(),
            reason: format!("unknown state {name:?}"),
        }
    })
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn record(id: Uuid, name: &str) -> VmRecord {
        VmRecord {
            id,
            name: name.to_owned(),
            state: VmState::Created,
            template: "ubuntu-26.04".to_owned(),
            template_version: "ubuntu-26.04-v1".to_owned(),
            template_kernel_sha256: "kernel".to_owned(),
            template_rootfs_sha256: "rootfs".to_owned(),
            template_boot_args_sha256: "args".to_owned(),
            cpu: 1,
            ram: 512,
            startup_step: None,
        }
    }

    #[test]
    fn crud_round_trips() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("nested/firecrab.db")).unwrap();
        assert!(store.load_all().unwrap().is_empty());

        let first = record(Uuid::new_v4(), "first");
        let mut second = record(Uuid::new_v4(), "second");
        store.insert(&first).unwrap();
        store.insert(&second).unwrap();
        let expected = HashMap::from([(first.id, first.clone()), (second.id, second.clone())]);
        assert_eq!(store.load_all().unwrap(), expected);

        second.state = VmState::Running;
        second.ram = 1024;
        store.update(&second).unwrap();
        assert_eq!(store.load_all().unwrap().get(&second.id), Some(&second));

        store.delete(first.id).unwrap();
        let remaining = store.load_all().unwrap();
        assert_eq!(remaining.len(), 1);
        assert!(remaining.contains_key(&second.id));

        assert!(matches!(
            store.delete(first.id),
            Err(PersistenceError::MissingVm { id }) if id == first.id
        ));
        assert!(matches!(
            store.update(&record(Uuid::new_v4(), "ghost")),
            Err(PersistenceError::MissingVm { .. })
        ));
    }

    #[test]
    fn reset_demotes_live_states_to_stopped() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("firecrab.db")).unwrap();
        let states = [
            VmState::Created,
            VmState::Starting,
            VmState::Running,
            VmState::Stopping,
            VmState::Stopped,
            VmState::Error,
        ];
        let mut ids = Vec::new();
        for state in states {
            let mut vm = record(Uuid::new_v4(), "vm");
            vm.state = state;
            store.insert(&vm).unwrap();
            ids.push((vm.id, state));
        }

        assert_eq!(store.reset_active_states().unwrap(), 3);

        let all = store.load_all().unwrap();
        for (id, before) in ids {
            let expected = match before {
                VmState::Starting | VmState::Running | VmState::Stopping => VmState::Stopped,
                other => other,
            };
            assert_eq!(all.get(&id).unwrap().state, expected, "{before:?}");
        }
    }

    #[test]
    fn records_survive_reopen() {
        let directory = tempdir().unwrap();
        let db_file = directory.path().join("firecrab.db");
        let vm = record(Uuid::new_v4(), "durable");

        let store = Store::open(&db_file).unwrap();
        store.insert(&vm).unwrap();
        drop(store);

        let reopened = Store::open(&db_file).unwrap();
        assert_eq!(reopened.load_all().unwrap().get(&vm.id), Some(&vm));
    }

    #[test]
    fn imports_legacy_vms_json_exactly_once() {
        let directory = tempdir().unwrap();
        let db_file = directory.path().join("firecrab.db");
        let legacy_file = directory.path().join("vms.json");
        let first = record(Uuid::new_v4(), "legacy-a");
        let second = record(Uuid::new_v4(), "legacy-b");
        let legacy = HashMap::from([(first.id, first.clone()), (second.id, second.clone())]);
        fs::write(&legacy_file, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

        let store = Store::open(&db_file).unwrap();
        assert_eq!(store.load_all().unwrap(), legacy);
        assert!(!legacy_file.exists());
        assert!(directory.path().join("vms.json.imported").exists());

        let extra = record(Uuid::new_v4(), "post-import");
        store.insert(&extra).unwrap();
        drop(store);

        let reopened = Store::open(&db_file).unwrap();
        let all = reopened.load_all().unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all.get(&extra.id), Some(&extra));
    }

    #[test]
    fn malformed_legacy_json_fails_open() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("vms.json"), b"{invalid").unwrap();

        assert!(matches!(
            Store::open(&directory.path().join("firecrab.db")),
            Err(PersistenceError::LegacyDeserialize { .. })
        ));
    }

    #[test]
    fn opens_in_wal_mode() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("firecrab.db")).unwrap();

        let mode: String = store
            .lock()
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn concurrent_lease_allocation_never_hands_out_duplicates() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("firecrab.db")).unwrap();

        let handles: Vec<_> = (0..16)
            .map(|_| {
                let store = store.clone();
                std::thread::spawn(move || store.allocate_lease(Uuid::new_v4()).unwrap())
            })
            .collect();
        let leases: Vec<Lease> = handles.into_iter().map(|handle| handle.join().unwrap()).collect();

        let mut ips: Vec<_> = leases.iter().map(|lease| lease.ipv4).collect();
        let mut macs: Vec<_> = leases.iter().map(|lease| lease.mac).collect();
        ips.sort();
        macs.sort_by_key(|mac| mac.0);
        let unique_ip_count = {
            let mut deduped = ips.clone();
            deduped.dedup();
            deduped.len()
        };
        let unique_mac_count = {
            let mut deduped = macs.clone();
            deduped.dedup();
            deduped.len()
        };
        assert_eq!(unique_ip_count, 16, "duplicate IPs handed out: {ips:?}");
        assert_eq!(unique_mac_count, 16, "duplicate MACs handed out: {macs:?}");
    }

    #[test]
    fn lease_persists_across_stop_start_and_frees_only_after_release() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("firecrab.db")).unwrap();
        let vm_id = Uuid::new_v4();

        let lease = store.allocate_lease(vm_id).unwrap();
        // Simulate stop/start: nothing in the lifecycle touches the lease.
        assert_eq!(
            store.allocate_lease(vm_id).unwrap_err().to_string(),
            IpamError::AlreadyLeased { vm_id }.to_string()
        );

        store.release_lease(vm_id).unwrap();
        let other_vm = Uuid::new_v4();
        let reallocated = store.allocate_lease(other_vm).unwrap();
        assert_eq!(reallocated.ipv4, lease.ipv4, "freed address should be reusable");
    }
}

//! SQLite-backed VM record storage: schema creation/migration, CRUD, IPAM
//! lease delegation, and one-time import of the legacy `vms.json` format.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use firecrab_api_types::{Ipv6AddressMode, MicroNetworkResponse};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use thiserror::Error;
use uuid::Uuid;

use crate::ipam::{self, IpamError, SubnetSpec};
use crate::model::{Lease, VmRecord, VmState};

/// Default SQLite database path, relative to the process's working directory.
const DB_FILE: &str = "data/firecrab.db";
/// File name of the legacy JSON store, imported once on first open.
const LEGACY_FILE_NAME: &str = "vms.json";

/// Schema for the `vms` table.
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
    ram INTEGER NOT NULL,
    disk_gb INTEGER NOT NULL DEFAULT 2,
    egress_policy TEXT NOT NULL DEFAULT 'internet',
    micro_network_id TEXT,
    storage_root TEXT NOT NULL DEFAULT 'default',
    disk_generation TEXT,
    last_runtime_id TEXT,
    purpose TEXT NOT NULL DEFAULT 'instance',
    env TEXT NOT NULL DEFAULT '{}'
) STRICT";

/// Selects every column [`Store::load_all`] needs.
const SELECT_ALL_SQL: &str = "SELECT id, name, state, template, template_version, \
    template_kernel_sha256, template_rootfs_sha256, template_boot_args_sha256, cpu, ram, disk_gb, \
    egress_policy, micro_network_id, storage_root, disk_generation, last_runtime_id, purpose, env FROM vms";

/// Inserts a new row; fails on a duplicate id.
const INSERT_SQL: &str = "INSERT INTO vms (id, name, state, template, template_version, \
    template_kernel_sha256, template_rootfs_sha256, template_boot_args_sha256, cpu, ram, disk_gb, \
    egress_policy, micro_network_id, storage_root, disk_generation, last_runtime_id, purpose, env) \
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)";

/// Upserts a row, used only by the one-time legacy `vms.json` import.
const IMPORT_SQL: &str = "INSERT OR REPLACE INTO vms (id, name, state, template, \
    template_version, template_kernel_sha256, template_rootfs_sha256, \
    template_boot_args_sha256, cpu, ram, disk_gb, egress_policy, micro_network_id, storage_root, \
    disk_generation, last_runtime_id, purpose, env) \
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)";

/// Replaces an existing row's columns by id.
const UPDATE_SQL: &str = "UPDATE vms SET name = ?2, state = ?3, template = ?4, \
    template_version = ?5, template_kernel_sha256 = ?6, template_rootfs_sha256 = ?7, \
    template_boot_args_sha256 = ?8, cpu = ?9, ram = ?10, disk_gb = ?11, egress_policy = ?12, \
    micro_network_id = ?13, storage_root = ?14, disk_generation = ?15, last_runtime_id = ?16, \
    purpose = ?17, env = ?18 WHERE id = ?1";

/// Schema for the `micro_networks` table (`public-docs/networking.md`).
/// The gateway isn't stored — it's derived from `subnet_cidr` — and neither
/// is the bridge name, which the helper derives from `id`.
const CREATE_MICRO_NETWORKS_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS micro_networks (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    subnet_cidr TEXT NOT NULL,
    internet_enabled INTEGER NOT NULL DEFAULT 1,
    uplink TEXT,
    ipv6_cidr TEXT,
    ipv6_address_mode TEXT
) STRICT";

/// Selects every column [`Store::list_micro_networks`] needs.
const SELECT_ALL_MICRO_NETWORKS_SQL: &str = "SELECT id, name, subnet_cidr, internet_enabled, \
    uplink, ipv6_cidr, ipv6_address_mode FROM micro_networks";

/// Inserts a new row; fails on a duplicate id.
const INSERT_MICRO_NETWORK_SQL: &str = "INSERT INTO micro_networks \
    (id, name, subnet_cidr, internet_enabled, uplink, ipv6_cidr, ipv6_address_mode) \
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";

/// MicroStorage pools (`public-docs/storage.md`).
const CREATE_MICRO_STORAGES_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS micro_storages (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    path TEXT NOT NULL UNIQUE
) STRICT";

const SELECT_ALL_MICRO_STORAGES_SQL: &str = "SELECT id, name, path FROM micro_storages";

const INSERT_MICRO_STORAGE_SQL: &str =
    "INSERT INTO micro_storages (id, name, path) VALUES (?1, ?2, ?3)";

/// Shell repository catalog (`feat/shell-repository` / issue #60).
const CREATE_SHELLS_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS shells (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
) STRICT";

/// Host-local MicroRegistry rows from a successful register job.
/// Uniqueness is the composite PK so a concurrent double-register cannot
/// insert two rows for the same alias and architecture.
const CREATE_MICROREGISTRY_LOCAL_TABLE_SQL: &str =
    "CREATE TABLE IF NOT EXISTS microregistry_local (
    alias TEXT NOT NULL,
    architecture TEXT NOT NULL,
    version TEXT NOT NULL,
    package TEXT NOT NULL DEFAULT '',
    sha256 TEXT NOT NULL DEFAULT '',
    min_disk_gb INTEGER NOT NULL,
    published_at TEXT NOT NULL,
    PRIMARY KEY (alias, architecture)
) STRICT";

const SELECT_MICROREGISTRY_LOCAL_ALL_SQL: &str = "SELECT alias, architecture, version, package, \
    sha256, min_disk_gb, published_at FROM microregistry_local ORDER BY alias";

const SELECT_MICROREGISTRY_LOCAL_BY_ARCH_SQL: &str = "SELECT alias, architecture, version, package, \
    sha256, min_disk_gb, published_at FROM microregistry_local WHERE architecture = ?1 ORDER BY alias";

const SELECT_MICROREGISTRY_LOCAL_ONE_SQL: &str = "SELECT alias, architecture, version, package, \
    sha256, min_disk_gb, published_at FROM microregistry_local WHERE alias = ?1 AND architecture = ?2";

const CREATE_DOCKER_HUB_CREDENTIAL_TABLE_SQL: &str =
    "CREATE TABLE IF NOT EXISTS docker_hub_credential (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    username TEXT NOT NULL,
    secret TEXT NOT NULL
) STRICT";

const SELECT_DOCKER_HUB_CREDENTIAL_SQL: &str =
    "SELECT username, secret FROM docker_hub_credential WHERE id = 1";

const UPSERT_DOCKER_HUB_CREDENTIAL_SQL: &str =
    "INSERT INTO docker_hub_credential (id, username, secret) VALUES (1, ?1, ?2)
     ON CONFLICT(id) DO UPDATE SET username = excluded.username, secret = excluded.secret";

const DELETE_DOCKER_HUB_CREDENTIAL_SQL: &str = "DELETE FROM docker_hub_credential WHERE id = 1";

/// Username and access token for authenticated Docker Hub pulls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerHubCredential {
    /// Docker Hub account name.
    pub username: String,
    /// Password or personal access token. Never returned on the wire.
    pub secret: String,
}

const INSERT_MICROREGISTRY_LOCAL_SQL: &str = "INSERT INTO microregistry_local \
    (alias, architecture, version, package, sha256, min_disk_gb, published_at) \
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";

const CREATE_SHELL_REVISIONS_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS shell_revisions (
    id TEXT PRIMARY KEY,
    shell_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    content TEXT NOT NULL,
    content_sha256 TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    UNIQUE (shell_id, version)
) STRICT";

const CREATE_VM_SHELLS_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS vm_shells (
    vm_id TEXT NOT NULL,
    shell_id TEXT NOT NULL,
    revision_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    PRIMARY KEY (vm_id, shell_id)
) STRICT";

const CREATE_PORT_FORWARDS_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS port_forwards (
    vm_id TEXT NOT NULL,
    host_port INTEGER NOT NULL,
    guest_port INTEGER NOT NULL,
    protocol TEXT NOT NULL DEFAULT 'tcp',
    PRIMARY KEY (vm_id, host_port, protocol)
) STRICT";

/// A host port can only ever forward to one VM at a time: the primary key
/// above only rules out the same VM claiming a port twice, so without this a
/// second VM could still be given a `host_port`/`protocol` already owned by
/// another. Handlers pre-check this too (`list_all_port_forwards`), but that
/// check-then-insert is racy under concurrent requests; this index is what
/// actually makes the conflict impossible rather than just unlikely.
const CREATE_PORT_FORWARDS_UNIQUE_HOST_PORT_SQL: &str = "CREATE UNIQUE INDEX IF NOT EXISTS port_forwards_host_port_protocol \
     ON port_forwards(host_port, protocol)";

/// Adds `disk_gb` to a `vms` table created before the column existed (a
/// bare `CREATE TABLE IF NOT EXISTS` doesn't retrofit new columns onto an
/// already-created table). `2` matches the fixed rootfs template size that
/// applied before disk capacity became configurable.
fn migrate_disk_gb_column(conn: &Connection) -> Result<(), PersistenceError> {
    let has_column: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('vms') WHERE name = 'disk_gb'")?
        .exists([])?;
    if !has_column {
        conn.execute(
            "ALTER TABLE vms ADD COLUMN disk_gb INTEGER NOT NULL DEFAULT 2",
            [],
        )?;
    }
    Ok(())
}

/// Adds `egress_policy` to a `vms` table created before the column existed,
/// same reasoning as [`migrate_disk_gb_column`]. `'internet'` matches the
/// behavior every VM had before this field existed (`setup_vm_network`
/// always applied `EgressPolicy::default()`).
fn migrate_egress_policy_column(conn: &Connection) -> Result<(), PersistenceError> {
    let has_column: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('vms') WHERE name = 'egress_policy'")?
        .exists([])?;
    if !has_column {
        conn.execute(
            "ALTER TABLE vms ADD COLUMN egress_policy TEXT NOT NULL DEFAULT 'internet'",
            [],
        )?;
    }
    Ok(())
}

/// Adds `micro_network_id` to a `vms`/`network_leases` pair created before
/// MicroNetwork membership existed. NULL means the default network — exactly
/// what every pre-existing VM and lease was on — so nothing needs
/// backfilling. `network_leases` is created after this runs on a fresh DB,
/// hence the table-exists check.
fn migrate_micro_network_columns(conn: &Connection) -> Result<(), PersistenceError> {
    for (table, sql) in [
        ("vms", "ALTER TABLE vms ADD COLUMN micro_network_id TEXT"),
        ("network_leases", ipam::ADD_LEASE_MICRO_NETWORK_COLUMN_SQL),
    ] {
        let table_exists: bool = conn
            .prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")?
            .exists([table])?;
        let has_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info(?1) WHERE name = 'micro_network_id'")?
            .exists([table])?;
        if table_exists && !has_column {
            conn.execute(sql, [])?;
        }
    }
    Ok(())
}

/// Adds `storage_root` so VMs created before multi-disk selection keep the
/// legacy `default` root (`data/vms/…`).
fn migrate_storage_root_column(conn: &Connection) -> Result<(), PersistenceError> {
    let has_column: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('vms') WHERE name = 'storage_root'")?
        .exists([])?;
    if !has_column {
        conn.execute(
            "ALTER TABLE vms ADD COLUMN storage_root TEXT NOT NULL DEFAULT 'default'",
            [],
        )?;
    }
    Ok(())
}

/// Disk generation + last runtime ids for the artifact layout
/// (`public-docs/storage.md`).
fn migrate_disk_generation_columns(conn: &Connection) -> Result<(), PersistenceError> {
    for (name, sql) in [
        (
            "disk_generation",
            "ALTER TABLE vms ADD COLUMN disk_generation TEXT",
        ),
        (
            "last_runtime_id",
            "ALTER TABLE vms ADD COLUMN last_runtime_id TEXT",
        ),
    ] {
        let has_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('vms') WHERE name = ?1")?
            .exists([name])?;
        if !has_column {
            conn.execute(sql, [])?;
        }
    }
    Ok(())
}

const ADD_ENV_COLUMN_SQL: &str = "ALTER TABLE vms ADD COLUMN env TEXT NOT NULL DEFAULT '{}'";

/// Adds `env` to a `vms` table created before per-VM environment existed.
/// `'{}'` matches the empty map every VM had before this field existed.
fn migrate_env_column(conn: &Connection) -> Result<(), PersistenceError> {
    let has_column: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('vms') WHERE name = 'env'")?
        .exists([])?;
    if !has_column {
        conn.execute(ADD_ENV_COLUMN_SQL, [])?;
    }
    Ok(())
}

/// Adds `purpose` to a `vms` table created before it existed. `'instance'`
/// matches the only kind of VM that could exist before builder VMs did.
fn migrate_purpose_column(conn: &Connection) -> Result<(), PersistenceError> {
    let has_column: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('vms') WHERE name = 'purpose'")?
        .exists([])?;
    if !has_column {
        conn.execute(
            "ALTER TABLE vms ADD COLUMN purpose TEXT NOT NULL DEFAULT 'instance'",
            [],
        )?;
    }
    Ok(())
}

/// Adds `internet_enabled` to a `micro_networks` table created before the
/// per-network internet toggle existed, same reasoning as
/// [`migrate_disk_gb_column`]. `1` matches the behavior every network had
/// then: all of them were masqueraded out of the host's uplink.
fn migrate_internet_enabled_column(conn: &Connection) -> Result<(), PersistenceError> {
    let has_column: bool = conn
        .prepare(
            "SELECT 1 FROM pragma_table_info('micro_networks') WHERE name = 'internet_enabled'",
        )?
        .exists([])?;
    if !has_column {
        conn.execute(
            "ALTER TABLE micro_networks ADD COLUMN internet_enabled INTEGER NOT NULL DEFAULT 1",
            [],
        )?;
    }
    Ok(())
}

/// Adds `uplink` to a `micro_networks` table created before per-network
/// egress NIC selection existed. `NULL` keeps the host default-route iface.
fn migrate_uplink_column(conn: &Connection) -> Result<(), PersistenceError> {
    let has_column: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('micro_networks') WHERE name = 'uplink'")?
        .exists([])?;
    if !has_column {
        conn.execute("ALTER TABLE micro_networks ADD COLUMN uplink TEXT", [])?;
    }
    Ok(())
}

/// Adds the IPv6 columns to a `micro_networks` table created before
/// dual-stack existed. `NULL` keeps such a network IPv4-only: its VMs were
/// given addresses out of one family only, and inventing a prefix under a
/// running network would hand them a second one nothing has pinned.
fn migrate_ipv6_columns(conn: &Connection) -> Result<(), PersistenceError> {
    for (column, sql) in [
        (
            "ipv6_cidr",
            "ALTER TABLE micro_networks ADD COLUMN ipv6_cidr TEXT",
        ),
        (
            "ipv6_address_mode",
            "ALTER TABLE micro_networks ADD COLUMN ipv6_address_mode TEXT",
        ),
    ] {
        let has_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('micro_networks') WHERE name = ?1")?
            .exists(params![column])?;
        if !has_column {
            conn.execute(sql, [])?;
        }
    }
    Ok(())
}

/// Adds `ipv6` to a `network_leases` table created before dual-stack, the
/// lease-side counterpart of [`migrate_ipv6_columns`].
fn migrate_lease_ipv6_column(conn: &Connection) -> Result<(), PersistenceError> {
    let has_column: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('network_leases') WHERE name = 'ipv6'")?
        .exists([])?;
    if !has_column {
        conn.execute(ipam::ADD_LEASE_IPV6_COLUMN_SQL, [])?;
    }
    Ok(())
}

/// One-shot upgrade: VMs/leases created when the default network was
/// implicit (`micro_network_id` NULL) get an explicit MicroNetwork row
/// (`name=default`, `172.30.0.0/24`) and are reattached to it.
///
/// Fresh installs have no NULL rows and create no seed network — operators
/// must POST a MicroNetwork before creating VMs.
fn promote_implicit_default_network(conn: &Connection) -> Result<(), PersistenceError> {
    let null_vms: i64 = conn.query_row(
        "SELECT COUNT(*) FROM vms WHERE micro_network_id IS NULL OR micro_network_id = ''",
        [],
        |row| row.get(0),
    )?;
    let null_leases: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM network_leases \
             WHERE released_at IS NULL \
               AND (micro_network_id IS NULL OR micro_network_id = '')",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if null_vms == 0 && null_leases == 0 {
        return Ok(());
    }

    let id = Uuid::new_v4();
    let cidr = format!(
        "{}/{}",
        crate::ipam::LEGACY_DEFAULT_NETWORK,
        crate::ipam::LEGACY_DEFAULT_PREFIX
    );
    // Promoted networks stay IPv4-only: the VMs being reattached were
    // addressed out of one family, and inventing a prefix under them would
    // hand out a second address nothing has pinned.
    conn.execute(
        INSERT_MICRO_NETWORK_SQL,
        params![
            id.to_string(),
            "default",
            cidr,
            1,
            Option::<String>::None,
            Option::<String>::None,
            Option::<String>::None
        ],
    )?;
    conn.execute(
        "UPDATE vms SET micro_network_id = ?1 \
         WHERE micro_network_id IS NULL OR micro_network_id = ''",
        params![id.to_string()],
    )?;
    let _ = conn.execute(
        "UPDATE network_leases SET micro_network_id = ?1 \
         WHERE micro_network_id IS NULL OR micro_network_id = ''",
        params![id.to_string()],
    );
    tracing::info!(
        micro_network_id = %id,
        null_vms,
        null_leases,
        "promoted implicit default network to explicit MicroNetwork"
    );
    Ok(())
}

/// Failure modes for opening or operating on the VM [`Store`].
#[derive(Debug, Error)]
pub enum PersistenceError {
    /// Couldn't create the database's parent directory.
    #[error("failed to create VM data directory {path}: {source}")]
    CreateDirectory {
        /// The directory that couldn't be created.
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Couldn't open the SQLite database file.
    #[error("failed to open VM database {path}: {source}")]
    Open {
        /// The database path that couldn't be opened.
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    /// A SQLite query/statement failed.
    #[error("VM database operation failed: {0}")]
    Database(#[from] rusqlite::Error),
    /// A stored row's data doesn't match what the application expects.
    #[error("VM database record {id} is invalid: {reason}")]
    CorruptRecord {
        /// The invalid row's id (as stored, not necessarily a valid UUID).
        id: String,
        /// Human-readable reason the row is invalid.
        reason: String,
    },
    /// An operation targeted a VM id with no matching row.
    #[error("VM {id} does not exist in the database")]
    MissingVm {
        /// The id that wasn't found.
        id: Uuid,
    },
    /// An operation targeted a MicroNetwork id with no matching row.
    #[error("MicroNetwork {id} does not exist in the database")]
    MissingMicroNetwork {
        /// The id that wasn't found.
        id: Uuid,
    },
    /// An operation targeted a MicroStorage id with no matching row.
    #[error("MicroStorage {id} does not exist in the database")]
    MissingMicroStorage {
        /// The id that wasn't found.
        id: Uuid,
    },
    /// A MicroStorage path is already registered.
    #[error("MicroStorage path {path} is already registered")]
    DuplicateMicroStoragePath {
        /// The conflicting path.
        path: String,
    },
    /// A local MicroRegistry alias is already registered for this architecture.
    #[error("MicroRegistry local alias {alias} is already registered for {architecture}")]
    DuplicateMicroRegistryLocal {
        /// Conflicting catalog alias.
        alias: String,
        /// Catalog architecture label (`x86_64` or `aarch64`).
        architecture: String,
    },
    /// A host port/protocol is already forwarded to another VM.
    #[error("host port {host_port}/{protocol} is already in use by another VM")]
    DuplicatePortForward {
        /// The conflicting host port.
        host_port: u16,
        /// The conflicting protocol ("tcp" or "udp").
        protocol: String,
    },
    /// An operation targeted a Shell id with no matching row.
    #[error("Shell {id} does not exist in the database")]
    MissingShell {
        /// The id that wasn't found.
        id: Uuid,
    },
    /// Shell still pinned on one or more VMs.
    #[error("Shell {id} is still pinned on {count} VM(s)")]
    ShellInUse {
        /// Shell id.
        id: Uuid,
        /// How many VMs still pin it.
        count: u32,
    },
    /// Couldn't read the legacy `vms.json` file.
    #[error("failed to read legacy VM data from {path}: {source}")]
    LegacyRead {
        /// The legacy file path that couldn't be read.
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The legacy `vms.json` file's content isn't valid for the expected shape.
    #[error("failed to deserialize legacy VM data from {path}: {source}")]
    LegacyDeserialize {
        /// The legacy file path that failed to parse.
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// Couldn't rename the legacy file after a successful import.
    #[error("failed to archive imported legacy VM data {path}: {source}")]
    LegacyArchive {
        /// The legacy file path that couldn't be renamed.
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// The default SQLite database path (`data/firecrab.db`).
pub fn default_db_file() -> PathBuf {
    PathBuf::from(DB_FILE)
}

/// One locally registered MicroRegistry row (`microregistry_local`).
/// Written only by a successful register job; listing never invents these
/// from installed aliases or disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalCatalogEntry {
    pub alias: String,
    pub architecture: String,
    pub version: String,
    pub package: String,
    pub sha256: String,
    pub min_disk_gb: u16,
    pub published_at: String,
}

/// Handle to the VM records SQLite database. Cheaply `Clone`able; all
/// clones share one connection behind a mutex.
#[derive(Debug, Clone)]
pub struct Store {
    /// The shared, mutex-guarded SQLite connection.
    conn: Arc<Mutex<Connection>>,
}

/// Restricts the database file to its owner.
///
/// A failure is logged rather than returned: a host whose filesystem cannot
/// carry the mode still has a working store, and the operator can see the
/// warning and decide.
fn restrict_to_owner(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    if let Err(error) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
        tracing::warn!(
            path = %path.display(),
            %error,
            "could not restrict the database file to its owner"
        );
    }
}

impl Store {
    /// Opens (creating if needed) the database at `path`: sets WAL mode,
    /// creates/migrates the schema, and imports any legacy `vms.json` found
    /// alongside it.
    pub fn open(path: &Path) -> Result<Self, PersistenceError> {
        if let Some(directory) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(directory).map_err(|source| PersistenceError::CreateDirectory {
                path: directory.to_owned(),
                source,
            })?;
        }

        let conn = Connection::open(path).map_err(|source| PersistenceError::Open {
            path: path.to_owned(),
            source,
        })?;
        // The database holds the operator's registry token, so it is
        // owner-only regardless of the service umask. Doing it before the
        // first write gives the `-wal` and `-shm` files the same mode.
        restrict_to_owner(path);
        let _mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute(CREATE_TABLE_SQL, [])?;
        migrate_disk_gb_column(&conn)?;
        migrate_egress_policy_column(&conn)?;
        migrate_micro_network_columns(&conn)?;
        migrate_storage_root_column(&conn)?;
        migrate_disk_generation_columns(&conn)?;
        migrate_purpose_column(&conn)?;
        migrate_env_column(&conn)?;
        conn.execute(ipam::CREATE_LEASES_TABLE_SQL, [])?;
        migrate_lease_ipv6_column(&conn)?;
        for index_sql in ipam::CREATE_LEASES_INDEXES_SQL {
            conn.execute(index_sql, [])?;
        }
        conn.execute(CREATE_MICRO_NETWORKS_TABLE_SQL, [])?;
        // After the CREATE, unlike the `vms` migrations above: the table this
        // one alters is the one just created, and `CREATE TABLE IF NOT EXISTS`
        // leaves an older table's columns as they were.
        migrate_internet_enabled_column(&conn)?;
        migrate_uplink_column(&conn)?;
        migrate_ipv6_columns(&conn)?;
        conn.execute(CREATE_MICRO_STORAGES_TABLE_SQL, [])?;
        conn.execute(CREATE_SHELLS_TABLE_SQL, [])?;
        conn.execute(CREATE_MICROREGISTRY_LOCAL_TABLE_SQL, [])?;
        conn.execute(CREATE_DOCKER_HUB_CREDENTIAL_TABLE_SQL, [])?;
        conn.execute(CREATE_SHELL_REVISIONS_TABLE_SQL, [])?;
        conn.execute(CREATE_VM_SHELLS_TABLE_SQL, [])?;
        conn.execute(CREATE_PORT_FORWARDS_TABLE_SQL, [])?;
        conn.execute(CREATE_PORT_FORWARDS_UNIQUE_HOST_PORT_SQL, [])?;
        // After micro_networks exists: promote pre-MicroNetwork VMs/leases
        // that still have NULL micro_network_id onto one explicit row.
        promote_implicit_default_network(&conn)?;

        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.import_legacy(&path.with_file_name(LEGACY_FILE_NAME))?;
        Ok(store)
    }

    /// Loads every VM record currently in the database.
    pub fn load_all(&self) -> Result<HashMap<Uuid, VmRecord>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare(SELECT_ALL_SQL)?;
        let mut rows = statement.query([])?;
        let mut vms = HashMap::new();
        while let Some(row) = rows.next()? {
            let id_text: String = row.get(0)?;
            let id = Uuid::parse_str(&id_text).map_err(|_| PersistenceError::CorruptRecord {
                id: id_text.clone(),
                reason: "id is not a UUID".to_owned(),
            })?;
            let state_text: String = row.get(2)?;
            vms.insert(
                id,
                VmRecord {
                    id,
                    name: row.get(1)?,
                    purpose: decode_purpose(&id_text, &row.get::<_, String>(16)?)?,
                    state: decode_state(&id_text, &state_text)?,
                    template: row.get(3)?,
                    template_version: row.get(4)?,
                    template_kernel_sha256: row.get(5)?,
                    template_rootfs_sha256: row.get(6)?,
                    template_boot_args_sha256: row.get(7)?,
                    cpu: row.get(8)?,
                    ram: row.get(9)?,
                    disk_gb: row.get(10)?,
                    egress_policy: decode_egress_policy(&id_text, &row.get::<_, String>(11)?)?,
                    micro_network_id: decode_required_id(
                        &id_text,
                        row.get(12)?,
                        "micro_network_id",
                    )?,
                    storage_root: row.get(13)?,
                    disk_generation: decode_optional_id(&id_text, row.get(14)?)?,
                    last_runtime_id: decode_optional_id(&id_text, row.get(15)?)?,
                    startup_step: None,
                    startup_timeline: Vec::new(),
                    env: decode_env(&id_text, &row.get::<_, String>(17)?)?,
                },
            );
        }
        Ok(vms)
    }

    /// Inserts a new VM record.
    pub fn insert(&self, vm: &VmRecord) -> Result<(), PersistenceError> {
        execute_record(&self.lock(), INSERT_SQL, vm)?;
        Ok(())
    }

    /// Replaces an existing VM record's columns.
    pub fn update(&self, vm: &VmRecord) -> Result<(), PersistenceError> {
        if execute_record(&self.lock(), UPDATE_SQL, vm)? == 0 {
            return Err(PersistenceError::MissingVm { id: vm.id });
        }
        Ok(())
    }

    /// Deletes a VM record by id.
    pub fn delete(&self, id: Uuid) -> Result<(), PersistenceError> {
        let changed = self
            .lock()
            .execute("DELETE FROM vms WHERE id = ?1", params![id.to_string()])?;
        if changed == 0 {
            return Err(PersistenceError::MissingVm { id });
        }
        Ok(())
    }

    /// Inserts a new MicroNetwork.
    pub fn insert_micro_network(
        &self,
        network: &MicroNetworkResponse,
    ) -> Result<(), PersistenceError> {
        self.lock().execute(
            INSERT_MICRO_NETWORK_SQL,
            params![
                network.id.to_string(),
                network.name,
                network.subnet_cidr,
                network.internet_enabled,
                network.uplink,
                network.ipv6_cidr,
                network.ipv6_address_mode.map(|mode| mode.id().to_owned()),
            ],
        )?;
        Ok(())
    }

    /// Flips one MicroNetwork's internet access. CIDR stays immutable —
    /// its VMs' addresses came out of it.
    pub fn set_micro_network_internet(
        &self,
        id: Uuid,
        internet_enabled: bool,
    ) -> Result<(), PersistenceError> {
        let changed = self.lock().execute(
            "UPDATE micro_networks SET internet_enabled = ?2 WHERE id = ?1",
            params![id.to_string(), internet_enabled],
        )?;
        if changed == 0 {
            return Err(PersistenceError::MissingMicroNetwork { id });
        }
        Ok(())
    }

    /// Sets one MicroNetwork's stored uplink. `None` means auto (the host
    /// default-route iface).
    pub fn set_micro_network_uplink(
        &self,
        id: Uuid,
        uplink: Option<String>,
    ) -> Result<(), PersistenceError> {
        let changed = self.lock().execute(
            "UPDATE micro_networks SET uplink = ?2 WHERE id = ?1",
            params![id.to_string(), uplink],
        )?;
        if changed == 0 {
            return Err(PersistenceError::MissingMicroNetwork { id });
        }
        Ok(())
    }

    /// Lists every MicroNetwork.
    pub fn list_micro_networks(&self) -> Result<Vec<MicroNetworkResponse>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare(SELECT_ALL_MICRO_NETWORKS_SQL)?;
        let mut rows = statement.query([])?;
        let mut networks = Vec::new();
        while let Some(row) = rows.next()? {
            let id_text: String = row.get(0)?;
            let id = Uuid::parse_str(&id_text).map_err(|_| PersistenceError::CorruptRecord {
                id: id_text.clone(),
                reason: "id is not a UUID".to_owned(),
            })?;
            let subnet_cidr: String = row.get(2)?;
            // Derived rather than stored, so the gateway can never drift out
            // of sync with the CIDR it belongs to.
            let gateway = SubnetSpec::parse(id, &subnet_cidr)
                .ok_or_else(|| PersistenceError::CorruptRecord {
                    id: id_text.clone(),
                    reason: format!("subnet_cidr {subnet_cidr:?} does not parse"),
                })?
                .gateway()
                .to_string();
            let ipv6_cidr: Option<String> = row.get(5)?;
            let ipv6_address_mode = ipv6_cidr.as_ref().map(|_| {
                match row.get::<_, Option<String>>(6).ok().flatten().as_deref() {
                    Some("dhcpv6") => Ipv6AddressMode::Dhcpv6,
                    _ => Ipv6AddressMode::Slaac,
                }
            });
            // Both derived from the stored prefix, never stored themselves:
            // the gateway the same way the v4 one is, and the egress mode
            // from the prefix's own scope (ULA -> NAT66, global -> direct).
            let ipv6 = ipv6_cidr.as_deref().and_then(|cidr| {
                SubnetSpec::parse_ipv6(cidr, ipam::protocol_address_mode(ipv6_address_mode))
            });
            networks.push(MicroNetworkResponse {
                id,
                name: row.get(1)?,
                subnet_cidr,
                gateway,
                internet_enabled: row.get(3)?,
                uplink: row.get(4)?,
                ipv6_cidr,
                ipv6_gateway: ipv6.map(|ipv6| ipv6.gateway.to_string()),
                ipv6_address_mode,
                ipv6_egress: ipv6.as_ref().map(ipam::ipv6_egress_mode),
            });
        }
        Ok(networks)
    }

    /// Deletes a MicroNetwork by id.
    pub fn delete_micro_network(&self, id: Uuid) -> Result<(), PersistenceError> {
        let changed = self.lock().execute(
            "DELETE FROM micro_networks WHERE id = ?1",
            params![id.to_string()],
        )?;
        if changed == 0 {
            return Err(PersistenceError::MissingMicroNetwork { id });
        }
        Ok(())
    }

    /// Lists every MicroStorage (path only — free space is filled by the handler).
    pub fn list_micro_storages(&self) -> Result<Vec<(Uuid, String, String)>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare(SELECT_ALL_MICRO_STORAGES_SQL)?;
        let mut rows = statement.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let id_text: String = row.get(0)?;
            let id = Uuid::parse_str(&id_text).map_err(|_| PersistenceError::CorruptRecord {
                id: id_text.clone(),
                reason: "id is not a UUID".to_owned(),
            })?;
            out.push((id, row.get(1)?, row.get(2)?));
        }
        Ok(out)
    }

    /// One MicroStorage by id.
    pub fn micro_storage(
        &self,
        id: Uuid,
    ) -> Result<Option<(Uuid, String, String)>, PersistenceError> {
        let conn = self.lock();
        let mut statement =
            conn.prepare("SELECT id, name, path FROM micro_storages WHERE id = ?1")?;
        let mut rows = statement.query(params![id.to_string()])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let id_text: String = row.get(0)?;
        let parsed = Uuid::parse_str(&id_text).map_err(|_| PersistenceError::CorruptRecord {
            id: id_text.clone(),
            reason: "id is not a UUID".to_owned(),
        })?;
        Ok(Some((parsed, row.get(1)?, row.get(2)?)))
    }

    /// Inserts a new MicroStorage.
    pub fn insert_micro_storage(
        &self,
        id: Uuid,
        name: &str,
        path: &str,
    ) -> Result<(), PersistenceError> {
        let result = self.lock().execute(
            INSERT_MICRO_STORAGE_SQL,
            params![id.to_string(), name, path],
        );
        match result {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(PersistenceError::DuplicateMicroStoragePath {
                    path: path.to_owned(),
                })
            }
            Err(error) => Err(PersistenceError::Database(error)),
        }
    }

    /// Deletes a MicroStorage by id.
    pub fn delete_micro_storage(&self, id: Uuid) -> Result<(), PersistenceError> {
        let changed = self.lock().execute(
            "DELETE FROM micro_storages WHERE id = ?1",
            params![id.to_string()],
        )?;
        if changed == 0 {
            return Err(PersistenceError::MissingMicroStorage { id });
        }
        Ok(())
    }

    /// Docker Hub login used by OCI inspect/import, if the operator saved one.
    pub fn docker_hub_credential(&self) -> Result<Option<DockerHubCredential>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare(SELECT_DOCKER_HUB_CREDENTIAL_SQL)?;
        statement
            .query_row([], |row| {
                Ok(DockerHubCredential {
                    username: row.get(0)?,
                    secret: row.get(1)?,
                })
            })
            .optional()
            .map_err(PersistenceError::from)
    }

    /// Replaces the stored Docker Hub login. The secret is write-only.
    pub fn put_docker_hub_credential(
        &self,
        username: &str,
        secret: &str,
    ) -> Result<(), PersistenceError> {
        self.lock()
            .execute(UPSERT_DOCKER_HUB_CREDENTIAL_SQL, params![username, secret])?;
        Ok(())
    }

    /// Forgets the stored Docker Hub login.
    pub fn delete_docker_hub_credential(&self) -> Result<(), PersistenceError> {
        self.lock().execute(DELETE_DOCKER_HUB_CREDENTIAL_SQL, [])?;
        Ok(())
    }

    /// Local MicroRegistry rows, optionally filtered by catalog architecture.
    pub fn list_microregistry_local(
        &self,
        architecture: Option<&str>,
    ) -> Result<Vec<LocalCatalogEntry>, PersistenceError> {
        let conn = self.lock();
        let mut statement = match architecture {
            Some(_) => conn.prepare(SELECT_MICROREGISTRY_LOCAL_BY_ARCH_SQL)?,
            None => conn.prepare(SELECT_MICROREGISTRY_LOCAL_ALL_SQL)?,
        };
        let mut rows = match architecture {
            Some(architecture) => statement.query(params![architecture])?,
            None => statement.query([])?,
        };
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(map_local_catalog_entry(row)?);
        }
        Ok(out)
    }

    /// One local MicroRegistry row by alias and architecture.
    pub fn microregistry_local(
        &self,
        alias: &str,
        architecture: &str,
    ) -> Result<Option<LocalCatalogEntry>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare(SELECT_MICROREGISTRY_LOCAL_ONE_SQL)?;
        Ok(statement
            .query_row(params![alias, architecture], map_local_catalog_entry)
            .optional()?)
    }

    /// Inserts a local MicroRegistry row. A composite-PK clash is
    /// [`PersistenceError::DuplicateMicroRegistryLocal`].
    pub fn insert_microregistry_local(
        &self,
        entry: &LocalCatalogEntry,
    ) -> Result<(), PersistenceError> {
        let result = self.lock().execute(
            INSERT_MICROREGISTRY_LOCAL_SQL,
            params![
                entry.alias,
                entry.architecture,
                entry.version,
                entry.package,
                entry.sha256,
                i64::from(entry.min_disk_gb),
                entry.published_at,
            ],
        );
        match result {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(PersistenceError::DuplicateMicroRegistryLocal {
                    alias: entry.alias.clone(),
                    architecture: entry.architecture.clone(),
                })
            }
            Err(error) => Err(PersistenceError::Database(error)),
        }
    }

    /// How many VMs still point at `storage_root` (id string).
    pub fn count_vms_with_storage_root(&self, storage_root: &str) -> Result<u32, PersistenceError> {
        let conn = self.lock();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM vms WHERE storage_root = ?1",
            params![storage_root],
            |row| row.get(0),
        )?;
        Ok(count as u32)
    }

    /// Creates a shell and its first revision (version 1).
    pub fn create_shell(
        &self,
        id: Uuid,
        name: &str,
        description: Option<&str>,
        revision_id: Uuid,
        content: &str,
        content_sha256: &str,
        now_ms: u64,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO shells (id, name, description, created_at_ms, updated_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id.to_string(),
                name,
                description,
                now_ms as i64,
                now_ms as i64
            ],
        )?;
        tx.execute(
            "INSERT INTO shell_revisions \
             (id, shell_id, version, content, content_sha256, created_at_ms) \
             VALUES (?1, ?2, 1, ?3, ?4, ?5)",
            params![
                revision_id.to_string(),
                id.to_string(),
                content,
                content_sha256,
                now_ms as i64
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Appends a new immutable revision; returns its version number.
    pub fn add_shell_revision(
        &self,
        shell_id: Uuid,
        revision_id: Uuid,
        content: &str,
        content_sha256: &str,
        now_ms: u64,
    ) -> Result<u32, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx
            .prepare("SELECT 1 FROM shells WHERE id = ?1")?
            .exists(params![shell_id.to_string()])?;
        if !exists {
            return Err(PersistenceError::MissingShell { id: shell_id });
        }
        let next: i64 = tx.query_row(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM shell_revisions WHERE shell_id = ?1",
            params![shell_id.to_string()],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO shell_revisions \
             (id, shell_id, version, content, content_sha256, created_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                revision_id.to_string(),
                shell_id.to_string(),
                next,
                content,
                content_sha256,
                now_ms as i64
            ],
        )?;
        tx.execute(
            "UPDATE shells SET updated_at_ms = ?2 WHERE id = ?1",
            params![shell_id.to_string(), now_ms as i64],
        )?;
        tx.commit()?;
        Ok(next as u32)
    }

    /// Lists every shell with latest revision summary.
    pub fn list_shells(&self) -> Result<Vec<firecrab_api_types::ShellResponse>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT s.id, s.name, s.description, s.created_at_ms, s.updated_at_ms, \
                    r.id, r.version, r.content_sha256 \
             FROM shells s \
             LEFT JOIN shell_revisions r ON r.shell_id = s.id \
               AND r.version = (SELECT MAX(version) FROM shell_revisions WHERE shell_id = s.id) \
             ORDER BY s.name COLLATE NOCASE, s.id",
        )?;
        let mut rows = statement.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let id_text: String = row.get(0)?;
            let id = Uuid::parse_str(&id_text).map_err(|_| PersistenceError::CorruptRecord {
                id: id_text.clone(),
                reason: "shell id is not a UUID".to_owned(),
            })?;
            let rev_id: Option<String> = row.get(5)?;
            let latest_revision_id =
                rev_id
                    .as_deref()
                    .map(Uuid::parse_str)
                    .transpose()
                    .map_err(|_| PersistenceError::CorruptRecord {
                        id: id_text.clone(),
                        reason: "revision id is not a UUID".to_owned(),
                    })?;
            let version: Option<i64> = row.get(6)?;
            out.push(firecrab_api_types::ShellResponse {
                id,
                name: row.get(1)?,
                description: row.get(2)?,
                latest_version: version.unwrap_or(0) as u32,
                latest_revision_id,
                content_sha256: row.get(7)?,
                created_at_ms: row.get::<_, i64>(3)? as u64,
                updated_at_ms: row.get::<_, i64>(4)? as u64,
            });
        }
        Ok(out)
    }

    /// One immutable revision with full body (must belong to `shell_id`).
    pub fn shell_revision(
        &self,
        shell_id: Uuid,
        revision_id: Uuid,
    ) -> Result<Option<firecrab_api_types::ShellRevisionResponse>, PersistenceError> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT id, version, content_sha256, content, created_at_ms \
                 FROM shell_revisions WHERE id = ?1 AND shell_id = ?2",
                params![revision_id.to_string(), shell_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((rev_text, version, content_sha256, content, created_at_ms)) = row else {
            return Ok(None);
        };
        let parsed_revision_id =
            Uuid::parse_str(&rev_text).map_err(|_| PersistenceError::CorruptRecord {
                id: rev_text.clone(),
                reason: "revision id is not a UUID".to_owned(),
            })?;
        Ok(Some(firecrab_api_types::ShellRevisionResponse {
            shell_id,
            revision_id: parsed_revision_id,
            version: version as u32,
            content_sha256,
            content,
            created_at_ms: created_at_ms as u64,
        }))
    }

    /// Shell detail: metadata + all revisions (newest first) + latest body.
    pub fn shell_detail(
        &self,
        id: Uuid,
    ) -> Result<Option<firecrab_api_types::ShellDetailResponse>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT id, name, description, created_at_ms, updated_at_ms FROM shells WHERE id = ?1",
        )?;
        let mut rows = statement.query(params![id.to_string()])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let id_text: String = row.get(0)?;
        let shell_id = Uuid::parse_str(&id_text).map_err(|_| PersistenceError::CorruptRecord {
            id: id_text.clone(),
            reason: "shell id is not a UUID".to_owned(),
        })?;
        let name: String = row.get(1)?;
        let description: Option<String> = row.get(2)?;
        let created_at_ms = row.get::<_, i64>(3)? as u64;
        let updated_at_ms = row.get::<_, i64>(4)? as u64;

        let mut rev_stmt = conn.prepare(
            "SELECT id, version, content_sha256, created_at_ms, content \
             FROM shell_revisions WHERE shell_id = ?1 ORDER BY version DESC",
        )?;
        let mut rev_rows = rev_stmt.query(params![id.to_string()])?;
        let mut revisions = Vec::new();
        let mut latest_content = None;
        while let Some(rev) = rev_rows.next()? {
            let rev_id_text: String = rev.get(0)?;
            let revision_id =
                Uuid::parse_str(&rev_id_text).map_err(|_| PersistenceError::CorruptRecord {
                    id: rev_id_text.clone(),
                    reason: "revision id is not a UUID".to_owned(),
                })?;
            let content: String = rev.get(4)?;
            if latest_content.is_none() {
                latest_content = Some(content.clone());
            }
            revisions.push(firecrab_api_types::ShellRevisionSummary {
                id: revision_id,
                version: rev.get::<_, i64>(1)? as u32,
                content_sha256: rev.get(2)?,
                created_at_ms: rev.get::<_, i64>(3)? as u64,
                size_bytes: content.len() as u32,
            });
        }
        Ok(Some(firecrab_api_types::ShellDetailResponse {
            id: shell_id,
            name,
            description,
            created_at_ms,
            updated_at_ms,
            revisions,
            latest_content,
        }))
    }

    /// Deletes a shell and its revisions when no VM pins it.
    pub fn delete_shell(&self, id: Uuid) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx
            .prepare("SELECT 1 FROM shells WHERE id = ?1")?
            .exists(params![id.to_string()])?;
        if !exists {
            return Err(PersistenceError::MissingShell { id });
        }
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM vm_shells WHERE shell_id = ?1",
            params![id.to_string()],
            |row| row.get(0),
        )?;
        if count > 0 {
            return Err(PersistenceError::ShellInUse {
                id,
                count: count as u32,
            });
        }
        tx.execute(
            "DELETE FROM shell_revisions WHERE shell_id = ?1",
            params![id.to_string()],
        )?;
        tx.execute("DELETE FROM shells WHERE id = ?1", params![id.to_string()])?;
        tx.commit()?;
        Ok(())
    }

    /// Resolves each shell id to its latest revision id (errors if any missing).
    pub fn resolve_latest_shell_revisions(
        &self,
        shell_ids: &[Uuid],
    ) -> Result<Vec<(Uuid, Uuid)>, PersistenceError> {
        let conn = self.lock();
        let mut out = Vec::with_capacity(shell_ids.len());
        for shell_id in shell_ids {
            let rev: Option<String> = conn
                .query_row(
                    "SELECT id FROM shell_revisions WHERE shell_id = ?1 \
                     ORDER BY version DESC LIMIT 1",
                    params![shell_id.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(rev_text) = rev else {
                return Err(PersistenceError::MissingShell { id: *shell_id });
            };
            let revision_id =
                Uuid::parse_str(&rev_text).map_err(|_| PersistenceError::CorruptRecord {
                    id: rev_text.clone(),
                    reason: "revision id is not a UUID".to_owned(),
                })?;
            out.push((*shell_id, revision_id));
        }
        Ok(out)
    }

    /// Replaces all shell pins for a VM (ordered).
    pub fn set_vm_shells(
        &self,
        vm_id: Uuid,
        pins: &[(Uuid, Uuid)],
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM vm_shells WHERE vm_id = ?1",
            params![vm_id.to_string()],
        )?;
        for (position, (shell_id, revision_id)) in pins.iter().enumerate() {
            tx.execute(
                "INSERT INTO vm_shells (vm_id, shell_id, revision_id, position) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    vm_id.to_string(),
                    shell_id.to_string(),
                    revision_id.to_string(),
                    position as i64
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Clears shell pins when a VM is deleted.
    pub fn clear_vm_shells(&self, vm_id: Uuid) -> Result<(), PersistenceError> {
        self.lock().execute(
            "DELETE FROM vm_shells WHERE vm_id = ?1",
            params![vm_id.to_string()],
        )?;
        Ok(())
    }

    /// Pinned shell refs for API responses.
    pub fn list_vm_shell_refs(
        &self,
        vm_id: Uuid,
    ) -> Result<Vec<firecrab_api_types::ShellRef>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT vs.shell_id, vs.revision_id, r.version, s.name \
             FROM vm_shells vs \
             JOIN shells s ON s.id = vs.shell_id \
             JOIN shell_revisions r ON r.id = vs.revision_id \
             WHERE vs.vm_id = ?1 \
             ORDER BY vs.position ASC",
        )?;
        let mut rows = statement.query(params![vm_id.to_string()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let shell_text: String = row.get(0)?;
            let rev_text: String = row.get(1)?;
            let shell_id =
                Uuid::parse_str(&shell_text).map_err(|_| PersistenceError::CorruptRecord {
                    id: shell_text.clone(),
                    reason: "shell id is not a UUID".to_owned(),
                })?;
            let revision_id =
                Uuid::parse_str(&rev_text).map_err(|_| PersistenceError::CorruptRecord {
                    id: rev_text.clone(),
                    reason: "revision id is not a UUID".to_owned(),
                })?;
            out.push(firecrab_api_types::ShellRef {
                shell_id,
                revision_id,
                version: row.get::<_, i64>(2)? as u32,
                name: row.get(3)?,
            });
        }
        Ok(out)
    }

    /// Ordered script bodies for start inject.
    pub fn list_vm_shell_scripts(
        &self,
        vm_id: Uuid,
    ) -> Result<Vec<crate::shells::ShellScript>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT vs.revision_id, r.content \
             FROM vm_shells vs \
             JOIN shell_revisions r ON r.id = vs.revision_id \
             WHERE vs.vm_id = ?1 \
             ORDER BY vs.position ASC",
        )?;
        let mut rows = statement.query(params![vm_id.to_string()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let rev_text: String = row.get(0)?;
            let revision_id =
                Uuid::parse_str(&rev_text).map_err(|_| PersistenceError::CorruptRecord {
                    id: rev_text.clone(),
                    reason: "revision id is not a UUID".to_owned(),
                })?;
            out.push(crate::shells::ShellScript {
                revision_id,
                content: row.get(1)?,
            });
        }
        Ok(out)
    }

    /// Stores (replaces) a VM's port forwarding rules in SQLite.
    pub fn set_vm_port_forwards(
        &self,
        vm_id: Uuid,
        forwards: &[firecrab_api_types::PortForward],
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM port_forwards WHERE vm_id = ?1",
            params![vm_id.to_string()],
        )?;
        for pf in forwards {
            let result = tx.execute(
                "INSERT INTO port_forwards (vm_id, host_port, guest_port, protocol) VALUES (?1, ?2, ?3, ?4)",
                params![
                    vm_id.to_string(),
                    i64::from(pf.host_port),
                    i64::from(pf.guest_port),
                    pf.protocol.to_string(),
                ],
            );
            match result {
                Ok(_) => {}
                // The app-level cross-VM check callers do first is racy
                // under concurrent requests; this unique index (see its own
                // doc comment) is what actually rules the conflict out, so
                // its violation has to surface as the same typed error.
                Err(rusqlite::Error::SqliteFailure(err, _))
                    if err.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    return Err(PersistenceError::DuplicatePortForward {
                        host_port: pf.host_port,
                        protocol: pf.protocol.to_string(),
                    });
                }
                Err(error) => return Err(PersistenceError::Database(error)),
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Clears all port forwarding rules for a VM.
    pub fn clear_vm_port_forwards(&self, vm_id: Uuid) -> Result<(), PersistenceError> {
        self.lock().execute(
            "DELETE FROM port_forwards WHERE vm_id = ?1",
            params![vm_id.to_string()],
        )?;
        Ok(())
    }

    /// Fetches all port forwarding rules for a VM.
    pub fn list_vm_port_forwards(
        &self,
        vm_id: Uuid,
    ) -> Result<Vec<firecrab_api_types::PortForward>, PersistenceError> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT host_port, guest_port, protocol FROM port_forwards WHERE vm_id = ?1 ORDER BY host_port ASC",
        )?;
        let mut rows = statement.query(params![vm_id.to_string()])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let host_port: u16 = row.get::<_, i64>(0)? as u16;
            let guest_port: u16 = row.get::<_, i64>(1)? as u16;
            let proto_str: String = row.get(2)?;
            let protocol = if proto_str.eq_ignore_ascii_case("udp") {
                firecrab_api_types::PortProtocol::Udp
            } else {
                firecrab_api_types::PortProtocol::Tcp
            };
            out.push(firecrab_api_types::PortForward {
                host_port,
                guest_port,
                protocol,
            });
        }
        Ok(out)
    }

    /// Fetches all port forwarding rules across all VMs.
    pub fn list_all_port_forwards(
        &self,
    ) -> Result<Vec<(Uuid, firecrab_api_types::PortForward)>, PersistenceError> {
        let conn = self.lock();
        let mut statement =
            conn.prepare("SELECT vm_id, host_port, guest_port, protocol FROM port_forwards")?;
        let mut rows = statement.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let vm_text: String = row.get(0)?;
            let vm_id = Uuid::parse_str(&vm_text).map_err(|_| PersistenceError::CorruptRecord {
                id: vm_text.clone(),
                reason: "vm_id is not a UUID".to_owned(),
            })?;
            let host_port: u16 = row.get::<_, i64>(1)? as u16;
            let guest_port: u16 = row.get::<_, i64>(2)? as u16;
            let proto_str: String = row.get(3)?;
            let protocol = if proto_str.eq_ignore_ascii_case("udp") {
                firecrab_api_types::PortProtocol::Udp
            } else {
                firecrab_api_types::PortProtocol::Tcp
            };
            out.push((
                vm_id,
                firecrab_api_types::PortForward {
                    host_port,
                    guest_port,
                    protocol,
                },
            ));
        }
        Ok(out)
    }

    /// Allocate an IPv4 + MAC for `vm_id` inside a `BEGIN IMMEDIATE`
    /// transaction, serializing concurrent allocations on the same lock.
    pub fn allocate_lease(&self, vm_id: Uuid, subnet: SubnetSpec) -> Result<Lease, IpamError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let lease = ipam::allocate(&tx, vm_id, subnet)?;
        tx.commit()?;
        Ok(lease)
    }

    /// Atomically moves a VM away from addresses that are still occupied by
    /// host networking state but no longer have a durable Firecrab record
    /// (for example an orphaned MicroVM after an interrupted API restart).
    /// The existing lease remains active if no replacement can be committed.
    pub fn rotate_lease(
        &self,
        vm_id: Uuid,
        subnet: SubnetSpec,
        unavailable_ipv4s: &HashSet<Ipv4Addr>,
    ) -> Result<Lease, IpamError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let lease = ipam::rotate(&tx, vm_id, subnet, unavailable_ipv4s)?;
        tx.commit()?;
        Ok(lease)
    }

    /// Whether `micro_network_id` still has an active lease — checked before
    /// deleting a MicroNetwork, so a network with VMs in it can't be pulled
    /// out from under them.
    pub fn micro_network_has_active_leases(
        &self,
        micro_network_id: Uuid,
    ) -> Result<bool, IpamError> {
        ipam::has_active_leases_in(&self.lock(), micro_network_id)
    }

    /// Looks up one MicroNetwork by id.
    pub fn micro_network(
        &self,
        id: Uuid,
    ) -> Result<Option<MicroNetworkResponse>, PersistenceError> {
        Ok(self
            .list_micro_networks()?
            .into_iter()
            .find(|network| network.id == id))
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

    /// Looks up `vm_id`'s current active lease (its allocated IPv4 + MAC),
    /// if it has one — the lease persists across stop/start, so a start
    /// after the VM's first fetches the same one back rather than
    /// allocating again.
    pub fn active_lease(&self, vm_id: Uuid) -> Result<Option<Lease>, IpamError> {
        ipam::active_lease(&self.lock(), vm_id)
    }

    /// Every currently-active lease, for a full DHCP-reservation resync
    /// (see `ipam::active_leases`).
    pub fn active_leases(&self) -> Result<Vec<Lease>, IpamError> {
        ipam::active_leases(&self.lock())
    }

    /// Current lease generation (see `ipam::current_revision`), tagged onto
    /// a DHCP snapshot so the helper can reject an out-of-order stale one.
    pub fn lease_revision(&self) -> Result<u64, IpamError> {
        ipam::current_revision(&self.lock())
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

    /// Imports `legacy` (the old JSON store) if it exists, then renames it
    /// with a `.imported` suffix so re-opening never imports it again.
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
        let records: HashMap<Uuid, VmRecord> =
            serde_json::from_slice(&content).map_err(|source| {
                PersistenceError::LegacyDeserialize {
                    path: legacy.to_owned(),
                    source,
                }
            })?;

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

    /// Locks the shared connection, recovering from a poisoned mutex.
    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Drops the `vms` table so tests can force subsequent queries to fail.
    #[cfg(test)]
    pub(crate) fn break_for_tests(&self) {
        self.lock().execute("DROP TABLE vms", []).unwrap();
    }
}

fn map_local_catalog_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<LocalCatalogEntry> {
    Ok(LocalCatalogEntry {
        alias: row.get(0)?,
        architecture: row.get(1)?,
        version: row.get(2)?,
        package: row.get(3)?,
        sha256: row.get(4)?,
        min_disk_gb: row.get::<_, i64>(5)? as u16,
        published_at: row.get(6)?,
    })
}

/// Binds `vm`'s fields as parameters and executes `sql` (shared by insert,
/// update, and legacy import, which differ only in which SQL they run).
fn execute_record(conn: &Connection, sql: &str, vm: &VmRecord) -> Result<usize, rusqlite::Error> {
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
            vm.disk_gb,
            vm.egress_policy.id(),
            vm.micro_network_id.to_string(),
            vm.storage_root,
            vm.disk_generation.map(|id| id.to_string()),
            vm.last_runtime_id.map(|id| id.to_string()),
            vm.purpose.id(),
            encode_env(&vm.env),
        ],
    )
}

/// Serializes the per-VM env map the same way it crosses the API (JSON object).
fn encode_env(env: &std::collections::BTreeMap<String, String>) -> String {
    serde_json::to_string(env).unwrap_or_else(|_| "{}".to_owned())
}

/// Inverse of [`encode_env`]; fails on anything that isn't a string object.
fn decode_env(
    id: &str,
    raw: &str,
) -> Result<std::collections::BTreeMap<String, String>, PersistenceError> {
    serde_json::from_str(raw).map_err(|_| PersistenceError::CorruptRecord {
        id: id.to_owned(),
        reason: "env is not a JSON object of strings".to_owned(),
    })
}

/// Decodes a nullable id column, reporting a stored non-UUID as corruption.
fn decode_optional_id(
    vm_id: &str,
    stored: Option<String>,
) -> Result<Option<Uuid>, PersistenceError> {
    stored
        .map(|text| {
            Uuid::parse_str(&text).map_err(|_| PersistenceError::CorruptRecord {
                id: vm_id.to_owned(),
                reason: format!("id column {text:?} is not a UUID"),
            })
        })
        .transpose()
}

/// Decodes a required MicroNetwork (or similar) id column.
fn decode_required_id(
    vm_id: &str,
    stored: Option<String>,
    column: &str,
) -> Result<Uuid, PersistenceError> {
    let text = stored.ok_or_else(|| PersistenceError::CorruptRecord {
        id: vm_id.to_owned(),
        reason: format!("{column} is missing"),
    })?;
    Uuid::parse_str(&text).map_err(|_| PersistenceError::CorruptRecord {
        id: vm_id.to_owned(),
        reason: format!("{column} {text:?} is not a UUID"),
    })
}

/// Encodes through serde so the DB text stays in lockstep with the API wire
/// format.
pub(crate) fn encode_state(state: VmState) -> String {
    match serde_json::to_value(state) {
        Ok(serde_json::Value::String(name)) => name,
        _ => unreachable!("VmState serializes to a string"),
    }
}

/// Inverse of [`encode_state`]; fails on any string that isn't a known state.
fn decode_state(id: &str, name: &str) -> Result<VmState, PersistenceError> {
    serde_json::from_value(serde_json::Value::String(name.to_owned())).map_err(|_| {
        PersistenceError::CorruptRecord {
            id: id.to_owned(),
            reason: format!("unknown state {name:?}"),
        }
    })
}

/// Inverse of `EgressPolicy::id`; fails on any string that isn't a known
/// policy id.
fn decode_egress_policy(
    id: &str,
    policy: &str,
) -> Result<crate::model::EgressPolicy, PersistenceError> {
    policy.parse().map_err(|_| PersistenceError::CorruptRecord {
        id: id.to_owned(),
        reason: format!("unknown egress policy {policy:?}"),
    })
}

/// Inverse of `VmPurpose::id`; fails on any string that isn't a known
/// purpose id.
fn decode_purpose(id: &str, purpose: &str) -> Result<crate::model::VmPurpose, PersistenceError> {
    match purpose {
        "instance" => Ok(crate::model::VmPurpose::Instance),
        "builder" => Ok(crate::model::VmPurpose::Builder),
        other => Err(PersistenceError::CorruptRecord {
            id: id.to_owned(),
            reason: format!("unknown purpose {other:?}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use firecrab_api_types::Ipv6EgressMode;
    use tempfile::tempdir;

    use super::*;
    use core::assert_matches;

    fn record(id: Uuid, name: &str) -> VmRecord {
        VmRecord {
            id,
            name: name.to_owned(),
            purpose: crate::model::VmPurpose::Instance,
            state: VmState::Created,
            template: "ubuntu-26.04".to_owned(),
            template_version: "ubuntu-26.04-v1".to_owned(),
            template_kernel_sha256: "kernel".to_owned(),
            template_rootfs_sha256: "rootfs".to_owned(),
            template_boot_args_sha256: "args".to_owned(),
            cpu: 1,
            ram: 512,
            disk_gb: 2,
            egress_policy: Default::default(),
            micro_network_id: Uuid::from_u128(1),
            storage_root: "default".to_owned(),
            disk_generation: None,
            last_runtime_id: None,
            startup_step: None,
            startup_timeline: Vec::new(),
            env: Default::default(),
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

        assert_matches!(store.delete(first.id),
            Err(PersistenceError::MissingVm { id }) if id == first.id);
        let result = store.update(&record(Uuid::new_v4(), "ghost"));
        assert_matches!(result, Err(PersistenceError::MissingVm { .. }));
    }

    #[test]
    fn shell_revisions_pin_and_block_delete_while_in_use() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("shells.db")).unwrap();
        let shell_id = Uuid::new_v4();
        let rev1 = Uuid::new_v4();
        store
            .create_shell(shell_id, "web-init", None, rev1, "echo a\n", "aa", 1)
            .unwrap();
        let rev2 = Uuid::new_v4();
        let version = store
            .add_shell_revision(shell_id, rev2, "echo b\n", "bb", 2)
            .unwrap();
        assert_eq!(version, 2);

        let pins = store.resolve_latest_shell_revisions(&[shell_id]).unwrap();
        assert_eq!(pins, vec![(shell_id, rev2)]);

        let vm = record(Uuid::new_v4(), "with-shell");
        store.insert(&vm).unwrap();
        store.set_vm_shells(vm.id, &pins).unwrap();
        assert_eq!(store.list_vm_shell_scripts(vm.id).unwrap().len(), 1);
        let result = store.delete_shell(shell_id);
        assert_matches!(result, Err(PersistenceError::ShellInUse { count: 1, .. }));

        store.clear_vm_shells(vm.id).unwrap();
        store.delete_shell(shell_id).unwrap();
        assert!(store.shell_detail(shell_id).unwrap().is_none());
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
    fn migrate_egress_policy_column_adds_it_to_a_pre_existing_table() {
        let directory = tempdir().unwrap();
        let db_file = directory.path().join("firecrab.db");

        // Simulate a `vms` table created before `egress_policy` existed —
        // the same shape `CREATE_TABLE_SQL` had before this column was added.
        {
            let conn = Connection::open(&db_file).unwrap();
            conn.execute(
                "CREATE TABLE vms (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    state TEXT NOT NULL,
                    template TEXT NOT NULL,
                    template_version TEXT NOT NULL,
                    template_kernel_sha256 TEXT NOT NULL,
                    template_rootfs_sha256 TEXT NOT NULL,
                    template_boot_args_sha256 TEXT NOT NULL,
                    cpu INTEGER NOT NULL,
                    ram INTEGER NOT NULL,
                    disk_gb INTEGER NOT NULL DEFAULT 2
                ) STRICT",
                [],
            )
            .unwrap();
            let vm = record(Uuid::new_v4(), "pre-migration");
            conn.execute(
                "INSERT INTO vms (id, name, state, template, template_version, \
                 template_kernel_sha256, template_rootfs_sha256, template_boot_args_sha256, \
                 cpu, ram, disk_gb) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                    vm.disk_gb,
                ],
            )
            .unwrap();
        }

        let store = Store::open(&db_file).unwrap();
        let vms = store.load_all().unwrap();
        let (_, migrated) = vms.iter().next().expect("the pre-migration row survives");
        assert_eq!(
            migrated.egress_policy,
            crate::model::EgressPolicy::Internet,
            "a column added by migration must default to the pre-existing behavior"
        );

        // And the column is now writable, same as any other field.
        let mut updated = migrated.clone();
        updated.egress_policy = crate::model::EgressPolicy::Isolated;
        store.update(&updated).unwrap();
        assert_eq!(
            store
                .load_all()
                .unwrap()
                .get(&updated.id)
                .unwrap()
                .egress_policy,
            crate::model::EgressPolicy::Isolated
        );
    }

    #[test]
    fn insert_update_and_load_all_round_trip_env() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("env.db")).unwrap();
        let mut vm = record(Uuid::new_v4(), "with-env");
        vm.env.insert("B".to_owned(), "2".to_owned());
        vm.env.insert("A".to_owned(), "1".to_owned());
        store.insert(&vm).unwrap();

        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.get(&vm.id).unwrap().env, vm.env);
        assert_eq!(
            loaded
                .get(&vm.id)
                .unwrap()
                .env
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["A".to_owned(), "B".to_owned()]
        );

        vm.env.clear();
        vm.env.insert("APP_NAME".to_owned(), "web".to_owned());
        store.update(&vm).unwrap();
        assert_eq!(store.load_all().unwrap().get(&vm.id).unwrap().env, vm.env);

        vm.env.clear();
        store.update(&vm).unwrap();
        assert!(
            store
                .load_all()
                .unwrap()
                .get(&vm.id)
                .unwrap()
                .env
                .is_empty()
        );
    }

    #[test]
    fn a_corrupt_env_column_is_reported_as_a_corrupt_record() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("env-corrupt.db")).unwrap();
        let vm = record(Uuid::new_v4(), "env-corrupt");
        store.insert(&vm).unwrap();

        for raw in ["not-json", "[1]", r#"{"K":1}"#] {
            store
                .lock()
                .execute(
                    "UPDATE vms SET env = ?1 WHERE id = ?2",
                    params![raw, vm.id.to_string()],
                )
                .unwrap();
            assert_matches!(store.load_all(),
                    Err(PersistenceError::CorruptRecord { ref id, ref reason })
                        if id == &vm.id.to_string() && reason.contains("env is not a JSON object"), "{raw} should be reported as a corrupt env column");
        }
    }

    #[test]
    fn a_corrupt_env_column_error_does_not_include_persisted_contents() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("env-secret.db")).unwrap();
        let vm = record(Uuid::new_v4(), "env-secret");
        store.insert(&vm).unwrap();
        const SECRET: &str = "sentinel-secret-do-not-leak";
        store
            .lock()
            .execute(
                "UPDATE vms SET env = ?1 WHERE id = ?2",
                params![
                    format!(r#"{{"APP_NAME":"{SECRET}","n":1}}"#),
                    vm.id.to_string()
                ],
            )
            .unwrap();

        let error = store.load_all().unwrap_err();
        let rendered = error.to_string();
        assert!(
            !rendered.contains(SECRET),
            "CorruptRecord must not echo persisted env: {rendered}"
        );
        assert_matches!(error,
                PersistenceError::CorruptRecord { ref reason, .. }
                    if reason == "env is not a JSON object of strings" && !reason.contains(SECRET), "{error}");
    }

    #[test]
    fn migrate_env_column_defaults_an_existing_row_to_empty_map() {
        let directory = tempdir().unwrap();
        let db_file = directory.path().join("firecrab.db");
        let id = Uuid::new_v4();

        {
            let conn = Connection::open(&db_file).unwrap();
            conn.execute(
                "CREATE TABLE vms (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    state TEXT NOT NULL,
                    template TEXT NOT NULL,
                    template_version TEXT NOT NULL,
                    template_kernel_sha256 TEXT NOT NULL,
                    template_rootfs_sha256 TEXT NOT NULL,
                    template_boot_args_sha256 TEXT NOT NULL,
                    cpu INTEGER NOT NULL,
                    ram INTEGER NOT NULL,
                    disk_gb INTEGER NOT NULL DEFAULT 2,
                    egress_policy TEXT NOT NULL DEFAULT 'internet',
                    micro_network_id TEXT,
                    storage_root TEXT NOT NULL DEFAULT 'default',
                    disk_generation TEXT,
                    last_runtime_id TEXT,
                    purpose TEXT NOT NULL DEFAULT 'instance'
                ) STRICT",
                [],
            )
            .unwrap();
            let vm = record(id, "pre-env");
            conn.execute(
                "INSERT INTO vms (id, name, state, template, template_version, \
                 template_kernel_sha256, template_rootfs_sha256, template_boot_args_sha256, \
                 cpu, ram, disk_gb, egress_policy, micro_network_id, storage_root, \
                 disk_generation, last_runtime_id, purpose) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
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
                    vm.disk_gb,
                    vm.egress_policy.id(),
                    vm.micro_network_id.to_string(),
                    vm.storage_root,
                    Option::<String>::None,
                    Option::<String>::None,
                    vm.purpose.id(),
                ],
            )
            .unwrap();
        }

        let store = Store::open(&db_file).unwrap();
        let migrated = store.load_all().unwrap();
        let row = migrated.get(&id).expect("the pre-migration row survives");
        assert!(
            row.env.is_empty(),
            "a column added by migration must default to {{}}"
        );

        let mut updated = row.clone();
        updated.env.insert("K".to_owned(), "v".to_owned());
        store.update(&updated).unwrap();
        assert_eq!(
            store.load_all().unwrap().get(&id).unwrap().env.get("K"),
            Some(&"v".to_owned())
        );
    }

    #[test]
    fn migrate_internet_enabled_column_defaults_an_existing_network_to_connected() {
        let directory = tempdir().unwrap();
        let db_file = directory.path().join("firecrab.db");
        let id = Uuid::new_v4();

        // A `micro_networks` table from before the internet toggle existed.
        {
            let conn = Connection::open(&db_file).unwrap();
            conn.execute(
                "CREATE TABLE micro_networks (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    subnet_cidr TEXT NOT NULL
                ) STRICT",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO micro_networks (id, name, subnet_cidr) VALUES (?1, ?2, ?3)",
                params![id.to_string(), "pre-migration", "172.31.0.0/24"],
            )
            .unwrap();
        }

        let store = Store::open(&db_file).unwrap();
        let network = store.micro_network(id).unwrap().expect("row survives");
        assert!(
            network.internet_enabled,
            "every network was masqueraded before the toggle existed"
        );

        // And it is writable from here on.
        store.set_micro_network_internet(id, false).unwrap();
        assert!(!store.micro_network(id).unwrap().unwrap().internet_enabled);
    }

    #[test]
    fn migrate_ipv6_columns_leaves_an_existing_network_ipv4_only() {
        let directory = tempdir().unwrap();
        let db_file = directory.path().join("firecrab.db");
        let id = Uuid::new_v4();

        // A `micro_networks` table from before dual-stack existed.
        {
            let conn = Connection::open(&db_file).unwrap();
            conn.execute(
                "CREATE TABLE micro_networks (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    subnet_cidr TEXT NOT NULL
                ) STRICT",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO micro_networks (id, name, subnet_cidr) VALUES (?1, ?2, ?3)",
                params![id.to_string(), "pre-migration", "172.31.0.0/24"],
            )
            .unwrap();
        }

        let store = Store::open(&db_file).unwrap();
        let network = store.micro_network(id).unwrap().expect("row survives");
        // A network created before it had a prefix stays IPv4-only rather
        // than silently acquiring one under its running VMs.
        assert_eq!(network.ipv6_cidr, None);
        assert_eq!(network.ipv6_gateway, None);
        assert_eq!(network.ipv6_egress, None);
    }

    #[test]
    fn a_dual_stack_network_derives_its_v6_gateway_and_egress_mode() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("firecrab.db")).unwrap();

        let ula = MicroNetworkResponse {
            id: Uuid::new_v4(),
            name: "ula".to_owned(),
            subnet_cidr: "172.31.0.0/24".to_owned(),
            gateway: "172.31.0.1".to_owned(),
            internet_enabled: true,
            uplink: None,
            ipv6_cidr: Some("fd00:1::/64".to_owned()),
            ipv6_gateway: Some("fd00:1::1".to_owned()),
            ipv6_address_mode: Some(Ipv6AddressMode::Slaac),
            ipv6_egress: Some(Ipv6EgressMode::Nat66),
        };
        let gua = MicroNetworkResponse {
            id: Uuid::new_v4(),
            name: "gua".to_owned(),
            subnet_cidr: "172.32.0.0/24".to_owned(),
            gateway: "172.32.0.1".to_owned(),
            ipv6_cidr: Some("2001:db8:1::/64".to_owned()),
            ipv6_gateway: Some("2001:db8:1::1".to_owned()),
            ipv6_address_mode: Some(Ipv6AddressMode::Dhcpv6),
            ipv6_egress: Some(Ipv6EgressMode::Direct),
            ..ula.clone()
        };
        store.insert_micro_network(&ula).unwrap();
        store.insert_micro_network(&gua).unwrap();

        let stored = store.micro_network(ula.id).unwrap().unwrap();
        // Derived on read, like the v4 gateway — never stored, so it cannot
        // drift from the prefix it belongs to.
        assert_eq!(stored.ipv6_gateway.as_deref(), Some("fd00:1::1"));
        assert_eq!(stored.ipv6_egress, Some(Ipv6EgressMode::Nat66));
        assert_eq!(stored.ipv6_address_mode, Some(Ipv6AddressMode::Slaac));

        let stored = store.micro_network(gua.id).unwrap().unwrap();
        assert_eq!(stored.ipv6_gateway.as_deref(), Some("2001:db8:1::1"));
        // A global prefix is routable as-is: reporting NAT66 here would
        // describe a translation the helper never renders.
        assert_eq!(stored.ipv6_egress, Some(Ipv6EgressMode::Direct));
        assert_eq!(stored.ipv6_address_mode, Some(Ipv6AddressMode::Dhcpv6));
    }

    #[test]
    fn migrate_lease_ipv6_column_leaves_existing_leases_ipv4_only() {
        let directory = tempdir().unwrap();
        let db_file = directory.path().join("firecrab.db");
        let vm_id = Uuid::new_v4();

        // A `network_leases` table from before dual-stack existed.
        {
            let conn = Connection::open(&db_file).unwrap();
            conn.execute(
                "CREATE TABLE network_leases (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    vm_id TEXT NOT NULL,
                    ipv4 TEXT NOT NULL,
                    mac TEXT NOT NULL,
                    allocated_at TEXT NOT NULL,
                    released_at TEXT,
                    micro_network_id TEXT
                ) STRICT",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO network_leases (vm_id, ipv4, mac, allocated_at, micro_network_id) \
                 VALUES (?1, ?2, ?3, datetime('now'), ?4)",
                params![
                    vm_id.to_string(),
                    "172.31.0.5",
                    "02:fc:00:00:00:05",
                    Uuid::new_v4().to_string()
                ],
            )
            .unwrap();
        }

        let store = Store::open(&db_file).unwrap();
        let lease = store.active_lease(vm_id).unwrap().expect("lease survives");
        assert_eq!(lease.ipv6, None);
    }

    #[test]
    fn migrate_uplink_column_leaves_existing_rows_null() {
        let directory = tempdir().unwrap();
        let db_file = directory.path().join("firecrab.db");
        let id = Uuid::new_v4();

        {
            let conn = Connection::open(&db_file).unwrap();
            conn.execute(
                "CREATE TABLE micro_networks (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    subnet_cidr TEXT NOT NULL,
                    internet_enabled INTEGER NOT NULL DEFAULT 1
                ) STRICT",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO micro_networks (id, name, subnet_cidr, internet_enabled) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![id.to_string(), "pre-uplink", "172.31.0.0/24", 1],
            )
            .unwrap();
        }

        let store = Store::open(&db_file).unwrap();
        let network = store.micro_network(id).unwrap().expect("row survives");
        assert_eq!(
            network.uplink, None,
            "NULL uplink keeps the host default-route iface"
        );
    }

    #[test]
    fn set_micro_network_uplink_round_trips() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("firecrab.db")).unwrap();
        let id = Uuid::new_v4();
        store
            .insert_micro_network(&MicroNetworkResponse {
                id,
                name: "prod".to_owned(),
                subnet_cidr: "172.31.0.0/24".to_owned(),
                gateway: "172.31.0.1".to_owned(),
                internet_enabled: true,
                uplink: None,
                ipv6_cidr: None,
                ipv6_gateway: None,
                ipv6_address_mode: None,
                ipv6_egress: None,
            })
            .unwrap();

        store
            .set_micro_network_uplink(id, Some("eth1".to_owned()))
            .unwrap();
        assert_eq!(
            store.micro_network(id).unwrap().unwrap().uplink.as_deref(),
            Some("eth1")
        );

        store.set_micro_network_uplink(id, None).unwrap();
        assert_eq!(store.micro_network(id).unwrap().unwrap().uplink, None);
    }

    #[test]
    fn setting_the_uplink_on_a_missing_network_is_an_error() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("firecrab.db")).unwrap();
        let id = Uuid::new_v4();
        assert_matches!(store.set_micro_network_uplink(id, Some("eth0".to_owned())).unwrap_err(),
            PersistenceError::MissingMicroNetwork { id: missing } if missing == id);
    }

    #[test]
    fn setting_the_internet_flag_on_a_missing_network_is_an_error() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("firecrab.db")).unwrap();
        let id = Uuid::new_v4();
        assert_matches!(store.set_micro_network_internet(id, false).unwrap_err(),
            PersistenceError::MissingMicroNetwork { id: missing } if missing == id);
    }

    #[test]
    fn purpose_round_trips_through_insert_and_load() {
        let dir = tempdir().unwrap();
        let store = Store::open(&dir.path().join("test.db")).unwrap();
        let mut vm = record(Uuid::new_v4(), "builder-vm");
        vm.purpose = crate::model::VmPurpose::Builder;
        store.insert(&vm).unwrap();

        let loaded = store.load_all().unwrap();
        assert_eq!(loaded[&vm.id].purpose, crate::model::VmPurpose::Builder);
    }

    #[test]
    fn decode_egress_policy_rejects_an_unknown_value_as_corrupt() {
        let error = decode_egress_policy("some-id", "wide-open").unwrap_err();
        assert_matches!(error,
            PersistenceError::CorruptRecord { id, reason }
                if id == "some-id" && reason.contains("wide-open"));
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

    /// Records written before multi-disk / configurable-disk support have no
    /// `storage_root` or `disk_gb` at all; the import must land them on the
    /// legacy `data/vms` root instead of failing.
    #[test]
    fn legacy_records_without_storage_root_import_onto_the_default_root() {
        let directory = tempdir().unwrap();
        let id = Uuid::new_v4();
        let legacy = serde_json::json!({
            id.to_string(): {
                "id": id,
                "name": "pre-multi-disk",
                "state": "stopped",
                "template": "ubuntu-26.04",
                "cpu": 1,
                "ram": 512,
                "micro_network_id": Uuid::from_u128(1),
            }
        });
        fs::write(
            directory.path().join("vms.json"),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();

        let store = Store::open(&directory.path().join("firecrab.db")).unwrap();
        let all = store.load_all().unwrap();
        let imported = all.get(&id).expect("legacy record imported");
        assert_eq!(imported.storage_root, "default");
        assert_eq!(imported.disk_gb, 2);
        assert_eq!(imported.state, VmState::Stopped);
    }

    /// Rows are decoded defensively: anything hand-edited into an
    /// unparseable id surfaces as `CorruptRecord` instead of a panic or a
    /// silently wrong record.
    #[test]
    fn corrupt_id_columns_are_reported_as_corrupt_records() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("firecrab.db")).unwrap();
        let vm = record(Uuid::new_v4(), "corruptible");
        store.insert(&vm).unwrap();
        let id = vm.id.to_string();

        for (column, value) in [
            ("micro_network_id", Some("not-a-uuid")),
            ("micro_network_id", None),
            ("disk_generation", Some("not-a-uuid")),
        ] {
            store
                .lock()
                .execute(
                    &format!("UPDATE vms SET {column} = ?1 WHERE id = ?2"),
                    params![value, id],
                )
                .unwrap();
            let loaded = store.load_all();
            assert!(loaded.is_err(), "{column}={value:?} should be corrupt");
            assert_matches!(loaded, Err(PersistenceError::CorruptRecord { .. }));
        }
    }

    #[test]
    fn a_micro_storage_row_with_a_non_uuid_id_is_corrupt() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("firecrab.db")).unwrap();
        store
            .lock()
            .execute(
                "INSERT INTO micro_storages (id, name, path) VALUES ('not-a-uuid', 'p', '/mnt/p')",
                [],
            )
            .unwrap();

        let result = store.list_micro_storages();
        assert_matches!(result, Err(PersistenceError::CorruptRecord { .. }));
    }

    /// Only a UNIQUE violation means "already registered" — every other
    /// SQLite failure must stay a plain database error so the handler
    /// answers 500 rather than a misleading validation message.
    #[test]
    fn a_non_constraint_failure_inserting_a_micro_storage_stays_a_database_error() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("firecrab.db")).unwrap();
        store
            .lock()
            .execute("DROP TABLE micro_storages", [])
            .unwrap();

        let result = store.insert_micro_storage(Uuid::new_v4(), "p", "/mnt/p");
        assert_matches!(result, Err(PersistenceError::Database(_)));
    }

    #[test]
    fn malformed_legacy_json_fails_open() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("vms.json"), b"{invalid").unwrap();

        let result = Store::open(&directory.path().join("firecrab.db"));
        assert_matches!(result, Err(PersistenceError::LegacyDeserialize { .. }));
    }

    #[test]
    fn set_vm_port_forwards_rejects_a_host_port_already_owned_by_another_vm() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("firecrab.db")).unwrap();
        let vm_a = record(Uuid::new_v4(), "vm-a");
        let vm_b = record(Uuid::new_v4(), "vm-b");
        store.insert(&vm_a).unwrap();
        store.insert(&vm_b).unwrap();

        let claimed = firecrab_api_types::PortForward {
            host_port: 8080,
            guest_port: 80,
            protocol: firecrab_api_types::PortProtocol::Tcp,
        };
        store
            .set_vm_port_forwards(vm_a.id, std::slice::from_ref(&claimed))
            .unwrap();

        // The unique index (not just the application-level pre-check the
        // handler already does) is what must reject this — this is exactly
        // the case a check-then-insert race could otherwise let through.
        let result = store.set_vm_port_forwards(vm_b.id, &[claimed]);
        assert_matches!(&result,
                Err(PersistenceError::DuplicatePortForward { host_port: 8080, protocol })
                    if protocol == "tcp", "expected a typed conflict, got {result:?}");
        // The whole write must have rolled back, not left a partial row.
        assert!(store.list_vm_port_forwards(vm_b.id).unwrap().is_empty());
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
                std::thread::spawn(move || {
                    store
                        .allocate_lease(
                            Uuid::new_v4(),
                            SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)),
                        )
                        .unwrap()
                })
            })
            .collect();
        let leases: Vec<Lease> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();

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

    fn local_catalog_entry(alias: &str, architecture: &str) -> LocalCatalogEntry {
        LocalCatalogEntry {
            alias: alias.to_owned(),
            architecture: architecture.to_owned(),
            version: "1".to_owned(),
            package: String::new(),
            sha256: String::new(),
            min_disk_gb: 1,
            published_at: "2026-08-15T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn microregistry_local_round_trips_through_insert_list_and_get() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("firecrab.db")).unwrap();
        let entry = local_catalog_entry("nginx-1.27", "x86_64");

        store.insert_microregistry_local(&entry).unwrap();
        assert_eq!(
            store
                .microregistry_local("nginx-1.27", "x86_64")
                .unwrap()
                .as_ref(),
            Some(&entry)
        );
        assert_eq!(
            store.list_microregistry_local(Some("x86_64")).unwrap(),
            std::slice::from_ref(&entry)
        );
        assert!(
            store
                .list_microregistry_local(Some("aarch64"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.list_microregistry_local(None).unwrap(), [entry]);
    }

    #[test]
    fn microregistry_local_rejects_a_duplicate_alias_architecture_pair() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("firecrab.db")).unwrap();
        let entry = local_catalog_entry("nginx-1.27", "x86_64");
        store.insert_microregistry_local(&entry).unwrap();

        let result = store.insert_microregistry_local(&entry);
        assert_matches!(&result,
                Err(PersistenceError::DuplicateMicroRegistryLocal { alias, architecture })
                    if alias == "nginx-1.27" && architecture == "x86_64", "expected a typed constraint conflict, got {result:?}");

        let other_arch = local_catalog_entry("nginx-1.27", "aarch64");
        store.insert_microregistry_local(&other_arch).unwrap();
        assert_eq!(store.list_microregistry_local(None).unwrap().len(), 2);
    }

    /// The store now holds a registry token, so the file must not be readable
    /// by every local account no matter what umask the service runs under.
    #[test]
    fn the_database_file_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempdir().unwrap();
        let path = directory.path().join("firecrab.db");
        let store = Store::open(&path).unwrap();
        store
            .put_docker_hub_credential("pista", "dckr_pat_secret")
            .unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "database mode is {:o}", mode & 0o777);
        for sidecar in ["firecrab.db-wal", "firecrab.db-shm"] {
            let sidecar = directory.path().join(sidecar);
            if let Ok(metadata) = std::fs::metadata(&sidecar) {
                let mode = metadata.permissions().mode();
                assert_eq!(
                    mode & 0o077,
                    0,
                    "{} mode is {:o}",
                    sidecar.display(),
                    mode & 0o777
                );
            }
        }
    }

    #[test]
    fn docker_hub_credential_round_trips_and_can_be_deleted() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("firecrab.db")).unwrap();
        assert_eq!(store.docker_hub_credential().unwrap(), None);

        store
            .put_docker_hub_credential("pista", "dckr_pat_secret")
            .unwrap();
        let stored = store.docker_hub_credential().unwrap().unwrap();
        assert_eq!(stored.username, "pista");
        assert_eq!(stored.secret, "dckr_pat_secret");

        store.put_docker_hub_credential("pista", "rotated").unwrap();
        assert_eq!(
            store.docker_hub_credential().unwrap().unwrap().secret,
            "rotated"
        );

        store.delete_docker_hub_credential().unwrap();
        assert_eq!(store.docker_hub_credential().unwrap(), None);
    }

    #[test]
    fn opening_an_existing_database_creates_microregistry_local() {
        let directory = tempdir().unwrap();
        let db_file = directory.path().join("firecrab.db");
        let store = Store::open(&db_file).unwrap();
        store
            .lock()
            .execute("DROP TABLE microregistry_local", [])
            .unwrap();
        drop(store);

        let reopened = Store::open(&db_file).unwrap();
        let exists: bool = reopened
            .lock()
            .prepare(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'microregistry_local'",
            )
            .unwrap()
            .exists([])
            .unwrap();
        assert!(exists, "upgrade must CREATE TABLE IF NOT EXISTS");
        reopened
            .insert_microregistry_local(&local_catalog_entry("nginx-1.27", "x86_64"))
            .unwrap();
        assert_eq!(
            reopened
                .microregistry_local("nginx-1.27", "x86_64")
                .unwrap()
                .unwrap()
                .alias,
            "nginx-1.27"
        );
    }

    #[test]
    fn lease_persists_across_stop_start_and_frees_only_after_release() {
        let directory = tempdir().unwrap();
        let store = Store::open(&directory.path().join("firecrab.db")).unwrap();
        let vm_id = Uuid::new_v4();

        let lease = store
            .allocate_lease(vm_id, SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)))
            .unwrap();
        // Simulate stop/start: nothing in the lifecycle touches the lease.
        assert_eq!(
            store
                .allocate_lease(vm_id, SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)))
                .unwrap_err()
                .to_string(),
            IpamError::AlreadyLeased { vm_id }.to_string()
        );

        store.release_lease(vm_id).unwrap();
        let other_vm = Uuid::new_v4();
        let reallocated = store
            .allocate_lease(
                other_vm,
                SubnetSpec::legacy_default_subnet(Uuid::from_u128(1)),
            )
            .unwrap();
        assert_eq!(
            reallocated.ipv4, lease.ipv4,
            "freed address should be reusable"
        );
    }
}

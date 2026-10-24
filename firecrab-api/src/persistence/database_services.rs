//! Private database service configuration. Payloads and credentials are never logged.
use super::{PersistenceError, Store};
use crate::pipe_databases::bindings::BindingRecord;
use crate::pipe_databases::events::{RabbitMQInput, Trigger};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

/// Private SQLite tables for account ownership, delivery and maintenance state.
pub(super) const CREATE_SQL:&str="
CREATE TABLE IF NOT EXISTS database_rabbitmq (id INTEGER PRIMARY KEY CHECK(id=1), spec TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS database_triggers (id TEXT PRIMARY KEY, spec TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS database_service_secrets (id TEXT PRIMARY KEY, secret TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS database_bindings (id TEXT PRIMARY KEY, service_id TEXT NOT NULL, database_id TEXT NOT NULL, spec TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS database_vm_services (id TEXT PRIMARY KEY, service_id TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS database_trigger_health (id TEXT PRIMARY KEY, error TEXT) STRICT;
CREATE TABLE IF NOT EXISTS database_managed_rabbitmq (id INTEGER PRIMARY KEY CHECK(id=1), spec TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS database_jobs (id TEXT PRIMARY KEY, database_id TEXT NOT NULL, spec TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS database_setup_claims (id TEXT NOT NULL, target_hash TEXT NOT NULL, PRIMARY KEY(id,target_hash)) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS database_binding_environment ON database_bindings(service_id,json_extract(spec,'$.input.envName'));
";

/// Serialize private state without including raw data in persistence errors.
fn encode<T: serde::Serialize>(value: &T) -> Result<String, PersistenceError> {
    serde_json::to_string(value).map_err(|_| PersistenceError::CorruptRecord {
        id: "database_services".into(),
        reason: "invalid configuration".into(),
    })
}
/// Decode private state with redacted corruption diagnostics.
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, PersistenceError> {
    serde_json::from_str(value).map_err(|_| PersistenceError::CorruptRecord {
        id: "database_services".into(),
        reason: "invalid stored configuration".into(),
    })
}

impl Store {
    /// Check the durable intent recorded before creating a MySQL namespace.
    pub fn database_setup_claim(&self, id: Uuid, target: &str) -> Result<bool, PersistenceError> {
        Ok(self.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM database_setup_claims WHERE id=?1 AND target_hash=?2)",
            params![id.to_string(), target],
            |row| row.get(0),
        )?)
    }
    /// Keep namespace creation ownership across cancellation or host restart.
    pub fn set_database_setup_claim(&self, id: Uuid, target: &str) -> Result<(), PersistenceError> {
        self.lock().execute(
            "INSERT OR IGNORE INTO database_setup_claims VALUES (?1,?2)",
            params![id.to_string(), target],
        )?;
        Ok(())
    }
    /// Clear temporary ownership after the source owner marker is verified.
    pub fn clear_database_setup_claim(
        &self,
        id: Uuid,
        target: &str,
    ) -> Result<(), PersistenceError> {
        self.lock().execute(
            "DELETE FROM database_setup_claims WHERE id=?1 AND target_hash=?2",
            params![id.to_string(), target],
        )?;
        Ok(())
    }
    /// Read one private job by its stable UUID.
    pub fn database_job(
        &self,
        id: Uuid,
    ) -> Result<Option<crate::pipe_databases::maintenance::Job>, PersistenceError> {
        let spec: Option<String> = self
            .lock()
            .query_row(
                "SELECT spec FROM database_jobs WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        spec.as_deref().map(decode).transpose()
    }
    /// List a connection's private jobs with the newest first.
    pub fn database_jobs(
        &self,
        id: Uuid,
    ) -> Result<Vec<crate::pipe_databases::maintenance::Job>, PersistenceError> {
        let conn = self.lock();
        let mut query = conn
            .prepare("SELECT spec FROM database_jobs WHERE database_id=?1 ORDER BY rowid DESC")?;
        query
            .query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|r| decode(&r?))
            .collect()
    }
    /// Persist maintenance progress before subsequent destructive work.
    pub fn save_database_job(
        &self,
        job: &crate::pipe_databases::maintenance::Job,
    ) -> Result<(), PersistenceError> {
        self.lock().execute("INSERT INTO database_jobs VALUES (?1,?2,?3) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec",params![job.id.to_string(),job.database_id.to_string(),encode(job)?])?;
        Ok(())
    }
    /// Detect queued or incomplete destructive work across matching saved endpoints.
    pub fn database_maintenance_active(&self, id: Uuid) -> Result<bool, PersistenceError> {
        Ok(self.database_target_jobs(id)?.iter().any(|j| {
            j.kind != crate::pipe_databases::maintenance::Kind::Backup
                && matches!(j.status.as_str(), "queued" | "running" | "needs_recovery")
        }))
    }
    /// Collect jobs for saved connections with the same target fingerprint.
    pub fn database_target_jobs(
        &self,
        id: Uuid,
    ) -> Result<Vec<crate::pipe_databases::maintenance::Job>, PersistenceError> {
        let Some(record) = self.pipe_database(id)? else {
            return Ok(vec![]);
        };
        let hash = crate::pipe_databases::maintenance::target_hash(&record).map_err(|_| {
            PersistenceError::CorruptRecord {
                id: id.to_string(),
                reason: "invalid database target".into(),
            }
        })?;
        let conn = self.lock();
        let mut query=conn.prepare("SELECT spec FROM database_jobs WHERE json_extract(spec,'$.targetHash')=?1 ORDER BY rowid DESC")?;
        query
            .query_map([hash], |r| r.get::<_, String>(0))?
            .map(|r| decode(&r?))
            .collect()
    }
    /// Derive private archive storage beside the file-backed SQLite database.
    pub fn database_backup_directory(&self) -> Result<std::path::PathBuf, PersistenceError> {
        let path: String = self.lock().query_row(
            "SELECT file FROM pragma_database_list WHERE name='main'",
            [],
            |r| r.get(0),
        )?;
        let path = std::path::Path::new(&path);
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| PersistenceError::CorruptRecord {
                id: "database_jobs".into(),
                reason: "file-backed persistence is required".into(),
            })?;
        Ok(parent.join("database-backups"))
    }
    /// Remove trigger metadata after the caller verifies retained events are drained.
    pub fn delete_database_trigger(&self, id: Uuid) -> Result<(), PersistenceError> {
        self.lock().execute(
            "DELETE FROM database_triggers WHERE id=?1",
            [id.to_string()],
        )?;
        Ok(())
    }
    /// Read private broker ownership and generated TLS material.
    pub fn managed_database_rabbitmq(
        &self,
    ) -> Result<Option<crate::pipe_databases::managed_rabbitmq::Cluster>, PersistenceError> {
        let spec: Option<String> = self
            .lock()
            .query_row(
                "SELECT spec FROM database_managed_rabbitmq WHERE id=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        spec.as_deref().map(decode).transpose()
    }
    /// Persist broker ownership before VM preparation or activation.
    pub fn save_managed_database_rabbitmq(
        &self,
        cluster: &crate::pipe_databases::managed_rabbitmq::Cluster,
    ) -> Result<(), PersistenceError> {
        self.lock().execute("INSERT INTO database_managed_rabbitmq VALUES (1,?1) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec",[encode(cluster)?])?;
        Ok(())
    }
    /// Read private generated account bindings for service deployment.
    pub fn database_bindings(&self) -> Result<Vec<BindingRecord>, PersistenceError> {
        let conn = self.lock();
        let mut query = conn.prepare("SELECT spec FROM database_bindings ORDER BY rowid")?;
        query
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|r| decode(&r?))
            .collect()
    }
    /// Persist a binding while preserving unique service environment names.
    pub fn save_database_binding(&self, binding: &BindingRecord) -> Result<(), PersistenceError> {
        self.lock().execute("INSERT INTO database_bindings VALUES (?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec",params![binding.id.to_string(),binding.input.service_id.to_string(),binding.database_id.to_string(),encode(binding)?])?;
        Ok(())
    }
    /// Remove private binding metadata after account revocation.
    pub fn delete_database_binding(&self, id: Uuid) -> Result<(), PersistenceError> {
        self.lock().execute(
            "DELETE FROM database_bindings WHERE id=?1",
            [id.to_string()],
        )?;
        Ok(())
    }
    /// Store a redacted worker health reason for dashboard polling.
    pub fn set_database_trigger_health(
        &self,
        id: Uuid,
        error: Option<&str>,
    ) -> Result<(), PersistenceError> {
        self.lock().execute("INSERT INTO database_trigger_health VALUES (?1,?2) ON CONFLICT(id) DO UPDATE SET error=excluded.error",params![id.to_string(),error])?;
        Ok(())
    }
    /// Read a trigger's latest redacted worker failure.
    pub fn database_trigger_health(&self, id: Uuid) -> Result<Option<String>, PersistenceError> {
        Ok(self
            .lock()
            .query_row(
                "SELECT error FROM database_trigger_health WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }
    /// Read private RabbitMQ authentication settings for relay workers.
    pub fn database_rabbitmq(&self) -> Result<Option<RabbitMQInput>, PersistenceError> {
        let spec: Option<String> = self
            .lock()
            .query_row("SELECT spec FROM database_rabbitmq WHERE id=1", [], |r| {
                r.get(0)
            })
            .optional()?;
        spec.as_deref().map(decode).transpose()
    }
    /// Replace private RabbitMQ settings after the caller checks retained triggers.
    pub fn save_database_rabbitmq(&self, input: &RabbitMQInput) -> Result<(), PersistenceError> {
        self.lock().execute("INSERT INTO database_rabbitmq VALUES (1,?1) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec",[encode(input)?])?;
        Ok(())
    }
    /// Read capture definitions and persisted delivery controls.
    pub fn database_triggers(&self) -> Result<Vec<Trigger>, PersistenceError> {
        let conn = self.lock();
        let mut query = conn.prepare("SELECT spec FROM database_triggers ORDER BY rowid")?;
        query
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|r| decode(&r?))
            .collect()
    }
    /// Read one retained capture definition by UUID.
    pub fn database_trigger(&self, id: Uuid) -> Result<Option<Trigger>, PersistenceError> {
        let spec: Option<String> = self
            .lock()
            .query_row(
                "SELECT spec FROM database_triggers WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        spec.as_deref().map(decode).transpose()
    }
    /// Persist capture ownership before nontransactional setup or lifecycle changes.
    pub fn save_database_trigger(&self, trigger: &Trigger) -> Result<(), PersistenceError> {
        self.lock().execute("INSERT INTO database_triggers VALUES (?1,?2) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec",params![trigger.id.to_string(),encode(trigger)?])?;
        Ok(())
    }
    /// Return or create the private HMAC key for one handler service.
    pub fn database_service_secret(&self, id: Uuid) -> Result<String, PersistenceError> {
        let conn = self.lock();
        let secret = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        conn.execute(
            "INSERT OR IGNORE INTO database_service_secrets VALUES (?1,?2)",
            params![id.to_string(), secret],
        )?;
        Ok(conn.query_row(
            "SELECT secret FROM database_service_secrets WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?)
    }
    /// Persist service ownership before installing private VM inputs.
    pub fn link_database_service_vm(
        &self,
        vm_id: Uuid,
        service_id: Uuid,
    ) -> Result<(), PersistenceError> {
        self.lock().execute("INSERT INTO database_vm_services VALUES (?1,?2) ON CONFLICT(id) DO UPDATE SET service_id=excluded.service_id",params![vm_id.to_string(),service_id.to_string()])?;
        Ok(())
    }
    /// Resolve a service-owned VM without putting secrets in VM metadata.
    pub fn database_vm_service(&self, vm_id: Uuid) -> Result<Option<Uuid>, PersistenceError> {
        let id: Option<String> = self
            .lock()
            .query_row(
                "SELECT service_id FROM database_vm_services WHERE id=?1",
                [vm_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        id.map(|id| {
            Uuid::parse_str(&id).map_err(|_| PersistenceError::CorruptRecord {
                id,
                reason: "invalid service ID".into(),
            })
        })
        .transpose()
    }
}

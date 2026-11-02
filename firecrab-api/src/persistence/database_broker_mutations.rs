//! Connection verification cannot replace broker inputs across a native event reservation.
use super::{
    PersistenceError, Store,
    database_services::{decode, encode},
    iac_native, iac_owners,
};
use crate::pipe_databases::{events::RabbitMQInput, managed_rabbitmq::Cluster};
use firecrab_stack::{ManagedResource, ResourceOwnership};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};

/// Identify the host's singleton delivery broker configuration.
pub(super) fn resource() -> ManagedResource {
    ManagedResource {
        kind: "database_rabbitmq".into(),
        id: "default".into(),
    }
}
/// Compare private settings without exposing their values in errors.
pub(super) fn fingerprint<T: serde::Serialize>(value: &T) -> Result<String, PersistenceError> {
    Ok(format!("{:x}", Sha256::digest(encode(value)?)))
}
/// Read private client settings in the caller's transaction.
pub(super) fn configuration(conn: &Connection) -> Result<Option<RabbitMQInput>, PersistenceError> {
    let raw: Option<String> = conn
        .query_row("SELECT spec FROM database_rabbitmq WHERE id=1", [], |r| {
            r.get(0)
        })
        .optional()?;
    raw.as_deref().map(decode).transpose()
}
/// Read the retained owned cluster and broker identities.
pub(super) fn managed(conn: &Connection) -> Result<Option<Cluster>, PersistenceError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT spec FROM database_managed_rabbitmq WHERE id=1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    raw.as_deref().map(decode).transpose()
}
/// Separate desired configuration from health phase/error observations.
pub(super) fn managed_configuration(value: Option<&Cluster>) -> Result<String, PersistenceError> {
    let value = value.map(|c| {
        let mut c = c.clone();
        c.phase.clear();
        c.error = None;
        c
    });
    fingerprint(&value)
}
#[derive(Clone)]
/// Verified writer snapshot captured before remote connection checks.
pub(crate) struct BrokerAuthority {
    /// Exact endpoint/resource/owner generation before verification.
    owner: ResourceOwnership,
    /// Saved private client settings, compared without exposing their values.
    input: String,
    /// Full managed record expected before committing a verified update.
    managed: String,
    /// Configuration generation that detects settings ABA.
    epoch: i64,
}
/// Read the monotonic explicit configuration generation.
pub(super) fn epoch(conn: &Connection) -> Result<i64, PersistenceError> {
    Ok(conn.query_row(
        "SELECT generation FROM database_broker_epoch WHERE id=1",
        [],
        |r| r.get(0),
    )?)
}
/// Invalidate old writers even when settings change back to their original values.
pub(super) fn advance(conn: &Connection) -> Result<(), PersistenceError> {
    if conn.execute("UPDATE database_broker_epoch SET generation=generation+1 WHERE id=1 AND generation<9223372036854775807", [])? != 1 {
        return Err(PersistenceError::StaleIacOwner);
    }
    Ok(())
}
/// A controller retains the original management epoch across all remote work.
#[derive(Clone)]
pub(crate) struct BrokerWatch {
    /// Original management generation and current controller checkpoint.
    authority: BrokerAuthority,
    /// Exact managed cluster identity that may be supervised.
    id: uuid::Uuid,
}
/// A private parent permission checked again inside VM insertion.
pub(crate) struct BrokerVmGrant {
    /// Parent checkpoint and management generation before creation dispatch.
    watch: BrokerWatch,
    /// One preassigned VM UUID from the persisted broker reservations.
    vm: uuid::Uuid,
}
/// Require a private parent grant for inserting reserved broker VMs.
pub(super) fn ensure_vm_unreserved(
    conn: &Connection,
    id: uuid::Uuid,
) -> Result<(), PersistenceError> {
    let reserved: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM database_managed_rabbitmq c,json_each(c.spec,'$.brokers') b WHERE json_extract(b.value,'$.id')=?1)", [id.to_string()], |r|r.get(0))?;
    if reserved {
        return Err(PersistenceError::IacParentManaged);
    }
    Ok(())
}
/// Verify the original owner, epoch and exact controller checkpoint.
fn current(conn: &Connection, watch: &BrokerWatch) -> Result<(), PersistenceError> {
    let cluster = managed(conn)?.ok_or(PersistenceError::StaleIacOwner)?;
    if !cluster.enabled
        || cluster.id != watch.id
        || epoch(conn)? != watch.authority.epoch
        || iac_owners::read(conn, &resource())? != watch.authority.owner
        || fingerprint(&configuration(conn)?)? != watch.authority.input
        || fingerprint(&Some(cluster))? != watch.authority.managed
    {
        return Err(PersistenceError::StaleIacOwner);
    }
    Ok(())
}
/// Apply native-operation, parent and ownership gates before a public edit.
fn allowed(conn: &Connection) -> Result<(), PersistenceError> {
    if !iac_owners::public_write_allowed(conn, &resource())? {
        return Err(PersistenceError::IacOwnershipConflict);
    }
    Ok(())
}
impl Store {
    /// Bind one empty broker reservation to the controller's current private authority.
    pub(crate) fn database_broker_vm_grant(
        &self,
        watch: &BrokerWatch,
        vm: uuid::Uuid,
    ) -> Result<BrokerVmGrant, PersistenceError> {
        let conn = self.lock();
        current(&conn, watch)?;
        iac_native::ensure_idle(&conn, &resource())?;
        let cluster = managed(&conn)?.ok_or(PersistenceError::StaleIacOwner)?;
        if !cluster
            .brokers
            .iter()
            .any(|b| b.id == vm && b.ipv4.is_empty())
        {
            return Err(PersistenceError::StaleIacOwner);
        }
        Ok(BrokerVmGrant {
            watch: watch.clone(),
            vm,
        })
    }
    /// Recheck parent authority in the transaction inserting the reserved VM.
    pub(crate) fn insert_database_broker_vm(
        &self,
        vm: &crate::model::VmRecord,
        grant: &BrokerVmGrant,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, &grant.watch)?;
        iac_native::ensure_idle(&tx, &resource())?;
        if vm.id != grant.vm || vm.purpose != crate::model::VmPurpose::Instance {
            return Err(PersistenceError::StaleIacOwner);
        }
        let target = super::iac_vm_deletes::resource(vm.id);
        if iac_owners::read(&tx, &target)?.owner.is_some() {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        super::iac_vm_deletes::ensure_available(&tx, vm.id)?;
        super::execute_record(&tx, super::INSERT_SQL, vm)?;
        tx.commit()?;
        Ok(())
    }
    /// Capture the owner and settings before external connection verification.
    pub(crate) fn database_broker_authority(&self) -> Result<BrokerAuthority, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        allowed(&tx)?;
        let authority = BrokerAuthority {
            owner: iac_owners::read(&tx, &resource())?,
            input: fingerprint(&configuration(&tx)?)?,
            managed: fingerprint(&managed(&tx)?)?,
            epoch: epoch(&tx)?,
        };
        tx.commit()?;
        Ok(authority)
    }
    /// External selection and stopping managed supervision commit together.
    #[cfg(test)]
    pub(crate) fn configure_external_database_broker(
        &self,
        input: &RabbitMQInput,
        expected: &BrokerAuthority,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        allowed(&tx)?;
        if iac_owners::read(&tx, &resource())? != expected.owner
            || fingerprint(&configuration(&tx)?)? != expected.input
            || fingerprint(&managed(&tx)?)? != expected.managed
            || epoch(&tx)? != expected.epoch
        {
            return Err(PersistenceError::StaleIacOwner);
        }
        let retained: bool =
            tx.query_row("SELECT EXISTS(SELECT 1 FROM database_triggers)", [], |r| {
                r.get(0)
            })?;
        if retained {
            return Err(PersistenceError::IacResourceBusy);
        }
        advance(&tx)?;
        if let Some(mut cluster) = managed(&tx)? {
            cluster.enabled = false;
            tx.execute(
                "UPDATE database_managed_rabbitmq SET spec=?1 WHERE id=1",
                [encode(&cluster)?],
            )?;
        }
        tx.execute("INSERT INTO database_rabbitmq VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec",[encode(input)?])?;
        tx.commit()?;
        Ok(())
    }
    /// Replace managed configuration under the original verified writer generation.
    pub(crate) fn configure_managed_database_broker(
        &self,
        cluster: &Cluster,
        expected: &BrokerAuthority,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        allowed(&tx)?;
        if iac_owners::read(&tx, &resource())? != expected.owner
            || fingerprint(&configuration(&tx)?)? != expected.input
            || fingerprint(&managed(&tx)?)? != expected.managed
            || epoch(&tx)? != expected.epoch
        {
            return Err(PersistenceError::StaleIacOwner);
        }
        let retained: bool =
            tx.query_row("SELECT EXISTS(SELECT 1 FROM database_triggers)", [], |r| {
                r.get(0)
            })?;
        if retained {
            return Err(PersistenceError::IacResourceBusy);
        }
        advance(&tx)?;
        tx.execute("INSERT INTO database_managed_rabbitmq VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec",[encode(cluster)?])?;
        tx.commit()?;
        Ok(())
    }
    #[cfg(test)]
    /// Seed private client settings in tests; production activation uses a witness.
    pub(super) fn save_database_broker_input(
        conn: &Connection,
        input: &RabbitMQInput,
    ) -> Result<(), PersistenceError> {
        if let Some(current) = configuration(conn)?
            && fingerprint(&current)? == fingerprint(input)?
        {
            return Ok(());
        }
        allowed(conn)?;
        advance(conn)?;
        conn.execute("INSERT INTO database_rabbitmq VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec",[encode(input)?])?;
        Ok(())
    }
    #[cfg(test)]
    /// Seed legacy test records without implicitly reenabling disabled supervision.
    pub(super) fn save_database_broker_observation(
        conn: &Connection,
        cluster: &Cluster,
    ) -> Result<(), PersistenceError> {
        let before = managed(conn)?;
        if managed_configuration(before.as_ref())? != managed_configuration(Some(cluster))? {
            iac_native::ensure_idle(conn, &resource())?;
        }
        if before
            .as_ref()
            .is_some_and(|old| old.id != cluster.id || (!old.enabled && cluster.enabled))
        {
            return Err(PersistenceError::StaleIacOwner);
        }
        conn.execute("INSERT INTO database_managed_rabbitmq VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec",[encode(cluster)?])?;
        Ok(())
    }

    /// Hold an independent OS file lock to exclude another API controller.
    pub(crate) fn database_broker_runtime_lock(&self) -> Result<std::fs::File, PersistenceError> {
        let conn = self.lock();
        let path = conn.path().ok_or(PersistenceError::InvalidStackResource)?;
        let path = std::fs::canonicalize(path)
            .map_err(|_| PersistenceError::IacResourceBusy)?
            .with_extension("database-broker.lock");
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options
            .open(path)
            .map_err(|_| PersistenceError::IacResourceBusy)?;
        if !file.metadata().is_ok_and(|m| m.is_file()) {
            return Err(PersistenceError::IacResourceBusy);
        }
        file.try_lock()
            .map_err(|_| PersistenceError::IacResourceBusy)?;
        Ok(file)
    }
    /// Capture the current generation without granting a new public writer.
    pub(crate) fn database_broker_watch(
        &self,
        cluster: &Cluster,
    ) -> Result<BrokerWatch, PersistenceError> {
        let authority = self.database_broker_authority()?;
        let watch = BrokerWatch {
            authority,
            id: cluster.id,
        };
        let conn = self.lock();
        current(&conn, &watch)?;
        if fingerprint(&Some(cluster))? != watch.authority.managed {
            return Err(PersistenceError::StaleIacOwner);
        }
        Ok(watch)
    }
    /// Allow health observation while fencing repair during reserved native work.
    pub(crate) fn check_database_broker_watch(
        &self,
        watch: &BrokerWatch,
        mutating: bool,
    ) -> Result<(), PersistenceError> {
        let conn = self.lock();
        current(&conn, watch)?;
        if mutating {
            iac_native::ensure_idle(&conn, &resource())?;
        }
        Ok(())
    }
    /// Commit a controller checkpoint and optional activation under original authority.
    pub(crate) fn save_database_broker_watch(
        &self,
        watch: &mut BrokerWatch,
        cluster: &Cluster,
        activate: bool,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, watch)?;
        if cluster.id != watch.id || !cluster.enabled {
            return Err(PersistenceError::StaleIacOwner);
        }
        let before = managed(&tx)?;
        if activate
            || managed_configuration(before.as_ref())? != managed_configuration(Some(cluster))?
        {
            iac_native::ensure_idle(&tx, &resource())?;
        }
        let next_managed = fingerprint(&Some(cluster))?;
        let next_input = if activate {
            let input = cluster
                .config
                .as_ref()
                .ok_or(PersistenceError::InvalidStackResource)?;
            tx.execute("INSERT INTO database_rabbitmq VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec", [encode(input)?])?;
            fingerprint(&Some(input))?
        } else {
            watch.authority.input.clone()
        };
        tx.execute(
            "UPDATE database_managed_rabbitmq SET spec=?1 WHERE id=1",
            [encode(cluster)?],
        )?;
        tx.commit()?;
        watch.authority.managed = next_managed;
        watch.authority.input = next_input;
        Ok(())
    }
}

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
    /// A private running journal grants only its own singleton-fence bypass.
    operation: Option<firecrab_stack::NativeOperation>,
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
pub(super) fn current(conn: &Connection, watch: &BrokerWatch) -> Result<(), PersistenceError> {
    let cluster = managed(conn)?.ok_or(PersistenceError::StaleIacOwner)?;
    if (!cluster.enabled
        && watch
            .operation
            .as_ref()
            .is_none_or(|op| op.action != firecrab_stack::NativeAction::DisconnectDatabaseBroker))
        || cluster.id != watch.id
        || epoch(conn)? != watch.authority.epoch
        || iac_owners::read(conn, &resource())? != watch.authority.owner
        || fingerprint(&configuration(conn)?)? != watch.authority.input
        || fingerprint(&Some(cluster))? != watch.authority.managed
    {
        return Err(PersistenceError::StaleIacOwner);
    }
    if let Some(operation) = &watch.operation {
        super::iac_managed_database_brokers::validate_operation(conn, operation)?;
    }
    Ok(())
}
/// Only the current private native journal may bypass its own root reservation.
fn runtime_allowed(conn: &Connection, watch: &BrokerWatch) -> Result<(), PersistenceError> {
    if let Some(operation) = &watch.operation {
        super::iac_managed_database_brokers::validate_operation(conn, operation)
    } else {
        iac_native::ensure_idle(conn, &resource())
    }
}
/// Capturing an observer does not grant a public configuration writer or a new manager.
fn runtime_owner(conn: &Connection) -> Result<ResourceOwnership, PersistenceError> {
    let owner = iac_owners::read(conn, &resource())?;
    if let Some(manager) = &owner.owner {
        if manager.manager == firecrab_stack::Manager::Terraform {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        super::stack_applies::ensure_idle(conn, manager.id)?;
        super::stack_sources::ensure_idle(conn, manager.id)?;
    }
    Ok(owner)
}
/// Same-configuration repair may restore transport for an interrupted event reader.
/// It cannot replace credentials, addresses, profile, image digest or owned identities.
pub(super) fn repair_allowed(
    conn: &Connection,
    watch: &BrokerWatch,
) -> Result<(), PersistenceError> {
    current(conn, watch)?;
    runtime_owner(conn)?;
    let direct:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM iac_native_operations WHERE kind='database_rabbitmq' AND resource_id='default' AND state IN('running','needs_recovery'))",[],|r|r.get(0))?;
    if direct {
        return Err(PersistenceError::IacResourceBusy);
    }
    let mut stmt=conn.prepare("SELECT o.kind,json_extract(o.status,'$.action') FROM iac_native_dependencies d JOIN iac_native_operations o ON o.operation_id=d.operation_id WHERE d.kind='database_rabbitmq' AND d.resource_id='default' AND o.state IN('running','needs_recovery')")?;
    let readers = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    if readers.is_empty() {
        return Ok(());
    }
    if readers.iter().any(|(kind, action)| {
        kind != "database_events"
            || !matches!(
                action.as_str(),
                "create_database_events" | "delete_database_events"
            )
    }) {
        return Err(PersistenceError::IacResourceBusy);
    }
    let cluster = managed(conn)?.ok_or(PersistenceError::IacResourceBusy)?;
    if cluster.config.is_none()
        || !cluster.image.contains("@sha256:")
        || cluster.brokers.iter().any(|b| b.ipv4.is_empty())
        || fingerprint(&cluster.config)? != fingerprint(&configuration(conn)?)?
    {
        return Err(PersistenceError::IacResourceBusy);
    }
    Ok(())
}
/// Ownership identifies the one host slot; a managed cluster UUID is never a second alias.
pub(super) fn validate_owner(
    conn: &Connection,
    target: &ManagedResource,
) -> Result<(), PersistenceError> {
    if *target != resource() {
        return Err(PersistenceError::InvalidStackResource);
    }
    iac_native::ensure_idle(conn, target)?;
    let input = configuration(conn)?;
    let cluster = managed(conn)?;
    if input.is_none() && cluster.is_none() {
        return Err(PersistenceError::InvalidStackResource);
    }
    if let Some(input) = input {
        input
            .validate()
            .map_err(|_| PersistenceError::InvalidStackResource)?;
    }
    if let Some(cluster) = cluster {
        if cluster.id.is_nil()
            || matches!(
                cluster.phase.as_str(),
                "queued" | "importing" | "starting" | "stopping"
            )
        {
            return Err(PersistenceError::IacResourceBusy);
        }
        let mut ids = std::collections::HashSet::new();
        for broker in &cluster.brokers {
            if broker.id.is_nil() || !ids.insert(broker.id) {
                return Err(PersistenceError::InvalidStackResource);
            }
            let state: Option<String> = conn
                .query_row(
                    "SELECT state FROM vms WHERE id=?1",
                    [broker.id.to_string()],
                    |r| r.get(0),
                )
                .optional()?;
            if state
                .as_deref()
                .is_some_and(|s| matches!(s, "starting" | "stopping"))
            {
                return Err(PersistenceError::IacResourceBusy);
            }
            let vm = ManagedResource {
                kind: "vm".into(),
                id: broker.id.to_string(),
            };
            if iac_owners::read(conn, &vm)?.owner.is_some() {
                return Err(PersistenceError::IacOwnershipConflict);
            }
        }
    }
    Ok(())
}
/// Apply native-operation, parent and ownership gates before a public edit.
#[cfg(test)]
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
        runtime_allowed(&conn, watch)?;
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
        runtime_allowed(&tx, &grant.watch)?;
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
    #[cfg(test)]
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
    #[cfg(test)]
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
        let conn = self.lock();
        let authority = BrokerAuthority {
            owner: runtime_owner(&conn)?,
            input: fingerprint(&configuration(&conn)?)?,
            managed: fingerprint(&managed(&conn)?)?,
            epoch: epoch(&conn)?,
        };
        let watch = BrokerWatch {
            authority,
            id: cluster.id,
            operation: None,
        };
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
            runtime_allowed(&conn, watch)?;
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
        for broker in &cluster.brokers {
            if let Some(generation) = broker.disk_generation {
                let actual: Option<String> = tx
                    .query_row(
                        "SELECT disk_generation FROM vms WHERE id=?1",
                        [broker.id.to_string()],
                        |r| r.get(0),
                    )
                    .optional()?
                    .flatten();
                if actual.as_deref() != Some(generation.to_string().as_str()) {
                    return Err(PersistenceError::StaleIacOwner);
                }
            }
        }
        if activate
            || managed_configuration(before.as_ref())? != managed_configuration(Some(cluster))?
        {
            runtime_allowed(&tx, watch)?;
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
        if let Some(operation) = &watch.operation {
            super::iac_managed_database_brokers::checkpoint(&tx, operation, cluster, activate)?;
        }
        tx.commit()?;
        watch.authority.managed = next_managed;
        watch.authority.input = next_input;
        Ok(())
    }
    /// Construct managed execution authority only after the opaque private intent is validated.
    pub(crate) fn native_database_broker_watch(
        &self,
        intent: &super::iac_managed_database_brokers::ManagedBrokerIntent,
    ) -> Result<BrokerWatch, PersistenceError> {
        let conn = self.lock();
        super::iac_managed_database_brokers::admitted(&conn, intent)?;
        let cluster = managed(&conn)?.ok_or(PersistenceError::InvalidStackResource)?;
        Ok(BrokerWatch {
            authority: BrokerAuthority {
                owner: intent.operation.ownership.clone(),
                input: fingerprint(&configuration(&conn)?)?,
                managed: fingerprint(&Some(&cluster))?,
                epoch: epoch(&conn)?,
            },
            id: cluster.id,
            operation: Some(intent.operation.clone()),
        })
    }
}

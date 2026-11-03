//! Managed broker work owns the singleton before starting or stopping any guest.
use super::{
    PersistenceError, Store, database_broker_mutations as broker,
    database_services::{decode, encode},
    iac_native, iac_owners,
};
use crate::pipe_databases::managed_rabbitmq::{Broker, Cluster, default_image};
use firecrab_stack::{NativeAction, NativeOperation, NativeOperationState};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

mod admission;
mod progress;
pub(super) use progress::{admitted, checkpoint};

/// Private setup, retained credentials, progress and original outcome.
pub(super) const SCHEMA:&str="CREATE TABLE IF NOT EXISTS iac_managed_database_broker_mutations (
 operation_id TEXT PRIMARY KEY REFERENCES iac_native_operations(operation_id),request_hash TEXT NOT NULL,
 definition TEXT NOT NULL,prepared_cluster TEXT,effects_started INTEGER NOT NULL DEFAULT 0 CHECK(effects_started IN(0,1)),outcome TEXT
) STRICT;";

/// Only explicit lifecycle requests or the captured host controller construct these modes.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum ManagedBrokerCommand {
    /// Select the existing durable profile, or reserve new broker identities once.
    Prepare { development: bool },
    /// Repair one owned member or reconcile the same persisted cluster.
    Repair {
        cluster: Uuid,
        restart_vm: Option<Uuid>,
    },
    /// Disconnect configuration and stop retained managed guests without deleting disks.
    Disconnect,
}
impl ManagedBrokerCommand {
    /// Map the frozen lifecycle mode to its public action.
    fn action(&self) -> NativeAction {
        match self {
            Self::Prepare { .. } => NativeAction::PrepareDatabaseBroker,
            Self::Repair { .. } => NativeAction::RepairDatabaseBroker,
            Self::Disconnect => NativeAction::DisconnectDatabaseBroker,
        }
    }
}
/// Desired identities and the rollback boundary cannot change during recovery.
#[derive(Clone, Serialize, Deserialize)]
struct Definition {
    /// Original lifecycle request before current cluster lookup.
    command: ManagedBrokerCommand,
    /// Original client selection; only successful completion may replace it.
    before_input: String,
    /// Previous metadata restored only when no managed effect was attempted.
    before: Option<Cluster>,
    /// Complete reserved identity set and original private credentials.
    initial: Option<Cluster>,
    /// Explicit configuration generation assigned with admission.
    epoch: i64,
}
/// A public operation JSON object cannot construct this execution permission.
#[derive(Clone)]
pub(crate) struct ManagedBrokerIntent {
    /// Exact original owner and execution generation.
    pub operation: NativeOperation,
    /// Immutable raw command hash, checked before current settings are read.
    request_hash: String,
    /// Immutable private execution boundary.
    definition: Definition,
    /// Last retained progress loaded on admission or recovery.
    prepared: Option<Cluster>,
}
impl ManagedBrokerIntent {
    /// Resume the exact persisted private broker state.
    pub(crate) fn cluster(&self) -> Option<Cluster> {
        self.prepared.clone()
    }
    /// A repair never accepts an arbitrary guest outside its initial identity set.
    pub(crate) fn restart_vm(&self) -> Option<Uuid> {
        match self.definition.command {
            ManagedBrokerCommand::Repair { restart_vm, .. } => restart_vm,
            _ => None,
        }
    }
    /// Disconnect stops retained guests and never recreates a delivery transport.
    pub(crate) fn disconnecting(&self) -> bool {
        matches!(self.definition.command, ManagedBrokerCommand::Disconnect)
    }
    /// Only the original unconfigured identity set may create its first data disks.
    pub(crate) fn first_boot_allowed(&self) -> bool {
        self.definition
            .initial
            .as_ref()
            .is_some_and(|c| c.config.is_none())
    }
}
/// Separate immutable retries from invalid unreserved configuration.
pub(crate) enum Admission {
    /// First admission alone grants dispatch.
    Ready(Box<ManagedBrokerIntent>, bool),
    /// Static configuration error before managed work.
    Invalid(&'static str),
}
/// Read the private request without consulting mutable controller settings.
fn read(conn: &Connection, id: Uuid) -> Result<Option<ManagedBrokerIntent>, PersistenceError> {
    let Some(operation) = iac_native::read(conn, id)? else {
        return Ok(None);
    };
    if operation.ownership.resource != broker::resource()
        || !matches!(
            operation.action,
            NativeAction::PrepareDatabaseBroker
                | NativeAction::RepairDatabaseBroker
                | NativeAction::DisconnectDatabaseBroker
        )
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let row:Option<(String,String,Option<String>)>=conn.query_row("SELECT request_hash,definition,prepared_cluster FROM iac_managed_database_broker_mutations WHERE operation_id=?1",[id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let (request_hash, raw, prepared) = row.ok_or(PersistenceError::IacOperationIdentity)?;
    let definition: Definition = decode(&raw)?;
    if broker::fingerprint(&definition.command)? != request_hash
        || definition.command.action() != operation.action
        || u64::try_from(definition.epoch).ok() != operation.configuration_generation
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    Ok(Some(ManagedBrokerIntent {
        operation,
        request_hash,
        definition,
        prepared: prepared.as_deref().map(decode).transpose()?,
    }))
}
/// Current progress must remain bound to the original identities and generated credentials.
fn valid_progress(
    before: Option<&Cluster>,
    value: Option<&Cluster>,
    definition: &Definition,
) -> Result<(), PersistenceError> {
    match (definition.initial.as_ref(), value) {
        (None, None) => Ok(()),
        (Some(initial), Some(value)) => {
            if initial.id != value.id
                || initial.development != value.development
                || initial.brokers.iter().map(|b| b.id).collect::<Vec<_>>()
                    != value.brokers.iter().map(|b| b.id).collect::<Vec<_>>()
            {
                return Err(PersistenceError::IacOperationIdentity);
            }
            if let Some(before) = before {
                if before.image.contains('@') && before.image != value.image {
                    return Err(PersistenceError::IacOperationIdentity);
                }
                if before.config.is_some()
                    && broker::fingerprint(&before.config)? != broker::fingerprint(&value.config)?
                {
                    return Err(PersistenceError::IacOperationIdentity);
                }
                for (old, new) in before.brokers.iter().zip(&value.brokers) {
                    if !old.ipv4.is_empty() && old.ipv4 != new.ipv4 {
                        return Err(PersistenceError::IacOperationIdentity);
                    }
                    if old.disk_generation.is_some() && old.disk_generation != new.disk_generation {
                        return Err(PersistenceError::IacOperationIdentity);
                    }
                    if before.config.is_some() && (old.env != new.env || old.files != new.files) {
                        return Err(PersistenceError::IacOperationIdentity);
                    }
                }
            }
            Ok(())
        }
        _ => Err(PersistenceError::IacOperationIdentity),
    }
}
/// Validate the exact running attempt and immutable request inside the caller's transaction.
fn current(conn: &Connection, intent: &ManagedBrokerIntent) -> Result<(), PersistenceError> {
    if intent.operation.state != NativeOperationState::Running
        || iac_native::read(conn, intent.operation.operation_id)?.as_ref()
            != Some(&intent.operation)
        || iac_owners::read(conn, &broker::resource())? != intent.operation.ownership
        || broker::epoch(conn)? != intent.definition.epoch
        || broker::fingerprint(&broker::configuration(conn)?)? != intent.definition.before_input
    {
        return Err(PersistenceError::StaleIacOwner);
    }
    let original =
        read(conn, intent.operation.operation_id)?.ok_or(PersistenceError::IacOperationIdentity)?;
    if original.request_hash != intent.request_hash
        || broker::fingerprint(&original.definition)? != broker::fingerprint(&intent.definition)?
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let actual = broker::managed(conn)?;
    if broker::managed_configuration(actual.as_ref())?
        != broker::managed_configuration(original.prepared.as_ref())?
    {
        return Err(PersistenceError::StaleIacOwner);
    }
    valid_progress(
        original.prepared.as_ref(),
        actual.as_ref(),
        &intent.definition,
    )
}
/// Controller checkpoints may bypass their own native fence only through this exact private journal.
pub(super) fn validate_operation(
    conn: &Connection,
    operation: &NativeOperation,
) -> Result<(), PersistenceError> {
    let intent =
        read(conn, operation.operation_id)?.ok_or(PersistenceError::IacOperationIdentity)?;
    if intent.operation != *operation {
        return Err(PersistenceError::StaleIacOwner);
    }
    current(conn, &intent)
}

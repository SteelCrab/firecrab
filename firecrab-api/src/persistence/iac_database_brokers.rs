//! External broker verification retains an immutable request before opening a socket.
use super::{
    PersistenceError, Store, database_broker_mutations as broker,
    database_services::{decode, encode},
    iac_native, iac_owners,
};
use crate::pipe_databases::events::RabbitMQInput;
use firecrab_stack::{NativeAction, NativeOperation, NativeOperationState};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Private broker requests contain credentials; ordinary receipts never include them.
pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS iac_database_broker_mutations (
 operation_id TEXT PRIMARY KEY REFERENCES iac_native_operations(operation_id),
 request_hash TEXT NOT NULL, definition TEXT NOT NULL
) STRICT;";

/// Only verification is remote; selecting the client and disabling supervision commit locally.
#[derive(Clone, Serialize, Deserialize)]
struct Definition {
    /// Original client settings supplied before parent configuration is read.
    input: RabbitMQInput,
    /// Prior private client configuration, represented only by its hash.
    before_input: String,
    /// Prior managed desired configuration; health observations remain independent.
    before_managed: String,
    /// Explicit configuration generation reserved by this attempt.
    epoch: i64,
}
/// This private witness cannot be constructed from public operation metadata.
#[derive(Clone)]
pub(crate) struct BrokerIntent {
    /// Redacted original receipt and current execution generation.
    pub operation: NativeOperation,
    /// Immutable request fingerprint used before current settings are loaded.
    request_hash: String,
    /// Private original transport and expected configuration boundary.
    definition: Definition,
}
impl BrokerIntent {
    /// Verify the original supplied transport after interruption or a lost reply.
    pub(crate) fn input(&self) -> &RabbitMQInput {
        &self.definition.input
    }
}
/// Resolve immutable retries separately from invalid, unreserved configuration.
pub(crate) enum Admission {
    /// The bool grants dispatch only on first admission.
    Ready(Box<BrokerIntent>, bool),
    /// Static validation error before native work or a connection attempt.
    Invalid(&'static str),
}
/// Load and validate the original private request without reading mutable broker settings.
fn read(conn: &Connection, id: Uuid) -> Result<Option<BrokerIntent>, PersistenceError> {
    let Some(operation) = iac_native::read(conn, id)? else {
        return Ok(None);
    };
    if operation.action != NativeAction::ConfigureDatabaseBroker
        || operation.ownership.resource != broker::resource()
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let row:Option<(String,String)>=conn.query_row("SELECT request_hash,definition FROM iac_database_broker_mutations WHERE operation_id=?1",[id.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let (request_hash, raw) = row.ok_or(PersistenceError::IacOperationIdentity)?;
    let definition: Definition = decode(&raw)?;
    if broker::fingerprint(&definition.input)? != request_hash
        || u64::try_from(definition.epoch).ok() != operation.configuration_generation
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    Ok(Some(BrokerIntent {
        operation,
        request_hash,
        definition,
    }))
}
/// Fence configuration changes while permitting phase/error health observations.
fn current(conn: &Connection, intent: &BrokerIntent) -> Result<(), PersistenceError> {
    if intent.operation.state != NativeOperationState::Running
        || iac_native::read(conn, intent.operation.operation_id)?.as_ref()
            != Some(&intent.operation)
        || iac_owners::read(conn, &broker::resource())? != intent.operation.ownership
        || broker::epoch(conn)? != intent.definition.epoch
        || broker::fingerprint(&broker::configuration(conn)?)? != intent.definition.before_input
        || broker::managed_configuration(broker::managed(conn)?.as_ref())?
            != intent.definition.before_managed
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
    Ok(())
}
impl Store {
    /// Reserve the singleton and original credentials before remote verification.
    pub(crate) fn begin_database_broker(
        &self,
        id: Uuid,
        input: RabbitMQInput,
    ) -> Result<Admission, PersistenceError> {
        if id.is_nil() {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let request_hash = broker::fingerprint(&input)?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(original) = read(&tx, id)? {
            if original.request_hash != request_hash {
                return Err(PersistenceError::IacOperationIdentity);
            }
            return Ok(Admission::Ready(Box::new(original), false));
        }
        match input.validate() {
            Ok(()) => (),
            Err(crate::pipe_databases::write::Error::Invalid(message)) => {
                return Ok(Admission::Invalid(message));
            }
            Err(_) => return Err(PersistenceError::IacOperationIdentity),
        }
        if !iac_owners::public_write_allowed(&tx, &broker::resource())? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        let retained: bool =
            tx.query_row("SELECT EXISTS(SELECT 1 FROM database_triggers)", [], |r| {
                r.get(0)
            })?;
        if retained {
            return Ok(Admission::Invalid(
                "Finish retained triggers before changing the RabbitMQ cluster",
            ));
        }
        let epoch = broker::epoch(&tx)?;
        let definition = Definition {
            input,
            before_input: broker::fingerprint(&broker::configuration(&tx)?)?,
            before_managed: broker::managed_configuration(broker::managed(&tx)?.as_ref())?,
            epoch,
        };
        let operation = NativeOperation {
            operation_id: id,
            ownership: iac_owners::read(&tx, &broker::resource())?,
            action: NativeAction::ConfigureDatabaseBroker,
            state: NativeOperationState::Running,
            error_code: None,
            execution_generation: 0,
            configuration_generation: Some(
                u64::try_from(epoch).map_err(|_| PersistenceError::IacOperationIdentity)?,
            ),
            retain_disks: None,
        };
        iac_native::save(&tx, &operation)?;
        tx.execute(
            "INSERT INTO iac_database_broker_mutations VALUES(?1,?2,?3)",
            rusqlite::params![id.to_string(), request_hash, encode(&definition)?],
        )?;
        tx.commit()?;
        Ok(Admission::Ready(
            Box::new(BrokerIntent {
                operation,
                request_hash,
                definition,
            }),
            true,
        ))
    }
    /// Recheck the same private attempt after a remote handshake and before committing selection.
    pub(crate) fn check_database_broker(
        &self,
        intent: &BrokerIntent,
    ) -> Result<(), PersistenceError> {
        current(&self.lock(), intent)
    }
    /// Select verified credentials, stop managed supervision and acknowledge atomically.
    pub(crate) fn finish_database_broker(
        &self,
        intent: &BrokerIntent,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        let retained: bool =
            tx.query_row("SELECT EXISTS(SELECT 1 FROM database_triggers)", [], |r| {
                r.get(0)
            })?;
        if retained {
            return Err(PersistenceError::IacResourceBusy);
        }
        if let Some(mut cluster) = broker::managed(&tx)? {
            cluster.enabled = false;
            tx.execute(
                "UPDATE database_managed_rabbitmq SET spec=?1 WHERE id=1",
                [encode(&cluster)?],
            )?;
        }
        tx.execute("INSERT INTO database_rabbitmq VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec",[encode(&intent.definition.input)?])?;
        broker::advance(&tx)?;
        let mut operation = intent.operation.clone();
        operation.state = NativeOperationState::Succeeded;
        operation.error_code = None;
        iac_native::save(&tx, &operation)?;
        tx.commit()?;
        Ok(())
    }
    /// A handshake declares no queues; failure leaves all original configuration untouched.
    pub(crate) fn fail_database_broker(
        &self,
        intent: &BrokerIntent,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        let mut operation = intent.operation.clone();
        operation.state = NativeOperationState::Failed;
        operation.error_code = Some("database_broker_unverified".into());
        iac_native::save(&tx, &operation)?;
        tx.commit()?;
        Ok(())
    }
    /// Resume the original verification; completed receipts never reconfigure newer settings.
    #[cfg(test)]
    pub(crate) fn recover_database_broker(
        &self,
        id: Uuid,
    ) -> Result<(BrokerIntent, bool), PersistenceError> {
        let generation = self
            .iac_native_operation(id)?
            .ok_or(PersistenceError::IacOperationIdentity)?
            .execution_generation;
        self.recover_database_broker_at(id, generation)
    }
    /// Compare the observed attempt inside the transaction before resuming its original work.
    pub(crate) fn recover_database_broker_at(
        &self,
        id: Uuid,
        expected_generation: u64,
    ) -> Result<(BrokerIntent, bool), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut intent = read(&tx, id)?.ok_or(PersistenceError::IacOperationIdentity)?;
        if matches!(
            intent.operation.state,
            NativeOperationState::Succeeded | NativeOperationState::Failed
        ) {
            return Ok((intent, false));
        }
        crate::persistence::iac_native::check_recovery_attempt(
            &intent.operation,
            expected_generation,
        )?;
        if intent.operation.state != NativeOperationState::NeedsRecovery {
            return Err(PersistenceError::IacResourceBusy);
        }
        intent.operation.execution_generation = intent
            .operation
            .execution_generation
            .checked_add(1)
            .filter(|v| *v <= i64::MAX as u64)
            .ok_or(PersistenceError::IacOperationIdentity)?;
        intent.operation.state = NativeOperationState::Running;
        intent.operation.error_code = None;
        iac_native::save(&tx, &intent.operation)?;
        current(&tx, &intent)?;
        tx.commit()?;
        Ok((intent, true))
    }
}

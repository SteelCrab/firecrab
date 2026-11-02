//! Progress remains bound to the original root operation across API restarts.
use super::*;

/// The controller already validated current state before changing rows in this transaction.
pub(in crate::persistence) fn checkpoint(
    conn: &Connection,
    operation: &NativeOperation,
    cluster: &Cluster,
    complete: bool,
) -> Result<(), PersistenceError> {
    let intent =
        read(conn, operation.operation_id)?.ok_or(PersistenceError::IacOperationIdentity)?;
    if intent.operation != *operation
        || operation.state != NativeOperationState::Running
        || intent.disconnecting()
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    valid_progress(intent.prepared.as_ref(), Some(cluster), &intent.definition)?;
    if !cluster.enabled {
        return Err(PersistenceError::IacOperationIdentity);
    }
    conn.execute("UPDATE iac_managed_database_broker_mutations SET prepared_cluster=?1 WHERE operation_id=?2",params![encode(cluster)?,operation.operation_id.to_string()])?;
    if complete {
        if cluster.phase != "ready" || cluster.config.is_none() {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let mut receipt = operation.clone();
        receipt.state = NativeOperationState::Succeeded;
        receipt.error_code = None;
        conn.execute(
            "UPDATE iac_managed_database_broker_mutations SET outcome=?1 WHERE operation_id=?2",
            params![
                encode(&cluster.summary())?,
                operation.operation_id.to_string()
            ],
        )?;
        iac_native::save(conn, &receipt)?;
    }
    Ok(())
}
/// Validate the original journal before constructing its private controller witness.
pub(in crate::persistence) fn admitted(
    conn: &Connection,
    intent: &ManagedBrokerIntent,
) -> Result<(), PersistenceError> {
    current(conn, intent)
}
impl Store {
    /// Mark uncertainty before image import or any owned VM start, stop or creation.
    pub(crate) fn managed_database_broker_effects(
        &self,
        intent: &ManagedBrokerIntent,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        tx.execute("UPDATE iac_managed_database_broker_mutations SET effects_started=1 WHERE operation_id=?1",[intent.operation.operation_id.to_string()])?;
        tx.commit()?;
        Ok(())
    }
    /// Preserve managed work after uncertain effects; only a no-effect attempt restores old metadata.
    pub(crate) fn fail_managed_database_broker(
        &self,
        intent: &ManagedBrokerIntent,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        let effects:bool=tx.query_row("SELECT effects_started FROM iac_managed_database_broker_mutations WHERE operation_id=?1",[intent.operation.operation_id.to_string()],|r|r.get(0))?;
        let mut operation = intent.operation.clone();
        if effects {
            if let Some(mut cluster) = broker::managed(&tx)? {
                cluster.phase = "failed".into();
                cluster.error=Some("Recover the original broker operation; owned disks and credentials are retained".into());
                tx.execute(
                    "UPDATE database_managed_rabbitmq SET spec=?1 WHERE id=1",
                    [encode(&cluster)?],
                )?;
                tx.execute("UPDATE iac_managed_database_broker_mutations SET prepared_cluster=?1 WHERE operation_id=?2",params![encode(&cluster)?,operation.operation_id.to_string()])?;
            }
            operation.state = NativeOperationState::NeedsRecovery;
            operation.error_code = Some("database_broker_incomplete".into());
        } else {
            if let Some(before) = &intent.definition.before {
                tx.execute("INSERT INTO database_managed_rabbitmq VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec",[encode(before)?])?;
            } else {
                tx.execute("DELETE FROM database_managed_rabbitmq WHERE id=1", [])?;
            }
            broker::advance(&tx)?;
            operation.state = NativeOperationState::Failed;
            operation.error_code = Some("database_broker_unstarted".into());
        }
        iac_native::save(&tx, &operation)?;
        tx.commit()?;
        Ok(())
    }
    /// Stop completion releases client selection but retains original guest/disk ownership records.
    pub(crate) fn finish_database_broker_disconnect(
        &self,
        intent: &ManagedBrokerIntent,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        if !intent.disconnecting() {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let mut retained = Vec::new();
        if let Some(mut cluster) = broker::managed(&tx)? {
            for broker in &cluster.brokers {
                let state: Option<String> = tx
                    .query_row(
                        "SELECT state FROM vms WHERE id=?1",
                        [broker.id.to_string()],
                        |r| r.get(0),
                    )
                    .optional()?;
                if state.is_none() && !broker.ipv4.is_empty() {
                    return Err(PersistenceError::InvalidStackResource);
                }
                if state
                    .as_deref()
                    .is_some_and(|s| !matches!(s, "created" | "stopped" | "error"))
                {
                    return Err(PersistenceError::IacResourceBusy);
                }
                retained.push(broker.id);
            }
            cluster.enabled = false;
            cluster.phase = "stopped".into();
            cluster.error = None;
            tx.execute(
                "UPDATE database_managed_rabbitmq SET spec=?1 WHERE id=1",
                [encode(&cluster)?],
            )?;
            tx.execute("UPDATE iac_managed_database_broker_mutations SET prepared_cluster=?1 WHERE operation_id=?2",params![encode(&cluster)?,intent.operation.operation_id.to_string()])?;
        }
        tx.execute("DELETE FROM database_rabbitmq WHERE id=1", [])?;
        let outcome = serde_json::json!({"configured":false,"retainedBrokerIds":retained});
        tx.execute(
            "UPDATE iac_managed_database_broker_mutations SET outcome=?1 WHERE operation_id=?2",
            params![encode(&outcome)?, intent.operation.operation_id.to_string()],
        )?;
        let mut operation = intent.operation.clone();
        operation.state = NativeOperationState::Succeeded;
        operation.error_code = None;
        iac_native::save(&tx, &operation)?;
        tx.commit()?;
        Ok(())
    }
    /// Original outcomes remain readable after later configuration or ownership changes.
    pub(crate) fn managed_database_broker_receipt(
        &self,
        id: Uuid,
    ) -> Result<Option<serde_json::Value>, PersistenceError> {
        let conn = self.lock();
        let raw: Option<String> = conn.query_row(
            "SELECT outcome FROM iac_managed_database_broker_mutations WHERE operation_id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?;
        raw.as_deref().map(decode).transpose()
    }
    /// Recovery advances only the original retained identities and generated TLS configuration.
    pub(crate) fn recover_managed_database_broker(
        &self,
        id: Uuid,
    ) -> Result<(ManagedBrokerIntent, bool), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut intent = read(&tx, id)?.ok_or(PersistenceError::IacOperationIdentity)?;
        if matches!(
            intent.operation.state,
            NativeOperationState::Succeeded | NativeOperationState::Failed
        ) {
            return Ok((intent, false));
        }
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

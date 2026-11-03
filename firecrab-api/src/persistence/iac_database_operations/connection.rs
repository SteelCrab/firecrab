use super::*;
use crate::persistence::Store;
use rusqlite::TransactionBehavior;

impl Store {
    #[cfg(test)]
    pub(crate) fn begin_database_connection(
        &self,
        existing: Option<Uuid>,
        id: Uuid,
        raw: DatabaseInput,
    ) -> Result<Admission, PersistenceError> {
        self.begin_database_change(
            id,
            match existing {
                Some(resource) => DatabaseChange::Update(resource, raw),
                None => DatabaseChange::Create(raw),
            },
        )
    }

    /// Exact retries precede mutable settings, access-policy merging and inherited trust.
    pub(crate) fn begin_database_change(
        &self,
        id: Uuid,
        change: DatabaseChange,
    ) -> Result<Admission, PersistenceError> {
        if id.is_nil() {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let request_hash = hash(&change)?;
        let existing = match &change {
            DatabaseChange::Create(_) => None,
            DatabaseChange::Update(id, _) | DatabaseChange::Access(id, _) => Some(*id),
        };
        let action = if existing.is_some() {
            NativeAction::UpdateDatabaseConnection
        } else {
            NativeAction::CreateDatabaseConnection
        };
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(previous) = read(&tx, id)? {
            if previous.request_hash != request_hash
                || previous.operation.action != action
                || existing.is_some_and(|value| value != previous.id())
            {
                return Err(PersistenceError::IacOperationIdentity);
            }
            return Ok(Admission::Ready(Box::new(previous), false));
        }
        let connection = existing.unwrap_or_else(Uuid::new_v4);
        let before = record(&tx, connection)?;
        if existing.is_some() && before.is_none() {
            return Ok(Admission::Missing);
        }
        if existing.is_none()
            && (before.is_some() || iac_owners::read(&tx, &resource(connection))?.generation != 0)
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        if !iac_owners::public_write_allowed(&tx, &resource(connection))? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        let verify_connection = !matches!(change, DatabaseChange::Access(_, false));
        let mut input = match change {
            DatabaseChange::Create(input) | DatabaseChange::Update(_, input) => input,
            DatabaseChange::Access(_, enabled) => {
                let mut input = before
                    .as_ref()
                    .ok_or(PersistenceError::IacOperationIdentity)?
                    .input
                    .clone();
                input.write_enabled = enabled;
                input
            }
        };
        if let Some(before) = &before {
            if input
                .ca_certificate
                .as_ref()
                .is_none_or(|v| v.trim().is_empty())
            {
                input.ca_certificate = before.input.ca_certificate.clone();
            }
            if let Err(message) = validate_update(&tx, before, &input)? {
                return Ok(Admission::Invalid(message));
            }
        }
        if let Err(message) = input.settings(connection) {
            return Ok(Admission::Invalid(message));
        }
        let mut targets = std::collections::BTreeSet::new();
        if let Some(before) = &before {
            targets.insert(
                crate::pipe_databases::maintenance::target_hash(before)
                    .map_err(|_| PersistenceError::IacOperationIdentity)?,
            );
        }
        targets.insert(
            crate::pipe_databases::maintenance::target_hash(&DatabaseRecord {
                id: connection,
                input: input.clone(),
            })
            .map_err(|_| PersistenceError::IacOperationIdentity)?,
        );
        for target in &targets {
            crate::persistence::iac_database_targets::idle(&tx, target, None, false)?;
        }
        let payload = Payload {
            verify_connection,
            input,
            before_hash: before.as_ref().map(|r| hash(&r.input)).transpose()?,
            dependencies_hash: dependencies(&tx, connection)?,
        };
        let operation = NativeOperation {
            operation_id: id,
            ownership: iac_owners::read(&tx, &resource(connection))?,
            action,
            state: NativeOperationState::Running,
            error_code: None,
            execution_generation: 0,
            configuration_generation: None,
            retain_disks: None,
        };
        iac_native::save(&tx, &operation)?;
        tx.execute("INSERT INTO iac_database_mutations(operation_id,request_hash,payload) VALUES (?1,?2,?3)",params![id.to_string(),request_hash,serde_json::to_string(&payload).map_err(|_|PersistenceError::IacOperationIdentity)?])?;
        for target in targets {
            tx.execute(
                "INSERT INTO iac_database_targets VALUES (?1,?2)",
                params![target, id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(Admission::Ready(
            Box::new(DatabaseIntent {
                operation,
                resource_id: connection,
                request_hash,
                payload,
            }),
            true,
        ))
    }

    pub(crate) fn begin_database_connection_effects(
        &self,
        intent: &DatabaseIntent,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        tx.execute(
            "UPDATE iac_database_mutations SET effects_started=1 WHERE operation_id=?1",
            [intent.operation.operation_id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn finish_database_connection(
        &self,
        intent: &DatabaseIntent,
    ) -> Result<NativeOperation, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        let spec = serde_json::to_string(intent.input())
            .map_err(|_| PersistenceError::IacOperationIdentity)?;
        if intent.payload.before_hash.is_some() {
            if tx.execute(
                "UPDATE pipe_databases SET spec=?2 WHERE id=?1",
                params![intent.id().to_string(), spec],
            )? != 1
            {
                return Err(PersistenceError::IacOperationIdentity);
            }
        } else {
            tx.execute(
                "INSERT INTO pipe_databases VALUES (?1,?2)",
                params![intent.id().to_string(), spec],
            )?;
        }
        let mut operation = intent.operation.clone();
        operation.state = NativeOperationState::Succeeded;
        iac_native::save(&tx, &operation)?;
        tx.execute(
            "DELETE FROM iac_database_targets WHERE operation_id=?1",
            [operation.operation_id.to_string()],
        )?;
        tx.commit()?;
        Ok(operation)
    }

    /// Only an attempt that never crossed its durable effect checkpoint can release its fence.
    pub(crate) fn fail_database_connection(
        &self,
        intent: &DatabaseIntent,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if intent.operation.state != NativeOperationState::Running
            || iac_native::read(&tx, intent.operation.operation_id)?.as_ref()
                != Some(&intent.operation)
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let effects: bool = tx.query_row(
            "SELECT effects_started FROM iac_database_mutations WHERE operation_id=?1",
            [intent.operation.operation_id.to_string()],
            |r| r.get(0),
        )?;
        let mut operation = intent.operation.clone();
        operation.state = if effects {
            NativeOperationState::NeedsRecovery
        } else {
            NativeOperationState::Failed
        };
        operation.error_code = Some(
            if effects {
                "database_update_incomplete"
            } else {
                "database_connection_unverified"
            }
            .into(),
        );
        iac_native::save(&tx, &operation)?;
        if !effects {
            tx.execute(
                "DELETE FROM iac_database_targets WHERE operation_id=?1",
                [operation.operation_id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn recover_database_connection(
        &self,
        id: Uuid,
    ) -> Result<(DatabaseIntent, bool), PersistenceError> {
        let generation = self
            .iac_native_operation(id)?
            .ok_or(PersistenceError::IacOperationIdentity)?
            .execution_generation;
        self.recover_database_connection_at(id, generation)
    }
    /// Compare the observed attempt inside the transaction before resuming its original work.
    pub(crate) fn recover_database_connection_at(
        &self,
        id: Uuid,
        expected_generation: u64,
    ) -> Result<(DatabaseIntent, bool), PersistenceError> {
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
        if iac_owners::read(&tx, &resource(intent.id()))? != intent.operation.ownership {
            return Err(PersistenceError::StaleIacOwner);
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
        tx.commit()?;
        Ok((intent, true))
    }
}

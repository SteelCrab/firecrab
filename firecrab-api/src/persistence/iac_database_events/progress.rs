//! Attempt-fenced event progress and original-outcome receipts.
use super::*;

impl Store {
    /// Fence the attempt and record uncertainty before remote mutation.
    pub(crate) fn checkpoint_database_events(
        &self,
        intent: &EventIntent,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        tx.execute(
            "UPDATE iac_database_event_mutations SET effects_started=1 WHERE operation_id=?1",
            [intent.operation.operation_id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Stop new delivery before any DDL or broker work; keep that stage through recovery.
    pub(crate) fn prepare_database_events(
        &self,
        intent: &EventIntent,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        let mut value = intent.trigger().clone();
        value.paused = true;
        write_trigger(&tx, &value)?;
        tx.execute(
            "UPDATE iac_database_event_mutations SET prepared_trigger=?2 WHERE operation_id=?1",
            params![intent.operation.operation_id.to_string(), encode(&value)?],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Read the current attempt's retained private account.
    pub(crate) fn event_binding(
        &self,
        intent: &EventIntent,
    ) -> Result<Option<BindingRecord>, PersistenceError> {
        let conn = self.lock();
        current(&conn, intent)?;
        staged_binding(&conn, intent)
    }
    /// Persist credentials once; later writes change only inbox/readiness observations.
    pub(crate) fn prepare_event_binding(
        &self,
        intent: &EventIntent,
        value: &BindingRecord,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        let remote_started: bool = tx.query_row(
            "SELECT effects_started FROM iac_database_event_mutations WHERE operation_id=?1",
            [intent.operation.operation_id.to_string()],
            |r| r.get(0),
        )?;
        if !remote_started {
            return Err(PersistenceError::IacOperationIdentity);
        }
        if !intent.changes_binding()
            || intent.binding_id() != Some(value.id)
            || value.database_id != intent.trigger().database_id
            || value.input.service_id != intent.trigger().input.service_id
            || value.input.env_name != "DATABASE_URL"
            || !value.event_inbox
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        if let Some(before) = staged_binding(&tx, intent)? {
            let mut expected = before.clone();
            expected.event_inbox = true;
            expected.ready = value.ready;
            if hash(&expected)? != hash(value)? {
                return Err(PersistenceError::IacOperationIdentity);
            }
        }
        super::super::iac_database_bindings::write(&tx, value)?;
        tx.execute("UPDATE iac_database_event_mutations SET prepared_binding=?2,effects_started=1 WHERE operation_id=?1",params![intent.operation.operation_id.to_string(),encode(value)?])?;
        tx.commit()?;
        Ok(())
    }
    /// Commit capture outcome and success receipt with target-lease release.
    pub(crate) fn finish_database_events(
        &self,
        intent: &EventIntent,
        outcome: Option<&Trigger>,
    ) -> Result<NativeOperation, PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        match (&intent.definition.work, outcome) {
            (Work::Delete(_), None) => {
                tx.execute(
                    "DELETE FROM database_triggers WHERE id=?1",
                    [intent.trigger().id.to_string()],
                )?;
            }
            (work, Some(value)) => {
                if value.id != intent.trigger().id
                    || value.database_id != intent.trigger().database_id
                    || hash(&value.input)? != hash(&intent.trigger().input)?
                {
                    return Err(PersistenceError::IacOperationIdentity);
                }
                let valid = match work {
                    Work::Create(_) => {
                        if intent.creation_noop() {
                            hash(value)? == hash(intent.trigger())?
                        } else {
                            value.installed && !value.detached && !value.paused
                        }
                    }
                    Work::Pause(paused) => {
                        value.paused == *paused
                            && value.installed == intent.trigger().installed
                            && value.detached == intent.trigger().detached
                    }
                    Work::Detach => value.detached && !value.paused,
                    Work::Delete(_) => false,
                };
                if !valid {
                    return Err(PersistenceError::IacOperationIdentity);
                }
                write_trigger(&tx, value)?;
            }
            _ => return Err(PersistenceError::IacOperationIdentity),
        }
        let mut operation = intent.operation.clone();
        operation.state = NativeOperationState::Succeeded;
        iac_native::save(&tx, &operation)?;
        tx.execute(
            "UPDATE iac_database_event_mutations SET outcome=?2 WHERE operation_id=?1",
            params![
                operation.operation_id.to_string(),
                outcome.map(encode).transpose()?
            ],
        )?;
        tx.execute(
            "DELETE FROM iac_database_targets WHERE operation_id=?1",
            [operation.operation_id.to_string()],
        )?;
        tx.commit()?;
        Ok(operation)
    }
    /// Restore no-effect preparation or retain uncertainty for original recovery.
    pub(crate) fn fail_database_events(
        &self,
        intent: &EventIntent,
    ) -> Result<(), PersistenceError> {
        self.fail_database_events_checked(intent, false)
    }
    /// Restore delivery when retained events prevent removal before broker changes.
    pub(crate) fn database_events_not_drained(
        &self,
        intent: &EventIntent,
    ) -> Result<(), PersistenceError> {
        self.fail_database_events_checked(intent, true)
    }
    /// Classify failure and restore local staging in one fenced transaction.
    fn fail_database_events_checked(
        &self,
        intent: &EventIntent,
        not_drained: bool,
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
            "SELECT effects_started FROM iac_database_event_mutations WHERE operation_id=?1",
            [intent.operation.operation_id.to_string()],
            |r| r.get(0),
        )?;
        if not_drained && (effects || intent.operation.action != NativeAction::DeleteDatabaseEvents)
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        if !effects {
            current(&tx, intent)?;
            if let Some(before) = &intent.definition.before {
                write_trigger(&tx, before)?;
            } else {
                tx.execute(
                    "DELETE FROM database_triggers WHERE id=?1",
                    [intent.trigger().id.to_string()],
                )?;
            }
        }
        let mut operation = intent.operation.clone();
        operation.state = if effects {
            NativeOperationState::NeedsRecovery
        } else {
            NativeOperationState::Failed
        };
        operation.error_code = Some(
            if not_drained {
                "database_events_not_drained"
            } else if effects {
                "database_events_incomplete"
            } else {
                "database_events_unverified"
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
    /// Advance the original uncertain attempt without replacing its resource or account.
    #[cfg(test)]
    pub(crate) fn recover_database_events(
        &self,
        id: Uuid,
    ) -> Result<(EventIntent, bool), PersistenceError> {
        let generation = self
            .iac_native_operation(id)?
            .ok_or(PersistenceError::IacOperationIdentity)?
            .execution_generation;
        self.recover_database_events_at(id, generation)
    }
    /// Compare the observed attempt inside the transaction before resuming its original work.
    pub(crate) fn recover_database_events_at(
        &self,
        id: Uuid,
        expected_generation: u64,
    ) -> Result<(EventIntent, bool), PersistenceError> {
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
    /// Return the original saved outcome after later changes or removal.
    pub(crate) fn database_events_receipt(
        &self,
        intent: &EventIntent,
    ) -> Result<Option<Trigger>, PersistenceError> {
        let conn = self.lock();
        let raw: Option<String> = conn.query_row(
            "SELECT outcome FROM iac_database_event_mutations WHERE operation_id=?1",
            [intent.operation.operation_id.to_string()],
            |r| r.get(0),
        )?;
        raw.as_deref().map(decode).transpose()
    }
}

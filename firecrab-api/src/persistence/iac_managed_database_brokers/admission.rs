//! Reserve original guest identities and configuration generation before dispatch.
use super::*;
impl Store {
    /// Completed retries reach their original request before runtime locks or mutable preflight.
    pub(crate) fn managed_database_broker_retry(
        &self,
        id: Uuid,
        command: &ManagedBrokerCommand,
    ) -> Result<Option<ManagedBrokerIntent>, PersistenceError> {
        let conn = self.lock();
        let Some(intent) = read(&conn, id)? else {
            return Ok(None);
        };
        if intent.request_hash != broker::fingerprint(command)? {
            return Err(PersistenceError::IacOperationIdentity);
        }
        Ok(Some(intent))
    }
    /// Admission saves the full identity set, private request and native fence atomically.
    pub(crate) fn begin_managed_database_broker(
        &self,
        id: Uuid,
        command: ManagedBrokerCommand,
        watch: Option<&super::super::BrokerWatch>,
    ) -> Result<Admission, PersistenceError> {
        if id.is_nil() {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let request_hash = broker::fingerprint(&command)?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(intent) = read(&tx, id)? {
            if intent.request_hash != request_hash {
                return Err(PersistenceError::IacOperationIdentity);
            }
            return Ok(Admission::Ready(Box::new(intent), false));
        }
        if matches!(command, ManagedBrokerCommand::Repair { .. }) {
            broker::repair_allowed(&tx, watch.ok_or(PersistenceError::StaleIacOwner)?)?;
        } else if !iac_owners::public_write_allowed(&tx, &broker::resource())? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        let before = broker::managed(&tx)?;
        if !matches!(command, ManagedBrokerCommand::Repair { .. }) {
            let retained: bool =
                tx.query_row("SELECT EXISTS(SELECT 1 FROM database_triggers)", [], |r| {
                    r.get(0)
                })?;
            if retained {
                return Ok(Admission::Invalid(
                    "Finish retained triggers before replacing or disconnecting RabbitMQ",
                ));
            }
        }
        let mut prepared = match command {
            ManagedBrokerCommand::Prepare { development } => {
                let mut value = if let Some(before) = before.clone() {
                    if (before.enabled && before.phase != "failed")
                        || before.development != development
                    {
                        return Err(PersistenceError::IacResourceBusy);
                    }
                    before
                } else {
                    Cluster {
                        id: Uuid::new_v4(),
                        enabled: true,
                        image: default_image(),
                        development,
                        phase: "queued".into(),
                        error: None,
                        brokers: vec![],
                        config: None,
                    }
                };
                value.enabled = true;
                Some(value)
            }
            ManagedBrokerCommand::Repair {
                cluster,
                restart_vm,
            } => {
                let watch = watch.ok_or(PersistenceError::StaleIacOwner)?;
                broker::current(&tx, watch)?;
                let before = before
                    .clone()
                    .ok_or(PersistenceError::InvalidStackResource)?;
                if !before.enabled
                    || before.id != cluster
                    || restart_vm.is_some_and(|id| !before.brokers.iter().any(|b| b.id == id))
                {
                    return Err(PersistenceError::StaleIacOwner);
                }
                Some(before)
            }
            ManagedBrokerCommand::Disconnect => before.clone(),
        };
        if let Some(value) = &mut prepared {
            let count = if value.development { 1 } else { 3 };
            let mut ids = std::collections::HashSet::new();
            if value.brokers.len() > count
                || value
                    .brokers
                    .iter()
                    .any(|b| b.id.is_nil() || !ids.insert(b.id))
                || (value.config.is_some()
                    && (value.brokers.len() != count
                        || value.brokers.iter().any(|b| b.ipv4.is_empty())))
            {
                return Ok(Admission::Invalid(
                    "Restore the original RabbitMQ broker reservations before retrying",
                ));
            }
            while value.brokers.len() < count {
                value.brokers.push(Broker {
                    id: Uuid::new_v4(),
                    ipv4: String::new(),
                    disk_generation: None,
                    env: Default::default(),
                    files: Default::default(),
                });
            }
            value.phase = if matches!(command, ManagedBrokerCommand::Disconnect) {
                "stopping"
            } else {
                "queued"
            }
            .into();
            value.error = None;
        }
        broker::advance(&tx)?;
        let epoch = broker::epoch(&tx)?;
        let definition = Definition {
            command: command.clone(),
            before_input: broker::fingerprint(&broker::configuration(&tx)?)?,
            before,
            initial: prepared.clone(),
            epoch,
        };
        let operation = NativeOperation {
            operation_id: id,
            ownership: iac_owners::read(&tx, &broker::resource())?,
            action: command.action(),
            state: NativeOperationState::Running,
            error_code: None,
            execution_generation: 0,
            configuration_generation: Some(
                u64::try_from(epoch).map_err(|_| PersistenceError::IacOperationIdentity)?,
            ),
            retain_disks: None,
        };
        iac_native::save(&tx, &operation)?;
        tx.execute("INSERT INTO iac_managed_database_broker_mutations(operation_id,request_hash,definition,prepared_cluster) VALUES(?1,?2,?3,?4)",params![id.to_string(),request_hash,encode(&definition)?,prepared.as_ref().map(encode).transpose()?])?;
        if let Some(prepared) = &prepared {
            tx.execute("INSERT INTO database_managed_rabbitmq VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET spec=excluded.spec",[encode(prepared)?])?;
        }
        tx.commit()?;
        Ok(Admission::Ready(
            Box::new(ManagedBrokerIntent {
                operation,
                request_hash,
                definition,
                prepared,
            }),
            true,
        ))
    }
}
